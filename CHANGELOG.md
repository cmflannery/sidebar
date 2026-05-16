# Changelog

All notable changes to this project are documented in this file. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [SemVer](https://semver.org/) (allowing breaking changes
in 0.x).

## [Unreleased]

### Added
- **`.github/workflows/release.yml`** — triggers on `v*.*.*` tag push.
  Builds release binaries for `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`, tarballs each
  with its `sha256` sum, and attaches them to the GitHub Release.

### Changed
- **Cap on `history` and `grep` result limits**: 1000 messages.
  Larger requests return `limit N exceeds max of 1000`.
- **Cap on `grep` query length**: 256 characters. A 1 MB substring
  query is almost certainly a bug and the LIKE scan is pure waste.
- **Cap on scheduled delivery delay**: 365 days from now. Beyond that,
  reject with a clear "max is 365 days" message. Past timestamps still
  fire on the next scheduler tick (intentional and tested).
- **Retention cleanup now also prunes scheduled rows** with status
  `delivered` or `failed` older than the retention cutoff. Pending
  rows are left alone — they're still waiting to fire.
- **Soft cap on message body size**: 64 KB. Larger sends are rejected
  with `body is N bytes; max is 65536`. Applies to both `Op::Send` and
  `Op::Schedule`. Bounds the daemon's memory footprint and keeps inbox
  output human-grokkable.
- **Cap on agent/channel name length**: 64 characters. Same `validate_name`
  helper used at every entry point (recipient parse, Join/Leave, Hello).
  Rejects with `name must be 64 characters or fewer`.

### Fixed
- **Empty/whitespace recipients no longer create ghost agent/channel
  rows.** Before this iteration, `sidebar send "" "x"`, `sidebar send
  "@" "x"`, or `sidebar send "#" "x"` silently succeeded and created
  an agent or channel with an empty name. Now dispatch validates the
  parsed `Recipient` (and channel names in Join/Leave, and MCP Hello
  agent names) and returns a clear `invalid recipient` error.
  `Recipient::parse` also now trims leading/trailing whitespace.

### Added
- **`sidebar channels [--details] [--json]`** — finally a CLI counterpart
  to the existing MCP `channels` tool. Plain mode lists channel names;
  `--details` adds a table with member counts and last-activity time.
  New wire op `Op::ChannelsDetailed`.
- **`sidebar inbox --mentions-only`** + `mcp__sidebar__inbox({mentions_only: true})`.
  Returns only messages explicitly addressed to the calling agent: DMs to
  them, or channel/broadcast messages where their name appears as an
  `@`-mention in the body. The rest stay unread for a follow-up plain
  inbox call. Useful for "give me just the stuff I have to act on" in
  a busy channel.
- **`@mention` semantics**. When a message goes to a channel or
  broadcast, `@name` tokens in the body cause the named agent to also
  get a delivery row — even if they aren't a channel member. Lets you
  @-ping someone into a discussion they aren't subscribed to. DMs
  ignore mentions (the target already receives the message). Email
  addresses (`user@example.com`) don't false-positive because `@` must
  follow whitespace or start of string.
- **`sidebar join <channel> [--as NAME]`** / **`sidebar leave ...`** +
  matching `mcp__sidebar__join` / `mcp__sidebar__leave` tools. Closes a
  real gap: previously the only auto-subscribed channel was `#general`,
  and there was no way to subscribe to others. Messages to `#foo` with
  no members would silently land with zero deliveries. Now agents
  explicitly opt in; channels are still auto-created on send.
- **`examples/demo-channel.sh`** — second flagship demo: claude + codex
  both `join("standup")`, master broadcasts a question to `#standup`,
  both subscribers respond on the channel. Captured output in README.
- **`sidebar grep <query>`** + **`mcp__sidebar__search`** — case-insensitive
  substring search across all message bodies. Returns newest matches
  first, capped (default 50, configurable with `--limit`). Useful for
  agents looking up earlier conversation context and for humans
  debugging a chatty session.
- **`--json` flag** on `participants`, `inbox`, `history`, and `grep`
  (in addition to the `agents` / `status` flags landed last iteration).
  Same JSON shapes as the corresponding MCP tools return.
- **`sidebar agents [--all] [--json]`** — table view of agents with
  human-readable last-seen times (`just now`, `42s ago`, `3h ago`,
  `12d ago`). Default hides agents not seen in the last 7 days; `--all`
  overrides. `--json` emits an array of `{name, first_seen, last_seen}`
  for scripting.
- **`sidebar status --json`** — same StatusInfo struct as the table
  form, machine-readable.
- New wire op `Op::Agents { include_stale }` and response variant
  `ResponseData::AgentsDetailed { agents_detailed }`.
- `install/` directory with `com.sidebar.daemon.plist` (launchd, macOS)
  and `sidebar.service` (systemd user unit, Linux), plus an
  `install/README.md` with copy-paste setup steps.
- CI smoke step: `cargo install --path . --root /tmp/...` then exercises
  the installed binary's `--help` to confirm subcommands surface.
- `examples/bench.sh` — reproducible local perf numbers (the same ones
  in README's perf table).
- `examples/demo-claude-codex.sh` — the flagship demo: launches real
  `claude` and `codex` CLI subshells; they coordinate on a question
  through sidebar with no human intervention. Captured output in
  README. Five-second wall-clock turnaround end to end.

### Changed
- `sidebar tail` now prefixes each line with `[HH:MM:SS]` to match
  `history` / `inbox` output.
- `sidebar history` and `sidebar inbox` now print timestamps in local
  time (previously UTC), which is more useful for humans glancing at
  output.
- Cargo.toml gained `keywords`, `categories`, `homepage`,
  `documentation`, `readme`, and an `exclude` list. `cargo publish
  --dry-run` succeeds (45 KB compressed).
- `mcp.rs::call` refactored: split into `call` + `call_inner`, factored
  out a `format_response_data` helper, replaced manual JSON-by-hand
  fallbacks with `serde_json` so error strings escape correctly.

### Added
- `CONTRIBUTING.md` with build/test loop, conventions, scope, and a
  short "good first issues" list.

## [0.3.0] – 2026-05-16

### Added
- **Automatic name uniquification** for MCP sessions. Two concurrent
  `sidebar mcp --as claude-code` stubs now get `claude-code` and
  `claude-code-2` respectively, instead of stepping on each other's
  inbox. The daemon tracks active names in memory and releases them on
  disconnect. The MCP `whoami` tool reports the assigned name.
- New wire frame `HelloAck { agent }` sent by the daemon after every
  Hello. Clients learn their assigned identity from this frame.
- `sidebar status` — daemon health snapshot: paused state, agent count,
  channel count, unread messages, pending scheduled rows, uptime,
  socket and DB paths. When the daemon is down, prints a friendly
  message instead of erroring.
- Concurrency stress test: 64 parallel sends all land in history with
  no losses (`concurrent_sends_all_land_in_history`).
- `CHANGELOG.md` and `CLAUDE.md` at the repo root.

### Changed
- **Wire protocol** now expects a HelloAck frame from the daemon
  immediately after Hello. Clients built on prior versions will break.
- `Store::send_message` returns `Result<i64>` (the message id) directly
  instead of a `SendResult` struct. The unused `recipients` field is
  gone.

### Removed
- Internal `agent_name_blocking` helper that only existed to populate
  the now-dead `SendResult.recipients`.

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
