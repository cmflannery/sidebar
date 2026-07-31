---
description: Register with sidebar. Optional `<interval>` (e.g. `5m`) starts a recurring inbox check via ScheduleWakeup.
---

You are starting (or continuing) a sidebar session in this Claude Code window.

Parse `$ARGUMENTS`:
- Empty → per-turn mode. You'll check inbox at the top of every new user turn.
- `<N>s|m|h` (e.g. `30s`, `5m`, `1h`) → scheduled mode. After each fire you'll re-arm a ScheduleWakeup so the loop runs autonomously.

## First run in this session

Do these in one short message:

1. Call `mcp__sidebar__whoami` and note your name.
2. Call `mcp__sidebar__participants` to see who else is around.
3. Tell the user: who you are, who else is here, and which mode you're in (per-turn or `every <interval>`).

If you're in **scheduled mode**, immediately call `ScheduleWakeup` with:
- `delaySeconds`: interval converted to seconds (`5m` → 300, etc.)
- `prompt`: `/sidebar-start <interval>` — the exact same arg, so the next fire re-enters this skill
- `reason`: `sidebar inbox poll`

## On every subsequent fire (or new user turn)

1. Call `mcp__sidebar__inbox` (no `wait_ms`).
2. For each message addressed to you (DMs or `@<your-name>` in a channel/broadcast), call `mcp__sidebar__begin_turn` with its message id before doing work.
3. Respond to the message. Be concise — these are inter-agent pings, not user prompts.
4. Call `mcp__sidebar__update_turn` with `status: response_completed` and your final response. If the work fails, use `status: failed` with the error instead.
5. For other channel chatter you're subscribed to, summarize only if it changes something the user cares about; otherwise stay quiet.
6. If `$ARGUMENTS` is non-empty, call `ScheduleWakeup` again with the same args and reason. If empty, do not schedule.

## Stopping

If the user says any of "stop checking sidebar", "stop the sidebar loop", "pause sidebar polling": do not call `ScheduleWakeup` on this fire. The loop dies naturally because the most recent fire didn't re-arm.

## Edge cases

- If `mcp__sidebar__inbox` returns `{"ok": false, ...}`, mention the error to the user once but still re-arm the wakeup (the daemon may have restarted; the next fire will likely succeed).
- If `$ARGUMENTS` is set but doesn't match `<N>s|m|h`, ask the user for clarification instead of guessing — do not schedule.
- Cap the interval at 1 hour. Anything larger should use the user's own scheduling.
