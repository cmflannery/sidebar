# Contributing to sidebar

Thanks for considering it. This is a small project; the goal is to keep
it small and sharp rather than grow it into a framework. PRs that improve
the existing core or add tests are very welcome; PRs that bolt on new
agent-runtime opinions are likely to bounce.

## Build / test loop

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same on push and PR (`.github/workflows/ci.yml`). The full
loop is well under a minute on a modern laptop.

End-to-end smoke check (needs `claude` and `codex` on PATH with sidebar
registered as an MCP — see README):

```bash
./examples/demo-claude-codex.sh
```

If you don't have those CLIs handy, the shell-only flow works too:

```bash
./examples/quickstart.sh
./examples/two-agents.sh
./examples/bench.sh
```

## Where things live

```
src/
  main.rs            clap CLI; subcommand definitions
  cli.rs             subcommand dispatchers
  client.rs          unix-socket client (used by CLI and MCP stub)
  paths.rs           SIDEBAR_HOME resolution
  proto.rs           wire shapes: Hello / HelloAck / Request / Response / Event / Op
  types.rs           Agent / Message / Recipient / Intent
  daemon/
    mod.rs           serve() — top-level: signals, scheduler, cleanup
    server.rs        accept loop, request dispatch, broker
    store.rs         rusqlite Store
    schema.sql       embedded via include_str!
  mcp.rs             rmcp 1.7 stdio MCP server
tests/integration.rs end-to-end via the binary
examples/            runnable demos
install/             launchd + systemd templates
```

[CLAUDE.md](./CLAUDE.md) is the same map written for AI agents helping
on the codebase.

## Conventions

- Edition 2024, Rust 1.85+, `unsafe_code = "forbid"` at the crate root.
- Clippy pedantic is on. A short list of pragmatic allows is in
  `Cargo.toml`'s `[lints.clippy]`; please don't add new ones without a
  comment explaining why.
- New `#[allow(dead_code)]` should be temporary and have a TODO; don't
  let them pile up.
- Wire shapes go in `proto.rs`; domain types go in `types.rs`.
- New CLI subcommands: add to `Command` enum in `main.rs`, then dispatch
  in `cli.rs`. New MCP tools: add to the `tool_router` impl in `mcp.rs`.
- Tests for new behavior live in `tests/integration.rs` and exercise the
  installed binary, not internal APIs.

## Good first issues

- `sidebar agents` table view (name, last_seen, channel memberships).
- `--json` output flag on `participants`, `inbox`, `history`.
- A search command: `sidebar search "term"` over message bodies.
- A purge / archive flow for inactive agents.

## What's out of scope

- Multi-machine sidebar / network transport.
- Auth between clients and the daemon.
- Built-in consensus/voting helpers (sidebar is the *transport*, not the
  deliberation engine — see PRODUCT.md §9).
- A web/desktop UI.

## License

By contributing you agree your changes are MIT-licensed (see LICENSE).
