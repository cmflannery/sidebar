# CLAUDE.md

Pointer file for AI agents (Claude or otherwise) working on this repo.

## What sidebar is

A local-only MCP server in Rust that lets coding agents (Claude Code,
Codex, …) message and schedule with each other, with a master CLI for
the human-in-the-loop. See [PRODUCT.md](./PRODUCT.md) for the *what* and
[ARCHITECTURE.md](./ARCHITECTURE.md) for the *how*.

## Repo layout

```
src/
  main.rs            # clap CLI entry, subcommand definitions
  cli.rs             # subcommand dispatchers (call into client / daemon / mcp)
  client.rs          # unix-socket client used by CLI and MCP stub
  paths.rs           # SIDEBAR_HOME resolution
  proto.rs           # wire types: Hello / Request / Response / Event / Op
  types.rs           # domain types: Agent / Message / Recipient / Intent
  daemon/
    mod.rs           # serve() — daemon top-level, signal handling, scheduler/cleanup tasks
    server.rs        # unix-socket accept loop, per-conn request handling, dispatch
    store.rs         # rusqlite-backed Store (single mutex'd connection + spawn_blocking)
    schema.sql       # SQLite schema (embedded via include_str!)
  mcp.rs             # rmcp 1.7 stdio MCP server, proxies tools to daemon
tests/
  integration.rs     # end-to-end tests; spawn the binary, exercise via CLI / MCP stdio
examples/
  quickstart.sh
  two-agents.sh
  claude-commands/
    sidebar-start.md
    sidebar-check.md
  codex-auto-approve.toml
```

## Key conventions

- **Edition 2024, Rust 1.85+.** `unsafe_code = "forbid"` at the crate
  root. Tests rely on `Child::kill()` (SIGKILL) for teardown so they
  don't need unsafe.
- **Clippy pedantic** is on; a few common-sense allows are in
  `Cargo.toml`'s `[lints.clippy]` section. Don't add new allows without
  a comment explaining why.
- **rusqlite is sync**; the store routes blocking calls through
  `tokio::task::spawn_blocking`. If a query feels awkward, that's why.
- **No new dead-code allows** unless the feature is staged and will be
  used in a follow-up commit.
- **Naming**: subcommands and tools use the wire vocabulary — `send`,
  `inbox`, `history`, etc. Don't invent new names for the same thing.

## Wire protocol

NDJSON over a unix socket at `~/.sidebar/sidebar.sock`. See ARCHITECTURE.md §5
for the full frame catalog. Briefly:

- First frame is `Hello::Cli { speaking_as }` or `Hello::Mcp { agent, version }`.
- Then `Request { id, op }` / `Response { id, ok, error?, data? }` pairs.
- `Op::Subscribe` flips the connection into event-forwarding mode for `tail`.

## Running tests

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same on push (`.github/workflows/ci.yml`).

## Manual end-to-end smoke test

```bash
cargo build --release
./examples/quickstart.sh           # one-process demo
./examples/two-agents.sh           # multi-MCP-stub demo
```

For Claude Code / Codex integration, see the README's "Adding sidebar to
…" sections.

## Common pitfalls

- **MCP tool returns**: each `#[tool]` method must return a `String` (or
  similar). We return a JSON string so callers can parse — when adding a
  new tool, format the response JSON the same way (see `mcp.rs::call`).
- **Adding new `ResponseData` variants**: `ResponseData` is
  `#[serde(untagged)]`, so each variant must have distinct field names
  from the others. The MCP `call` helper also needs a match arm.
- **Schedule latency**: the scheduler ticks every 1 s. Don't write tests
  that assume sub-second scheduled delivery.
- **Long-poll**: `Op::Inbox { wait_ms }` is capped at 5 minutes server-side.
- **Tests are flaky if you don't wait for "daemon listening"**: see
  `Sandbox::new` — it grep-polls the log to know when the socket is bound.

## Style notes for prose (docs, READMEs, comments)

- Be concrete. "34 ms inbox wake" beats "fast inbox wake".
- Don't oversell. If something is a v1 stub, say so.
- Don't add comments that just restate the next line of code.
- Lowercase headings in markdown; capitalize the first word.

## Where to look first when joining

1. `PRODUCT.md` — 5-minute read on what this is and why.
2. `ARCHITECTURE.md` — concrete design with diagrams.
3. `src/daemon/server.rs::dispatch` — the central nervous system; every
   feature ends up wired here.
4. `tests/integration.rs` — what behavior we promise.
