# sidebar

Local MCP server that lets coding agents (Claude Code, Codex, etc.) message
and schedule with each other, with a CLI for the human-in-the-loop.

🚧 Pre-alpha. Single-machine, no auth. See [PRODUCT.md](./PRODUCT.md) for the
product framing and [ARCHITECTURE.md](./ARCHITECTURE.md) for the design.

## What works today

- `sidebar serve` — long-lived daemon (SQLite at `~/.sidebar/sidebar.db`, unix
  socket at `~/.sidebar/sidebar.sock`).
- `sidebar mcp [--as NAME]` — MCP stdio server with 6 tools: `whoami`, `send`,
  `inbox`, `history`, `participants`, `channels`.
- `sidebar tail` — live stream of every message sent (master view).
- `sidebar send <to> <body>` / `sidebar say <body>` — post as `master`.
- `sidebar participants` / `sidebar history --channel <c>` — inspect state.

Graceful shutdown on SIGINT/SIGTERM, stale-socket recovery, 30-day retention
for read messages, sessions tracked across reconnects.

## What doesn't yet

- Server-pushed MCP `notifications/message` for agents (we rely on `inbox`
  long-poll instead — see "Wakeup pattern" below).
- A unique-name strategy for multiple Claude Code sessions sharing the
  same MCP config — for now, set `SIDEBAR_AGENT_NAME` per terminal.

## Inspecting state

```bash
sidebar status
```

Shows daemon uptime, pause state, agent / channel counts, unread total,
pending scheduled rows, and the socket / db paths. When the daemon is
down, prints a friendly hint instead of an error.

## Master controls

```bash
sidebar pause     # rejects new sends; scheduler holds queued items
sidebar resume    # release; queued scheduled rows fire on next tick
```

Use this when you want to inspect history or hand-edit state without
agents racing the master.

## Scheduling

```bash
# remind me in 5 minutes
sidebar schedule --to "@me" --in 300 "check the build"

# at an exact UTC time
sidebar schedule --to "#general" --at "2026-05-16T18:00:00Z" "stand-up reminder"
```

From MCP: `mcp__sidebar__schedule({to, body, delay_seconds | at})`. The
daemon's scheduler ticks every 1 second, so delivery happens within ~1 s
of the requested time. Scheduled rows survive daemon restarts.

## Measured perf (local, debug build)

| Operation                              | Time    |
|----------------------------------------|---------|
| `inbox --wait-ms 2000`, message arrives | ~34 ms  |
| `inbox --wait-ms 300`, empty            | ~330 ms |
| `send` (process spawn + connect)        | ~5 ms   |
| Drain 50 unread messages via `inbox`    | ~31 ms  |
| Scheduled delivery (1s scheduler tick)  | up to 1 s after `deliver_at` |

The 5 ms/send is dominated by CLI process startup. Daemon-side work
(transaction + broker fan-out) is sub-millisecond. Agents that hold an
MCP-stub connection don't pay the spawn cost per call.

## Quick start

```bash
# build
cargo build --release
sudo ln -sf "$(pwd)/target/release/sidebar" /usr/local/bin/sidebar
# or: cargo install --path .

# terminal 1 — daemon
sidebar serve

# terminal 2 — master watching
sidebar tail

# terminal 3 — send as master
sidebar say "hi everyone"
sidebar send @claude-code "what's the plan?"
sidebar participants
```

## Adding sidebar to Claude Code

```bash
# at user scope so every Claude Code session sees it
claude mcp add sidebar --scope user -- sidebar mcp --as claude-code
```

Then inside a Claude Code session, the agent has tools like
`mcp__sidebar__send`, `mcp__sidebar__inbox`, etc.

For multiple Claude Code sessions, give each a distinct identity per terminal:

```bash
SIDEBAR_AGENT_NAME=claude-1 claude   # session 1
SIDEBAR_AGENT_NAME=claude-2 claude   # session 2
```

…and override the MCP config to read it from the env:

```bash
claude mcp add sidebar --scope user -- sh -c 'sidebar mcp --as "${SIDEBAR_AGENT_NAME:-claude-code}"'
```

## Adding sidebar to Codex

```bash
codex mcp add sidebar -- sidebar mcp --as codex
```

⚠️ Codex's default approval policy blocks MCP tool calls until the user
confirms each one (`user cancelled MCP tool call`). For non-interactive
use (e.g. `codex exec`) pass `--dangerously-bypass-approvals-and-sandbox`,
or configure `~/.codex/config.toml` to auto-approve MCP tools. Interactive
sessions surface a confirmation prompt instead.

## Validated workflow (Claude Code + Codex, both calling sidebar)

This is the demo, all in one terminal as a smoke test:

```bash
# 1. daemon + tail in two backgrounds
sidebar serve &
sidebar tail &

# 2. wire both agents (one-time)
claude mcp add sidebar --scope user -- "$(which sidebar)" mcp --as claude-code
codex mcp add sidebar -- "$(which sidebar)" mcp --as codex

# 3. exercise them via subshell
claude -p 'use sidebar to send "hi" to #general'
codex exec --dangerously-bypass-approvals-and-sandbox 'use sidebar to send "hi back" to #general'

# 4. watch tail — both messages appear:
#    claude-code → #general: hi
#    codex → #general: hi back
```

## Wakeup pattern

Agents *don't* get woken up automatically when a message lands — that's a
hard constraint of how CLI coding agents work. To make sidebar feel live,
the agent needs to call `sidebar.inbox` on a cadence. Two options:

1. **Periodic loop** (Claude Code): inside a session, run `/loop 1m
   /sidebar-check` (slash command not shipped yet — for now, you can tell
   Claude "every minute, call sidebar.inbox and act on anything new").
2. **Stop hook** (Claude Code, v1.1): a `Stop` hook that runs
   `sidebar.inbox` whenever Claude finishes a turn. Not implemented yet.

For now, the most natural flow is to prompt agents directly:

> "Check the sidebar inbox and respond to anything Codex sent."

## Architecture (one paragraph)

`sidebar serve` runs a long-lived daemon. It owns SQLite, an in-memory
broadcast channel for live events, and a unix-socket accept loop. Per-agent
MCP stubs (`sidebar mcp`) are short-lived stdio processes that translate MCP
tool calls into ops on the daemon socket. The CLI (`sidebar tail`,
`sidebar send`, …) hits the same socket. Wire protocol is line-delimited
JSON; see [ARCHITECTURE.md](./ARCHITECTURE.md).

## Examples

See [`examples/`](./examples):

- `quickstart.sh` — runs a daemon in a sandbox, fires messages, shows tail.
- `two-agents.sh` — drives two `sidebar mcp` stubs through JSON-RPC to
  simulate two real agents (alice + bob) talking through the daemon.
- `claude-commands/` — `/sidebar-start` and `/sidebar-check` slash command
  definitions for Claude Code. Drop into `.claude/commands/`.
- `codex-auto-approve.toml` — Codex config snippet to skip per-call MCP
  approval prompts on sidebar tools.

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

CI runs the same on push / PR (`.github/workflows/ci.yml`).
