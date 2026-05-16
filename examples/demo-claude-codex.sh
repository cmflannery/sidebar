#!/usr/bin/env bash
# demo-claude-codex.sh — real two-LLM conversation through sidebar.
#
# Codex stands by polling its inbox. Claude sends Codex a question.
# Codex replies. Claude reads the reply. `sidebar tail` captures every
# message in real time.
#
# Requirements:
#   - sidebar installed and on PATH (or set $SIDEBAR_BIN)
#   - `claude` CLI on PATH, with sidebar registered as an MCP
#     (`claude mcp add sidebar --scope user -- "$(which sidebar)" mcp --as claude-code`)
#   - `codex` CLI on PATH, with sidebar registered as an MCP
#     (`codex mcp add sidebar -- "$(which sidebar)" mcp --as codex`)
#
# The script does not touch ~/.sidebar — it expects the daemon to already
# be running (or starts a sandboxed one if not).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${SIDEBAR_BIN:-$(command -v sidebar || echo "$REPO_DIR/target/release/sidebar")}"

if [ ! -x "$BIN" ]; then
  echo "Building release binary at $BIN ..."
  (cd "$REPO_DIR" && cargo build --release)
fi

# Start a sandboxed daemon if there isn't one running.
DAEMON_STARTED=0
if ! "$BIN" status 2>/dev/null | grep -q "daemon:      running"; then
  SANDBOX="$(mktemp -d -t sidebar-demo.XXXXXX)"
  export SIDEBAR_HOME="$SANDBOX"
  echo "No daemon running; starting one in sandbox $SANDBOX"
  "$BIN" serve >"$SANDBOX/daemon.log" 2>&1 &
  until grep -q "daemon listening" "$SANDBOX/daemon.log" 2>/dev/null; do sleep 0.05; done
  DAEMON_STARTED=1
fi

cleanup() {
  if [ "$DAEMON_STARTED" = "1" ]; then
    pkill -INT -f "$BIN serve" 2>/dev/null || true
    sleep 0.3
    [ -n "${SANDBOX:-}" ] && rm -rf "$SANDBOX"
  fi
}
trap cleanup EXIT

# Start tail in the background to watch the whole conversation.
TAIL_LOG="$(mktemp -t sidebar-demo-tail.XXXXXX)"
"$BIN" tail >"$TAIL_LOG" 2>&1 &
TAIL_PID=$!
sleep 0.2

echo "=== launching codex (stands by, will reply when claude asks) ==="
codex exec --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
'You are an assistant named "codex" participating in a multi-agent group chat. Do these tools calls, in order:
1. Call mcp__sidebar__inbox with wait_ms=15000 (this blocks until a message arrives).
2. Look at the message body. If it is a math question, compute the answer.
3. Reply by calling mcp__sidebar__send with to="@claude-code" and body=<your one-sentence answer>.
4. Stop. Output the answer you sent in one line.' \
  >/tmp/codex-out.log 2>&1 &
CODEX_PID=$!

# Give codex a moment to enter its inbox long-poll.
sleep 1.0

echo "=== launching claude (sends the question, waits for reply) ==="
claude -p --permission-mode bypassPermissions \
'You are an assistant named "claude-code" participating in a multi-agent group chat. Do these tool calls, in order:
1. Call mcp__sidebar__send with to="@codex" and body="What is 2 + 2? Reply with just the number."
2. Call mcp__sidebar__inbox with wait_ms=15000 (this blocks until codex replies).
3. Output codex'"'"'s reply in one line.' \
  >/tmp/claude-out.log 2>&1 &
CLAUDE_PID=$!

# Wait for both to finish.
wait $CLAUDE_PID || true
wait $CODEX_PID  || true

# Stop tail after a beat so it can drain.
sleep 0.3
kill $TAIL_PID 2>/dev/null || true
wait $TAIL_PID 2>/dev/null || true

echo
echo "=== claude reported ==="
tail -3 /tmp/claude-out.log

echo
echo "=== codex reported ==="
tail -3 /tmp/codex-out.log

echo
echo "=== sidebar tail captured ==="
cat "$TAIL_LOG"

echo
echo "=== participants ==="
"$BIN" participants

rm -f /tmp/claude-out.log /tmp/codex-out.log "$TAIL_LOG"
