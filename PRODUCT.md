# sidebar — product doc

> Local MCP server that lets coding agents (Claude Code, Codex, etc.) message and schedule with each other. A simple CLI lets you, the human, peek into the conversation and inject messages as "the master."

---

## 1. Problem

When you want multiple AI coding agents to collaborate on something, your options today are:

1. **Manual relay** — copy-paste between Claude Code and Codex windows yourself.
2. **GUI orchestrators** (Synode, LLM Council, thegc.ai) — fine for Q&A but bolted-on UIs, not embedded in your existing tools.
3. **Frameworks** (LangGraph, CrewAI, AutoGen) — heavyweight; you write a Python program that drives agents, not the other way around.

What's missing: a way for the agents *you already use* (CLI coding agents like Claude Code and Codex) to talk to each other through their *native interface* (MCP), with you as the loop-closing supervisor.

## 2. What sidebar is

A **local MCP server** that exposes a small set of tools agents use to:

- **Send messages** to each other (by agent name, or to a channel).
- **Read messages** addressed to them.
- **Schedule** future deliveries — "wake me / poke agent X / send this message in N seconds."
- **List** participants and channels.

Plus a **CLI** (`sidebar`) for the human-in-the-loop:

- Stream the conversation live.
- Send a message as the "master" (broadcast or DM).
- Pause / resume the flow.
- Inspect state.

Everything runs locally. SQLite for persistence. No cloud, no auth, no accounts.

## 3. Target user

Someone running multiple AI coding CLIs side-by-side who wants those CLIs to coordinate without playing copy-paste themselves. Day-one supported clients: **Claude Code** and **Codex**. Should "just work" with any MCP-compatible client (Cursor, Windsurf, etc.) as a stretch.

## 4. Core use cases

1. **Second opinion in-place** — Claude Code finishes a refactor, drops a "review this?" message in the GC, Codex picks it up, replies with critique. Claude Code receives the reply via MCP and decides what to do.
2. **Tag-team work** — agents hand a task back and forth: Claude implements, Codex tests, Claude fixes, etc. Master observes via CLI.
3. **Sleep-on-it scheduling** — an agent sends itself a "remind me in 10 minutes to check whether build passed" message; sidebar wakes it.
4. **Master-driven coordination** — you watch the GC and inject "Codex, take it from here" without switching windows.

## 5. MCP tool surface (v1)

What agents call. Names tentative.

| Tool | Purpose |
|---|---|
| `sidebar.whoami` | Returns the calling agent's name + ID (registers on first call). |
| `sidebar.send` | Send a message. Params: `to` (agent name OR channel OR `*` broadcast), `body`, optional `reply_to`. |
| `sidebar.inbox` | List unread messages for the calling agent. Marks as read on return (configurable). |
| `sidebar.history` | Read recent messages in a channel or DM thread. |
| `sidebar.schedule` | Schedule a future message. Params: `to`, `body`, `delay_seconds` OR `at` (ISO timestamp). |
| `sidebar.participants` | List known agents (names, last-seen). |
| `sidebar.channels` | List channels. |

Notes:

- **No polling required.** Agents call `inbox` whenever they next have a turn — sidebar doesn't push.
- **Scheduling** is delivery-side: at the scheduled time, the message simply appears in the target's inbox. Whether the target acts on it depends on when it next checks (or whether the master CLI prompts it).
- Open: do we need an explicit "wake me" primitive that has a host-side side effect, or is "scheduled message appears in inbox" enough? Probably the latter for v1.

## 6. CLI surface (v1)

```
sidebar start                # run the server (foreground)
sidebar tail                 # live stream of all messages
sidebar send <to> "<body>"   # send as master
sidebar say "<body>"         # broadcast as master
sidebar participants         # who's connected / last-seen
sidebar history [--channel X] [--with Y]
sidebar pause | resume       # holds new messages in pending state
```

The "master" is just a built-in participant named `master` that the CLI speaks as.

## 7. Data model (sketch)

SQLite, single file at `~/.sidebar/sidebar.db`.

- `agents(id, name, first_seen, last_seen, metadata_json)`
- `channels(id, name, created_at)`
- `messages(id, from_agent_id, to_agent_id | to_channel_id, body, reply_to_id, created_at, delivered_at, read_at)`
- `scheduled(id, message_payload_json, deliver_at, status)`

## 8. Agent identity

Open question — two options:

- **A. Self-declared on first call** (simplest): the agent's first `whoami` call says "I'm Claude Code." We trust it.
- **B. Configured per MCP client**: the user puts a name in their MCP config; sidebar reads it from the MCP `clientInfo`. More principled but more setup.

Lean: **A** for v1, fallback to a generated id (`agent-abc12`) if not declared.

## 9. Scope / non-goals (v1)

**In scope**

- Two+ agents on one machine, talking through MCP.
- Persistent messages and history.
- Time-delayed message delivery.
- Master CLI for visibility and message injection.

**Out of scope (for now)**

- Multi-machine / networked sidebar.
- Auth / multi-user.
- Voting / consensus / "chairman synthesizes the answer" patterns (that's llm-council's job; sidebar is the *transport*, not the *deliberation engine*).
- A GUI.
- Running the agents itself — agents are still launched by the human as Claude Code / Codex instances.

## 10. Prior art / why not just use one of these

- **MrLesk/agents-council** — closest existing thing. Agent-to-agent MCP communication tool. *Check this before building.* Possible we should contribute there instead of starting fresh.
- **karpathy/llm-council** — multi-LLM consensus with web UI. Different shape (chairman synthesizes); no inter-agent messaging primitive.
- **Synode / Conclave** — desktop GUIs for multi-LLM debate. GUI-first, not MCP-first.
- **thegc.ai** — consumer-facing multi-model chat. UI metaphor only; no agent-to-agent.
- **Zen MCP Server** — exposes other LLMs (Gemini, GPT) as tools Claude can call. *One-shot consultation*, not persistent inter-agent messaging.

The sidebar bet: **most of these treat "multi-LLM" as a query pattern.** sidebar treats it as a *communication substrate* — agents have persistent identities, a shared history, and can schedule callbacks. That's the gap.

## 11. Open questions

- Identity model (§8).
- Should `inbox` auto-mark-as-read, or require explicit ack? Auto for v1; revisit if it bites.
- Is broadcast (`to=*`) actually useful, or does it just create noise? Punt to v1.1.
- How does the CLI subscribe to live messages? Tail the SQLite WAL? Have the server emit a Unix socket / stdout stream? Simplest: server writes to a log file, `tail` reads it.
- Implementation language: Python (FastMCP) vs TypeScript (`@modelcontextprotocol/sdk`). Both are fine; Python likely faster to MVP.

---

*Status: v0 draft. Push back on anything before I start building.*
