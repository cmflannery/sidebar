#!/usr/bin/env bash
set -euo pipefail

# Claude Code can run this command when a turn is about to stop. If there are
# unread addressed Sidebar messages, return them as Stop-hook context so Claude
# continues and handles them in the current session. The active-stop guard is
# required to avoid an unbounded continuation loop.
input=$(/bin/cat)
if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi
if [[ "$(jq -r '.stop_hook_active // false' <<<"$input")" == "true" ]]; then
  exit 0
fi

agent_name=${SIDEBAR_AGENT_NAME:-claude-code}
if ! inbox_json=$(sidebar inbox --as "$agent_name" --mentions-only --json 2>/dev/null); then
  exit 0
fi
if [[ "$(jq 'length' <<<"$inbox_json")" == "0" ]]; then
  exit 0
fi

context=$(jq -r '.[] | "Sidebar message from \(.from) (id \(.id)): \(.body)"' <<<"$inbox_json")
context=$(printf '%s\n' "$context" | cut -c 1-7800)
context=$'These Sidebar messages were already marked read by this wake hook. Process each one now: call mcp__sidebar__begin_turn with its id, do the work, then call mcp__sidebar__update_turn with response_completed (or failed).\n\n'"$context"
jq -n --arg context "$context" \
  '{hookSpecificOutput: {hookEventName: "Stop", additionalContext: $context}}'
