#!/usr/bin/env bash
# two-agents.sh — simulate two real agent sessions (Alice and Bob) by
# driving two `sidebar mcp` stdio stubs via JSON-RPC. Alice sends a DM
# to Bob; Bob does an `inbox` long-poll and receives it.
#
# This is the closest you can get to the Claude-Codex demo without
# actually starting LLMs.
#
# Usage: ./examples/two-agents.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${SIDEBAR_BIN:-$REPO_DIR/target/release/sidebar}"

if [ ! -x "$BIN" ]; then
  echo "Building release binary at $BIN ..."
  (cd "$REPO_DIR" && cargo build --release)
fi

SANDBOX="$(mktemp -d -t sidebar-twoagent.XXXXXX)"
export SIDEBAR_HOME="$SANDBOX"
echo "Using sandbox: $SANDBOX"

cleanup() {
  pkill -INT -f "$BIN serve" 2>/dev/null || true
  sleep 0.3
  rm -rf "$SANDBOX"
}
trap cleanup EXIT

echo "Starting daemon ..."
"$BIN" serve >"$SANDBOX/daemon.log" 2>&1 &
until grep -q "daemon listening" "$SANDBOX/daemon.log" 2>/dev/null; do sleep 0.05; done

echo "Registering alice + bob via their MCP stubs ..."
# A handshake that registers the agent and then does one tool call.
register_send() {
  local name="$1" body="$2"
  cat <<EOF | "$BIN" mcp --as "$name" 2>>"$SANDBOX/mcp-$name.err" >"$SANDBOX/mcp-$name.out"
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"$name","version":"0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send","arguments":{"to":"@bob","body":"$body"}}}
EOF
}

# Alice sends a DM to Bob.
register_send "alice" "hello bob, this is alice"

echo
echo "Bob long-polls his inbox for up to 2s ..."
cat <<'EOF' | "$BIN" mcp --as bob 2>>"$SANDBOX/mcp-bob.err" >"$SANDBOX/mcp-bob.out"
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bob","version":"0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"inbox","arguments":{"wait_ms":2000}}}
EOF

echo "Bob's inbox tool result:"
python3 - "$SANDBOX/mcp-bob.out" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    if msg.get("id") != 2:
        continue
    content = msg.get("result", {}).get("content", [])
    for part in content:
        if part.get("type") != "text":
            continue
        try:
            payload = json.loads(part["text"])
        except json.JSONDecodeError:
            print(part["text"])
            continue
        for m in payload.get("messages", []):
            print(f"  {m['from']} → {m['to']}: {m['body']}")
PY

echo
echo "Participants now:"
"$BIN" participants

echo
echo "Done."
