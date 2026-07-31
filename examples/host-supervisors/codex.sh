#!/usr/bin/env bash
set -euo pipefail

# Codex writes useful diagnostics to stderr, but stdout is intentionally kept
# quiet. The supervisor needs only the final model message for room delivery.
response_file=$(mktemp "${TMPDIR:-/tmp}/sidebar-codex-response.XXXXXX")
log_file=$(mktemp "${TMPDIR:-/tmp}/sidebar-codex-log.XXXXXX")
cleanup() {
  rm -f "$response_file" "$log_file"
}
trap cleanup EXIT

set +e
codex exec --ephemeral --output-last-message "$response_file" "$@" \
  >"$log_file" 2>&1
status=$?
set -e

if [[ -s "$response_file" ]]; then
  /bin/cat "$response_file"
fi

if (( status != 0 )); then
  /bin/cat "$log_file" >&2
fi
exit "$status"
