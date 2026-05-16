# sidebar — architecture

Implementation design for v1. Companion to [PRODUCT.md](./PRODUCT.md) (the *what* / *why*).

---

## 1. Topology

Everything runs locally. One long-lived daemon owns state. Per-agent MCP stubs proxy to it. CLI commands also proxy to it.

```
   Claude Code        Codex         (any MCP client)
       │                │                  │
   stdio MCP        stdio MCP          stdio MCP
       │                │                  │
       ▼                ▼                  ▼
  ┌──────────┐    ┌──────────┐       ┌──────────┐
  │ sidebar  │    │ sidebar  │       │ sidebar  │   ← MCP stubs (short-lived per session)
  │   mcp    │    │   mcp    │       │   mcp    │
  └─────┬────┘    └─────┬────┘       └─────┬────┘
        │               │                  │
        └───────────────┴──────────────────┘
                        │
                    unix socket: ~/.sidebar/sidebar.sock
                        │
                ┌───────▼────────┐
                │ sidebar serve  │   ← daemon (long-lived)
                │  ┌──────────┐  │
                │  │ broker   │  │   in-memory pub/sub
                │  │ scheduler│  │   delayed deliveries
                │  │ store    │──┼──→ ~/.sidebar/sidebar.db (SQLite)
                │  └──────────┘  │
                └───────▲────────┘
                        │
                    unix socket (same)
                        │
                ┌───────┴────────┐
                │   sidebar CLI  │   tail / send / participants / pause / ...
                └────────────────┘
```

## 2. Binary layout

One Rust binary, `sidebar`, with subcommands via `clap`:

| Subcommand | Role | Lifetime |
|---|---|---|
| `sidebar serve` | Run the daemon. Listens on `~/.sidebar/sidebar.sock`. | Long-lived (until stopped). |
| `sidebar mcp` | MCP stdio server. Stub that proxies tool calls to daemon. Configured into Claude Code / Codex as an MCP server. | One per agent session. |
| `sidebar tail` | Stream messages live to terminal. | Foreground until Ctrl-C. |
| `sidebar send <to> <body>` | Send a message as `master`. | One-shot. |
| `sidebar say <body>` | Broadcast as `master`. | One-shot. |
| `sidebar participants` | List known agents. | One-shot. |
| `sidebar history [--channel X | --with Y]` | Print history. | One-shot. |
| `sidebar pause` / `sidebar resume` | Hold or release new message delivery. | One-shot. |

## 3. Daemon

The daemon (`sidebar serve`) is the only stateful component. Responsibilities:

- **Listen** on `~/.sidebar/sidebar.sock` (unix domain socket) for both MCP-stub and CLI clients.
- **Own** the SQLite connection pool. All reads and writes go through the daemon.
- **Maintain** an in-memory broker: per-agent subscriber lists, channel memberships, pending notifications.
- **Run** the scheduler: a tokio task that wakes on the soonest `scheduled.deliver_at` and moves due messages from `scheduled` into `messages`.
- **Auto-start** on demand? For v1 the user runs `sidebar serve` manually (in a terminal or via launchd later). MCP stubs that can't reach the socket return an error telling the user to start it.

Crate sketch:

- `tokio` — async runtime.
- `sqlx` with the `sqlite` feature — async SQLite. (Alternative: `rusqlite` + a blocking pool. Pick `sqlx` for one async story.)
- `serde` / `serde_json` — wire protocol.
- `tracing` / `tracing-subscriber` — logging to `~/.sidebar/sidebar.log`.
- `clap` — subcommands.
- `anyhow` / `thiserror` — error handling.

## 4. MCP stub

`sidebar mcp` is what the user wires into Claude Code's / Codex's MCP config. It's a short-lived process (one per agent session). On startup it:

1. Connects to `~/.sidebar/sidebar.sock`.
2. Reads MCP client info from the stdio handshake → forwards to daemon as `register`.
3. Translates each MCP tool call into a daemon request and streams the response back.
4. Streams daemon-pushed `notifications/message` events back out as MCP notifications (when applicable).

Crate: `rmcp` (official Rust MCP SDK). Verify on `cargo add` that the API for both server stdio and client-side notifications is shaped how we expect; fall back to a hand-rolled MCP impl if needed.

## 5. Wire protocol — daemon ↔ stubs/CLI

Length-prefixed newline-delimited JSON (NDJSON) over the unix socket. Each line is one request or response.

**Connection types** declared in a hello frame:

```json
{"hello": "mcp",   "agent": "claude-code", "version": "0.1.0"}
{"hello": "cli",   "as": "master"}
```

**Requests** are tagged with a client-chosen `id` for correlation:

```json
{"id": 17, "op": "send", "to": "@codex", "body": "review this", "reply_to": null}
{"id": 18, "op": "inbox", "wait_ms": 30000}
{"id": 19, "op": "schedule", "to": "@claude-code", "body": "check build", "at": "2026-05-15T20:00:00Z"}
{"id": 20, "op": "history", "channel": "#general", "limit": 50}
{"id": 21, "op": "participants"}
```

**Responses** echo the `id`:

```json
{"id": 17, "ok": true, "message_id": 42}
{"id": 18, "ok": true, "messages": [{ ... }, { ... }]}
```

**Server-pushed events** carry no `id`:

```json
{"event": "message", "to": "@codex", "from": "@claude-code", "body": "...", "message_id": 42}
{"event": "paused"}
```

This is internal and not exposed to agents — agents only see MCP tools.

## 6. Data model

SQLite at `~/.sidebar/sidebar.db`, WAL mode.

```sql
CREATE TABLE agents (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT UNIQUE NOT NULL,
  display_name TEXT,
  first_seen   TEXT NOT NULL,
  last_seen    TEXT NOT NULL,
  metadata     TEXT  -- JSON: persona, role hints, etc.
);

CREATE TABLE channels (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT UNIQUE NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE memberships (
  agent_id   INTEGER NOT NULL REFERENCES agents(id),
  channel_id INTEGER NOT NULL REFERENCES channels(id),
  joined_at  TEXT NOT NULL,
  PRIMARY KEY (agent_id, channel_id)
);

CREATE TABLE messages (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  from_agent   INTEGER NOT NULL REFERENCES agents(id),
  to_agent     INTEGER REFERENCES agents(id),   -- DM target, null if channel/broadcast
  to_channel   INTEGER REFERENCES channels(id), -- channel target, null if DM/broadcast
  is_broadcast INTEGER NOT NULL DEFAULT 0,
  body         TEXT NOT NULL,
  intent       TEXT,                            -- 'fyi' | 'question' | 'task' | 'handoff' | null
  reply_to     INTEGER REFERENCES messages(id),
  created_at   TEXT NOT NULL
);

CREATE TABLE deliveries (
  message_id   INTEGER NOT NULL REFERENCES messages(id),
  agent_id     INTEGER NOT NULL REFERENCES agents(id),
  delivered_at TEXT,
  read_at      TEXT,
  PRIMARY KEY (message_id, agent_id)
);

CREATE TABLE scheduled (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  payload      TEXT NOT NULL,   -- JSON of the send op
  deliver_at   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  status       TEXT NOT NULL DEFAULT 'pending'  -- pending | delivered | cancelled
);

CREATE INDEX idx_messages_created  ON messages(created_at);
CREATE INDEX idx_deliveries_agent  ON deliveries(agent_id, read_at);
CREATE INDEX idx_scheduled_deliver ON scheduled(status, deliver_at);
```

A `deliveries` row is created per addressed recipient when a message is sent:

- DM → one row (target).
- Channel → one row per channel member.
- Broadcast → one row per known agent (sans sender).

This is what lets `inbox` be a simple `WHERE agent_id=? AND read_at IS NULL` query.

## 7. Message protocol

Layered, in priority order:

1. **SQLite is the source of truth.** Every send is a `messages` row plus N `deliveries` rows in one transaction.
2. **Broker (in-memory pub/sub)** in the daemon notifies any subscribed clients (connected MCP stubs, `sidebar tail`) about new messages immediately.
3. **MCP notifications** — when a recipient agent has an open MCP stub connected and subscribed, the daemon pushes a `notifications/message` (via the stub) so the agent's harness can surface it. Whether the LLM actually wakes up depends on harness wiring (see §9).
4. **Long-poll on `inbox`** — `inbox(wait_ms)` blocks the call up to `wait_ms` waiting for a new delivery. Useful for `--print` mode loops and for harnesses that don't surface MCP notifications.

Reads:

- `inbox` returns unread `deliveries` joined with `messages`, marks them `read` (configurable).
- `history` is straightforward query against `messages` + `deliveries`.

## 8. Scheduling

`schedule` writes a row to `scheduled`. A scheduler task in the daemon sleeps until the next `MIN(deliver_at)`. When it fires, it runs the embedded `send` op as a normal send (creating `messages` + `deliveries`), then marks the row `delivered`. Clock changes / sleep / restart: on daemon start, it scans for any past-due `pending` rows and delivers them immediately.

Honest guarantees: sidebar **delivers** at the scheduled time. Whether the recipient *acts* on it depends on §9.

## 9. Wakeup pattern

Sidebar cannot push-trigger an LLM. To act on a message, an agent's harness must give the LLM a turn that includes the inbox check.

**Default mechanism: harness loop primitives.**

For Claude Code, we ship two slash commands the user installs:

- `/sidebar-start [interval]` — registers this session with sidebar (calls `whoami`), then runs `/loop {interval} /sidebar-check`. Default interval: 1 minute.
- `/sidebar-check` — calls `sidebar.inbox`. If empty, no-op. If non-empty, the command's output is the inbox content, which becomes the next user-side prompt — the LLM responds to it as if it were a message.

For Codex, we'll target the equivalent loop primitive on Codex's side (TBD on exact spelling; the *pattern* is identical).

**Future v1.1: hooks.** A `Stop`-style hook that checks sidebar on every turn boundary, enabling sub-second latency without `/loop` token cost. Optional, opt-in.

## 10. Agent identity

On first MCP tool call, the stub forwards an MCP `clientInfo` (e.g. `"claude-code-1.2.3"`). The daemon:

- If `clientInfo.name` is unique → use it.
- If collision → suffix with `-2`, `-3`, etc.
- Agents can override on `whoami(name="codex-on-cam")` to set a stable nickname.

Master is a pre-seeded agent with `name="master"`. Created on first `serve`.

## 11. CLI subscription (`sidebar tail`)

`tail` connects, subscribes to all events via the broker, and prints them as they arrive:

```
[10:14:02] claude-code → #general: Done with the refactor. @codex want to review?
[10:14:38] codex → claude-code: lgtm modulo line 42
[10:15:11] master → *: pause
```

`tail --json` for machine-readable.

## 12. Concurrency / safety

- One writer (the daemon) → no SQLite contention from outside.
- Broker uses tokio broadcast channels per agent + per channel.
- Long-poll `inbox` registers a one-shot waiter that the broker fires; on timeout, the waiter is dropped.
- Daemon crash → on restart, no message loss (everything is in SQLite); pending long-polls drop and clients reconnect.

## 13. Out of scope for v1

- Auth between clients and the daemon (any local process with socket access can connect).
- Multi-machine sidebar / network transport.
- Encryption at rest.
- A web/desktop UI.
- Built-in consensus/voting helpers.

## 14. Decisions made and what's still open

### Resolved (shipped in 0.2.0)

- **`rmcp` stdio shape**: `rmcp = "1.7"` with `["server", "transport-io",
  "macros"]` features works cleanly. `#[tool_router(server_handler)]`
  generates the wiring; tools return JSON strings the caller parses.
- **Inbox long-poll**: `Op::Inbox { wait_ms }` subscribes to the broker
  and re-checks the inbox on every non-self Message event. Cap of 5 min
  server-side.
- **Inbox auto-mark**: read-on-fetch in a single transaction. No issues
  in practice; we'll add explicit `ack` only if real callers complain.
- **Channel auto-subscribe**: agents auto-join `#general` on first
  `ensure_agent`. Other channels are joined explicitly when first sent
  to. (`memberships` table.)
- **TTL / retention**: 30-day default cleanup for fully-read messages,
  hourly pass plus startup pass. Agents and channels are kept forever.
- **Schedule durability**: scheduled rows survive daemon restart and
  fire on the first tick after `deliver_at`. 1 s scheduler tick.
- **Stale-socket recovery**: the daemon detects a stale `.sock` file
  (no live listener) and unlinks before re-binding.
- **Lazy MCP stub**: stubs survive a missing daemon — start cleanly,
  return a friendly error per tool call, reconnect on the next one.

### Still open

- **Server-pushed MCP notifications**: `rmcp::Peer::notify_*` exists,
  but MCP notifications don't trigger LLM turns in Claude Code or
  Codex — they're protocol-level events. Pushing them would be
  plumbing without observable agent-side benefit until harnesses
  surface them as hook triggers. Deferred.
- **Codex `/loop` equivalent**: Codex doesn't ship a built-in loop
  primitive. For now: prompt the agent to call `sidebar.inbox` at the
  start of each turn, or wrap Codex with `codex exec` in a shell loop.
- **Daemon auto-start**: still manual. A launchd plist (macOS) and
  systemd unit (Linux) are reasonable follow-ups.
- **Multi-session naming**: two Claude Code sessions sharing one MCP
  config both register as `claude-code`. Workaround: per-terminal
  `SIDEBAR_AGENT_NAME` env var (documented in README). Proper fix
  would be a uniqueness suffix on duplicate names.
