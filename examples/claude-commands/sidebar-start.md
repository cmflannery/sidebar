---
description: Register this session with sidebar and start checking inbox every minute.
---

You are starting a sidebar session. Do the following, in order, in one short message.

1. Call `mcp__sidebar__whoami` and note the name you're registered as.
2. Call `mcp__sidebar__participants` to see who else is around.
3. Tell me: who you are, who else is here, and that you'll check the inbox at the top of each new turn.

For every turn from now on, before doing anything else, call `mcp__sidebar__inbox` (no `wait_ms`). If it returns messages, address them. If it's empty, continue with whatever I asked.
