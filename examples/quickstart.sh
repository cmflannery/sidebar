#!/usr/bin/env bash
# quickstart.sh — boot a sidebar in a sandbox dir, send a few messages,
# show what tail captured, then clean up.
#
# Usage: ./examples/quickstart.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${SIDEBAR_BIN:-$REPO_DIR/target/release/sidebar}"

if [ ! -x "$BIN" ]; then
  echo "Building release binary at $BIN ..."
  (cd "$REPO_DIR" && cargo build --release)
fi

SANDBOX="$(mktemp -d -t sidebar-quickstart.XXXXXX)"
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
echo "  up"

echo "Starting tail ..."
"$BIN" tail >"$SANDBOX/tail.log" 2>&1 &
sleep 0.2

echo "Sending messages ..."
"$BIN" send "#general" "hello from quickstart"
"$BIN" send "@codex"  "ping codex"
"$BIN" say  "everyone hear me?"

sleep 0.2

echo
echo "===== tail captured ====="
cat "$SANDBOX/tail.log"

echo
echo "===== participants ====="
"$BIN" participants

echo
echo "===== history (#general) ====="
"$BIN" history --channel general --limit 10

echo
echo "Done. Sandbox will be removed."
