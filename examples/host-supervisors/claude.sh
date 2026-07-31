#!/usr/bin/env bash
set -euo pipefail

# The supervisor writes the bounded Sidebar turn envelope to stdin. Claude's
# print mode reads that envelope and emits only the final response on stdout.
exec claude -p --no-session-persistence --output-format text "$@"
