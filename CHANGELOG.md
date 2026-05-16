# Changelog

All notable changes to this project are documented in this file. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [SemVer](https://semver.org/) (allowing breaking changes
in 0.x).

## [Unreleased]

### Added
- `sidebar status` — daemon health snapshot: paused state, agent count,
  channel count, unread messages, pending scheduled rows, uptime,
  socket and DB paths. When the daemon is down, prints a friendly
  message instead of erroring.
- `CHANGELOG.md` and `CLAUDE.md` at the repo root.

## [0.2.0] – 2026-05-16

### Added
- **Scheduled delivery** — `sidebar schedule --to <r> --in N | --at <iso>`
  and `mcp__sidebar__schedule`. A scheduler task in the daemon ticks every
  1 s, delivers due rows through `send_message` (transactionally), and
  fans out events through the broker. Scheduled rows survive daemon
  restart.
- **Pause / Resume** — `sidebar pause` rejects new `Op::Send` and holds
  scheduler deliveries; `sidebar resume` releases. Both broadcast
  `Event::Paused` / `Event::Resumed` to subscribed clients.
- **examples/** directory: `quickstart.sh`, `two-agents.sh`,
  `claude-commands/sidebar-start.md`, `claude-commands/sidebar-check.md`,
  `codex-auto-approve.toml`.
- **Two-MCP-stub marquee integration test** proving the multi-agent vision
  works end-to-end via real stdio MCP stubs.

### Fixed
- MCP stub no longer dies if the daemon is unreachable at startup
  (regression: Claude Code surfaced `-32000` on `/mcp` reload). Stub now
  starts cleanly, returns `daemon not reachable` from tool calls, and
  reconnects on the next call once the daemon is back. Locked in by
  `mcp_stub_survives_missing_daemon`.
- Inbox MCP tool description corrected — used to say `wait_ms` was ignored
  even after long-poll landed.

## [0.1.0] – 2026-05-15

### Added
- Daemon (`sidebar serve`) with unix socket protocol, SQLite store at
  `~/.sidebar/sidebar.db`, graceful SIGINT/SIGTERM shutdown, stale-socket
  detection, 30-day retention cleanup, session tracking.
- MCP stub (`sidebar mcp [--as NAME]`) using rmcp 1.7. Tools: `whoami`,
  `send`, `inbox`, `history`, `participants`, `channels`.
- CLI: `sidebar tail`, `send`, `say`, `participants`, `history`, `inbox`.
- **Inbox long-poll**: `Op::Inbox { wait_ms }` subscribes to the broker
  and wakes when an event addressed to the calling agent arrives. Capped
  at 5 minutes.
- Integration tests covering CLI end-to-end and the daemon-down
  regression. CI via GitHub Actions running `fmt --check`, `clippy -D
  warnings`, `check`, and `test`.
- Validated end-to-end with both Claude Code and Codex via subshell.
