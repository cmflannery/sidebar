#!/usr/bin/env bash
# bench.sh — produce real perf numbers for the README.
#
# Measures:
#   1. send→tail wake latency
#   2. inbox long-poll wake latency (the headline number)
#   3. send throughput
#   4. inbox drain throughput
#   5. schedule round-trip latency
#
# Usage: ./examples/bench.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${SIDEBAR_BIN:-$REPO_DIR/target/release/sidebar}"

if [ ! -x "$BIN" ]; then
  echo "Building release binary at $BIN ..."
  (cd "$REPO_DIR" && cargo build --release)
fi

SANDBOX="$(mktemp -d -t sidebar-bench.XXXXXX)"
export SIDEBAR_HOME="$SANDBOX"

cleanup() {
  pkill -INT -f "$BIN serve" 2>/dev/null || true
  sleep 0.3
  rm -rf "$SANDBOX"
}
trap cleanup EXIT

ms() { python3 -c "import time; print(int(time.time()*1000))"; }

# ---- daemon ----
"$BIN" serve >"$SANDBOX/daemon.log" 2>&1 &
until grep -q "daemon listening" "$SANDBOX/daemon.log" 2>/dev/null; do sleep 0.02; done
printf '%-40s ' "daemon startup"
echo "$(grep -c "daemon listening" "$SANDBOX/daemon.log") daemon up"

# ---- 1. inbox long-poll wake latency ----
"$BIN" inbox --as bob --wait-ms 5000 >"$SANDBOX/bob.out" 2>&1 &
sleep 0.15  # let the inbox waiter register
T0=$(ms)
"$BIN" send "@bob" "wake bob" >/dev/null
wait $! 2>/dev/null || true  # bob's inbox process exits when message arrives
T1=$(ms)
printf '%-40s %s ms\n' "inbox long-poll wake (send→deliver):" "$((T1-T0))"

# ---- 2. send latency (cold: process start + connect + send + close) ----
N=20
T0=$(ms)
for i in $(seq 1 $N); do
  "$BIN" send "@throughput-test" "msg-$i" >/dev/null
done
T1=$(ms)
TOTAL=$((T1-T0))
PER=$((TOTAL/N))
printf '%-40s %s ms total (%s ms each)\n' "$N sequential cold sends:" "$TOTAL" "$PER"

# ---- 3. inbox drain throughput ----
T0=$(ms)
"$BIN" inbox --as throughput-test >"$SANDBOX/drain.out"
T1=$(ms)
LINES=$(wc -l < "$SANDBOX/drain.out" | tr -d ' ')
printf '%-40s %s msgs drained in %s ms\n' "single inbox drain ($N msgs):" "$LINES" "$((T1-T0))"

# ---- 4. schedule round-trip ----
T0=$(ms)
"$BIN" schedule --to "@scheduled-test" --in 0 "immediate" >/dev/null
"$BIN" inbox --as scheduled-test --wait-ms 2000 >/dev/null
T1=$(ms)
printf '%-40s %s ms (1s scheduler tick floor)\n' "schedule+0s → inbox:" "$((T1-T0))"

# ---- 5. status ----
T0=$(ms)
"$BIN" status >/dev/null
T1=$(ms)
printf '%-40s %s ms\n' "status round-trip:" "$((T1-T0))"

echo
echo "Done. Sandbox at $SANDBOX will be removed."
