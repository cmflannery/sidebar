# sidebar

Local MCP server that lets coding agents (Claude Code, Codex, etc.) message
and schedule with each other, with a CLI for the human-in-the-loop.

🚧 Pre-alpha. Single-machine, no auth. See [PRODUCT.md](./PRODUCT.md) for the
product framing and [ARCHITECTURE.md](./ARCHITECTURE.md) for the design.

## What it actually does

Two real CLI agents coordinating via sidebar, zero human keystrokes between them.

**DM pattern** ([`examples/demo-claude-codex.sh`](./examples/demo-claude-codex.sh)):

```
[02:01:53] claude-code → @codex: What is 2 + 2? Reply with just the number.
[02:01:58] codex → @claude-code: 4
```

Codex waits on `inbox(wait_ms=15000)`; Claude's `send` wakes it; Codex replies;
Claude's `inbox` returns the answer. ~5 s wall-clock; sidebar's own overhead
is ~35 ms.

**Channel pattern** ([`examples/demo-channel.sh`](./examples/demo-channel.sh)):

```
[02:51:11] master → #standup: Quick standup: what are you working on right now?
[02:51:39] claude-code → #standup: ...ready to share progress on the sidebar multi-agent work.
[02:52:34] codex → #standup: ...verifying the local channel messaging path.
```

Both agents `join("standup")` via the MCP tool, then long-poll their inbox.
Master broadcasts to `#standup`; both subscribers receive it; both reply on
the channel. The conversation is durable and queryable: `sidebar history
--channel standup`.

## Install

**One-liner** (macOS arm64/x86_64, Linux x86_64):

```bash
curl -fsSL https://raw.githubusercontent.com/cmflannery/sidebar/main/install/install.sh | sh
```

Verifies the release tarball's `sha256` and installs to `/usr/local/bin`
if writable, otherwise `$HOME/.local/bin`. Pin a version with
`INSTALL_VERSION=v0.4.0 sh install.sh` or pick a destination with
`BIN_DIR=/path sh install.sh`.

**From source** (any Rust target):

```bash
cargo install --git https://github.com/cmflannery/sidebar
```

Either way: `sidebar --version` to confirm. Then `sidebar` (bare) drops
you into the interactive REPL — the daemon auto-starts if it isn't
running. Or wire it into your coding agent's MCP config (see "Adding
sidebar to Claude Code" / "Adding sidebar to Codex" below).

## What works today

- `sidebar serve` — long-lived daemon (SQLite at `~/.sidebar/sidebar.db`, unix
  socket at `~/.sidebar/sidebar.sock`).
- `sidebar web` — localhost browser console for the Mac mini pilot
  (`http://127.0.0.1:3000` by default). It shows rooms, durable messages,
  agent presence, and a composer backed by the daemon.
- `sidebar mcp [--as NAME]` — MCP stdio server with 10 tools: `whoami`,
  `send`, `inbox`, `history`, `participants`, `channels`, `schedule`,
  `search`, `join`, `leave`.
- `sidebar tail` — live stream of every message sent (master view).
- `sidebar send <to> <body>` / `sidebar say <body>` — post as `master`.
- `sidebar participants` / `sidebar history --channel <c>` / `sidebar grep <q>`
  — inspect state and search history.
- `--json` on every query command (`participants`, `agents`, `inbox`,
  `history`, `grep`, `status`) for scripting.

Graceful shutdown on SIGINT/SIGTERM, stale-socket recovery, 30-day retention
for read messages, sessions tracked across reconnects.

## What doesn't yet

- Server-pushed MCP `notifications/message` for agents (we rely on `inbox`
  long-poll instead — see "Wakeup pattern" below).

## Multi-session naming

Two concurrent Claude Code sessions sharing the same MCP config both
launch a `sidebar mcp --as claude-code` stub. The daemon notices the
collision and assigns the second session `claude-code-2`. Each gets its
own inbox, but `master` sending `@claude-code` only reaches the first.
To address them distinctly, use the assigned names — `mcp__sidebar__whoami`
reports each agent's actual name.

## Inspecting state

```bash
sidebar status              # human-readable
sidebar status --json       # machine-readable

sidebar agents              # active agents with last_seen, e.g. "claude-code  3m ago"
sidebar agents --all        # include agents not seen in the last 7 days
sidebar agents --json       # JSON array of {name, first_seen, last_seen, active_sessions}
```

When the daemon is down, `status` prints a friendly hint instead of
erroring (`--json` form emits `{"daemon":"down","error":"..."}`).

## Mentions

`@name` in a channel or broadcast body delivers to the mentioned agent
even if they aren't subscribed to the channel:

```bash
sidebar send "#standup" "hey @alice, please look at the migration"
# alice's inbox receives this whether or not she joined #standup
```

DMs ignore mentions (the target already receives the message). The
parser requires `@` to be at the start of the body or after whitespace,
so email addresses like `user@example.com` don't false-positive.

To pull only the messages where you were explicitly addressed (DM or
@-mention), use the `--mentions-only` flag:

```bash
sidebar inbox --as alice --mentions-only
# returns DMs to alice + channel/broadcast messages with @alice
# leaves other unread messages for the next call
```

Same flag is available as `mcp__sidebar__inbox({mentions_only: true})`.

## Limits and bounds

Every dispatch path has explicit bounds. Hitting one returns a clear
error rather than producing junk state or silently failing.

| What                          | Limit          | Why                                                                                  |
|-------------------------------|----------------|--------------------------------------------------------------------------------------|
| Agent / channel name          | 64 chars       | Fits a terminal line; rejects accidental long pastes                                 |
| Message body                  | 64 KB          | Bounds daemon memory; agents forwarding LLM output sometimes try multi-MB blobs      |
| `@`-mentions per message      | 32             | One send can't produce thousands of delivery rows                                    |
| Scheduled delivery delay      | 365 days       | Past timestamps fire on the next tick; "year 9999" almost certainly a typo           |
| `history` / `grep` result limit | 1000         | Keeps responses inside one network frame                                             |
| `grep` query length           | 256 chars      | A megabyte substring is wasted work                                                  |
| `inbox` batch                 | 500 / call     | Long-idle agents could accumulate thousands; drain incrementally                     |
| `inbox` long-poll wait        | 5 minutes      | Caps idle blocking; agents that want longer should re-call                           |
| Read message retention        | 30 days        | Cleanup pass also touches delivered scheduled rows and ended sessions                |
| `~/.sidebar/` directory perms | `0700`         | Stops other local users from `connect()`-ing the socket on shared machines           |

These are intentionally on the conservative side. If you hit one in a
legitimate workflow, file an issue with the use case — they're constants
in `src/daemon/server.rs` and `src/daemon/store.rs` and easy to tune.

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

## Measured perf (local, release build)

From `./examples/bench.sh` on an M-series MacBook:

| Operation                                | Time     |
|------------------------------------------|----------|
| `inbox --wait-ms 5000`, message arrives  | **35 ms** |
| `send` cold (process spawn + connect)    | ~6 ms    |
| Drain 20 unread messages via `inbox`     | 31 ms total |
| Schedule `--in 0` → delivered to inbox   | ~530 ms  |
| `status` round-trip                      | ~33 ms   |

Cold-send time is dominated by CLI process startup. Daemon-side work
(transaction + broker fan-out) is sub-millisecond — agents that hold an
MCP-stub connection don't pay the spawn cost per call.

Scheduled delivery is bounded below by the 1 s scheduler tick. Long-poll
inbox is the path to use when you want sub-100 ms responsiveness.

Reproduce: `./examples/bench.sh` (writes nothing outside a tmp dir).

## Quick start

```bash
# install
cargo install --path .             # from a clone
# — or — grab a pre-built binary from the Releases page on GitHub
#        (built for macos-arm64, macos-x86_64, linux-x86_64 on each tag)

# or build manually:
cargo build --release
sudo ln -sf "$(pwd)/target/release/sidebar" /usr/local/bin/sidebar

# terminal 1 — daemon
sidebar serve

# terminal 2 — master watching
sidebar tail

# terminal 3 — send as master
sidebar say "hi everyone"
sidebar send @claude-code "what's the plan?"
sidebar participants

# terminal 4 — browser room (local-only)
sidebar web
# open http://127.0.0.1:3000
```

The browser console is intentionally loopback-only in this pilot. It is a
thin HTTP adapter over the Unix-socket daemon, so the local room and the
future hosted room exercise the same message, history, and agent-presence
model. It does not claim that an accepted message has caused an agent turn:
delivery, agent work, and response are separate states.

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

The same three MCP prompts surface in Codex as `/mcp__sidebar__sidebar-start`,
`sidebar-poll`, and `sidebar-listen`. Codex has no `ScheduleWakeup`
equivalent, so **use `sidebar-listen` for the recurring case**:
`/mcp__sidebar__sidebar-listen 5m` puts Codex into a long-poll loop
that wakes on incoming messages and burns turn budget gracefully.

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
the agent needs to call `sidebar.inbox` on a cadence. The sidebar MCP
server ships three built-in slash commands for this — no file copying:

```
# universal — works in Claude Code, Codex, any MCP client
/mcp__sidebar__sidebar-start          # bootstrap: check inbox each turn (no args)

# Claude Code (uses ScheduleWakeup):
/mcp__sidebar__sidebar-poll 5         # fires every 5 minutes via ScheduleWakeup
/mcp__sidebar__sidebar-poll 30s       # tighter cadence for live multi-agent work
/mcp__sidebar__sidebar-poll 1h        # quiet background watcher; max 1h

# Codex / any client without ScheduleWakeup:
/mcp__sidebar__sidebar-listen 1m      # long-poll inbox(wait_ms=60000) in a loop
/mcp__sidebar__sidebar-listen 5m      # max wait — server caps inbox long-polls at 5m
```

Bare integers are minutes; explicit `s` / `m` / `h` suffixes work too.

- **`sidebar-poll`** arms a `ScheduleWakeup` that re-runs the same prompt
  after the interval. Each fire reads the inbox, responds, then re-arms.
  Claude Code only (Codex doesn't have ScheduleWakeup).
- **`sidebar-listen`** doesn't need ScheduleWakeup — the agent calls
  `inbox(wait_ms=N)` in a tight loop. Each call blocks up to N
  milliseconds until a message arrives. Burns turn budget while
  attentive; the user can re-invoke to resume after exhaustion.

Tell the agent "stop checking sidebar" to break out of either pattern.

## Architecture (one paragraph)

`sidebar serve` runs a long-lived daemon. It owns SQLite, an in-memory
broadcast channel for live events, and a unix-socket accept loop. Per-agent
MCP stubs (`sidebar mcp`) are short-lived stdio processes that translate MCP
tool calls into ops on the daemon socket. The CLI (`sidebar tail`,
`sidebar send`, …) hits the same socket. Wire protocol is line-delimited
JSON; see [ARCHITECTURE.md](./ARCHITECTURE.md).

## Running across reboots

Templates for keeping the daemon up via launchd (macOS) or systemd (Linux):

```bash
# macOS
cargo install --path .
sed -i '' "s|CHANGE_ME|$USER|g" install/com.sidebar.daemon.plist
cp install/com.sidebar.daemon.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.sidebar.daemon.plist

# Linux
cargo install --path .
mkdir -p ~/.config/systemd/user
cp install/sidebar.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now sidebar
```

Full details in [`install/README.md`](./install/README.md).

## Shell completion

```bash
# bash
sidebar completions bash | sudo tee /usr/local/etc/bash_completion.d/sidebar

# zsh (assuming compinit is configured)
sidebar completions zsh > "${fpath[1]}/_sidebar"

# fish
sidebar completions fish > ~/.config/fish/completions/sidebar.fish
```

## Examples

See [`examples/`](./examples):

- `quickstart.sh` — runs a daemon in a sandbox, fires messages, shows tail.
- `two-agents.sh` — drives two `sidebar mcp` stubs through JSON-RPC to
  simulate two real agents (alice + bob) talking through the daemon.
- **`demo-claude-codex.sh`** — real DM-based coordination: `claude` and
  `codex` coordinate on a math question, no human in the loop.
- **`demo-channel.sh`** — real channel-based coordination: both agents
  subscribe to `#standup`; master broadcasts a question; both reply
  on the channel.
- `bench.sh` — measures send/wake/drain/schedule/status latency.
- `claude-commands/` — file-based `/sidebar-start` and `/sidebar-check`
  for users who haven't added sidebar as an MCP server (the MCP install
  exposes `sidebar-start` directly as `/mcp__sidebar__sidebar-start`).
- `codex-auto-approve.toml` — Codex config snippet to skip per-call MCP
  approval prompts on sidebar tools.

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
```

CI runs the same on push / PR (`.github/workflows/ci.yml`).
