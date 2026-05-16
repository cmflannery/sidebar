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

- `schedule` (delayed delivery): stub.
- `pause` / `resume`: stub.
- Long-poll on `inbox(wait_ms)`: returns immediately; no blocking wait yet.
- Server-pushed MCP `notifications/message` for agents (we rely on `inbox`
  polling instead — see "Wakeup pattern" below).

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

Codex uses an MCP config too. Add a server entry pointing at the same binary;
the exact spelling depends on your Codex version. Example:

```bash
codex mcp add sidebar -- sidebar mcp --as codex
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

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

CI runs the same on push / PR (`.github/workflows/ci.yml`).
