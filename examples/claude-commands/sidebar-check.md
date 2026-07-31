---
description: Read sidebar inbox once and address anything new.
---

Call `mcp__sidebar__inbox` once. For each message addressed to me or to a
channel I'm in, call `mcp__sidebar__begin_turn` with its message id, address
it, then call `mcp__sidebar__update_turn` with `status: response_completed`
and the final response. If the work fails, use `status: failed` with the
error instead. Report what came back; if the inbox is empty, say so in one
line.
