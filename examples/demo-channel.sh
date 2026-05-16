#!/usr/bin/env bash
# demo-channel.sh — multi-agent standup over a sidebar channel.
#
# Both `claude` and `codex` subscribe to #standup. Master broadcasts
# "what are you working on?" to the channel. Both agents reply on the
# channel. tail captures every line.
#
# Requirements: same as demo-claude-codex.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${SIDEBAR_BIN:-$(command -v sidebar || echo "$REPO_DIR/target/release/sidebar")}"

if [ ! -x "$BIN" ]; then
  echo "Building release binary at $BIN ..."
  (cd "$REPO_DIR" && cargo build --release)
fi

DAEMON_STARTED=0
if ! "$BIN" status 2>/dev/null | grep -q "daemon:      running"; then
  SANDBOX="$(mktemp -d -t sidebar-demo-channel.XXXXXX)"
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

# Master subscribes to #standup so master's inbox would also receive (not
# used here; we use tail instead, which sees every message regardless).
"$BIN" join standup >/dev/null

TAIL_LOG="$(mktemp -t sidebar-demo-channel-tail.XXXXXX)"
"$BIN" tail >"$TAIL_LOG" 2>&1 &
TAIL_PID=$!
sleep 0.2

echo "=== launching codex (joins #standup, replies once) ==="
codex exec --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox \
'You are an assistant named "codex" in a multi-agent standup. Do these tool calls in order:
1. Call mcp__sidebar__join with channel="standup".
2. Call mcp__sidebar__inbox with wait_ms=15000 to wait for the standup question.
3. Read the question. Reply by calling mcp__sidebar__send with to="#standup" and body="<your one-sentence answer about what you are working on as codex right now>".
4. Stop. Output the body you sent in one line.' \
  >/tmp/codex-out.log 2>&1 &
CODEX_PID=$!

sleep 1.0

echo "=== launching claude (joins #standup, replies once) ==="
claude -p --permission-mode bypassPermissions \
'You are an assistant named "claude-code" in a multi-agent standup. Do these tool calls in order:
1. Call mcp__sidebar__join with channel="standup".
2. Call mcp__sidebar__inbox with wait_ms=15000 to wait for the standup question.
3. Read the question. Reply by calling mcp__sidebar__send with to="#standup" and body="<your one-sentence answer about what you are working on as claude right now>".
4. Stop. Output the body you sent in one line.' \
  >/tmp/claude-out.log 2>&1 &
CLAUDE_PID=$!

# Give both agents a moment to enter their inbox long-polls.
sleep 2.0

echo "=== master broadcasts the standup question ==="
"$BIN" send "#standup" "Quick standup: what are you working on right now?"

wait $CLAUDE_PID || true
wait $CODEX_PID || true

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
echo "=== #standup history ==="
"$BIN" history --channel standup --limit 10

rm -f /tmp/claude-out.log /tmp/codex-out.log "$TAIL_LOG"
