//! MCP stdio stub. Translates MCP tool calls into daemon socket ops.
//!
//! Wired into Claude Code / Codex via `claude mcp add sidebar -- sidebar mcp --as <name>`.

use std::sync::Arc;

use anyhow::Result;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, ListPromptsResult, PaginatedRequestParams,
    PromptMessage, PromptMessageRole,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{
    RoleServer, ServerHandler, ServiceExt, prompt, prompt_handler, prompt_router, schemars, tool,
    tool_handler, tool_router,
};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::client::Client;
use crate::proto::{Op, ResponseData, When};
use crate::types::Recipient;

#[derive(Clone)]
struct SidebarMcp {
    agent_name: Arc<Mutex<String>>,
    /// Lazy connection. `None` means "not yet connected" or "previously dropped
    /// after a failure". Each tool call re-establishes if needed; this lets
    /// the stub survive a daemon restart instead of dying on stdin EOF.
    client: Arc<Mutex<Option<Client>>>,
    prompt_router: PromptRouter<Self>,
}

// ---- tool arg types ----

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SendArgs {
    /// Recipient: `@name` for DM, `#channel` for channel, `*` for broadcast.
    to: String,
    /// Message body.
    body: String,
    /// Optional intent label: fyi | question | task | handoff.
    #[serde(default)]
    intent: Option<String>,
    /// Optional message id this is replying to.
    #[serde(default)]
    reply_to: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InboxArgs {
    /// Long-poll wait in milliseconds. If set and the inbox is empty, the
    /// call blocks up to this many milliseconds waiting for a new message
    /// addressed to the calling agent. Capped server-side at 5 minutes.
    #[serde(default)]
    wait_ms: Option<u64>,
    /// When true, return only messages explicitly addressed to this agent:
    /// DMs and channel/broadcast messages that @-mention them by name.
    /// Other unread messages stay unread for a later non-filtered call.
    #[serde(default)]
    mentions_only: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HistoryArgs {
    /// Channel name to scope history to (without `#`).
    #[serde(default)]
    channel: Option<String>,
    /// Other agent for DM-thread history (without `@`).
    #[serde(default)]
    with: Option<String>,
    /// Maximum messages to return.
    #[serde(default = "default_history_limit")]
    limit: usize,
}

fn default_history_limit() -> usize {
    50
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChannelsArg {
    /// One or more channel names. Leading `#` on any name is tolerated.
    channels: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// Substring to look for (case-insensitive).
    query: String,
    /// Maximum results.
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    50
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CancelArgs {
    /// Id of the scheduled row to cancel.
    scheduled_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScheduleArgs {
    /// Recipient: `@agent`, `#channel`, or `*`.
    to: String,
    /// Body.
    body: String,
    /// Send this many seconds from now. Either `delay_seconds` or `at` must be set.
    #[serde(default)]
    delay_seconds: Option<u64>,
    /// Send at this ISO-8601 UTC timestamp.
    #[serde(default)]
    at: Option<String>,
}

#[tool_router]
impl SidebarMcp {
    #[tool(description = "Returns the calling agent's registered name in sidebar.")]
    async fn whoami(&self) -> String {
        self.agent_name.lock().await.clone()
    }

    #[tool(description = "Send a message. `to` is `@agent`, `#channel`, or `*` for broadcast.")]
    async fn send(&self, Parameters(args): Parameters<SendArgs>) -> String {
        let intent = args.intent.as_deref().and_then(parse_intent);
        self.call(Op::Send {
            to: args.to,
            body: args.body,
            intent,
            reply_to: args.reply_to,
        })
        .await
    }

    #[tool(
        description = "Read unread messages for the calling agent (oldest first, up to 500 per call). Marks the returned subset as read; call again if 500 came back to drain the rest. Pass `wait_ms` to long-poll when the inbox is empty."
    )]
    async fn inbox(&self, Parameters(args): Parameters<InboxArgs>) -> String {
        self.call(Op::Inbox {
            wait_ms: args.wait_ms,
            mentions_only: args.mentions_only,
        })
        .await
    }

    #[tool(description = "Read recent messages from a channel or DM thread.")]
    async fn history(&self, Parameters(args): Parameters<HistoryArgs>) -> String {
        self.call(Op::History {
            channel: args.channel,
            with: args.with,
            limit: args.limit,
        })
        .await
    }

    #[tool(description = "List known agents.")]
    async fn participants(&self) -> String {
        self.call(Op::Participants).await
    }

    #[tool(description = "List known channels.")]
    async fn channels(&self) -> String {
        self.call(Op::Channels).await
    }

    #[tool(
        description = "Subscribe the calling agent to one or more channels. Pass `channels` as an array of names (leading `#` tolerated). Auto-creates any channel that doesn't exist."
    )]
    async fn join(&self, Parameters(args): Parameters<ChannelsArg>) -> String {
        for raw in &args.channels {
            let channel = raw.trim_start_matches('#').to_string();
            let r = self.call(Op::Join { channel }).await;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r) {
                if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                    return r;
                }
            }
        }
        serde_json::json!({ "ok": true, "joined": args.channels.len() }).to_string()
    }

    #[tool(description = "Unsubscribe the calling agent from one or more channels.")]
    async fn leave(&self, Parameters(args): Parameters<ChannelsArg>) -> String {
        for raw in &args.channels {
            let channel = raw.trim_start_matches('#').to_string();
            let r = self.call(Op::Leave { channel }).await;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r) {
                if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
                    return r;
                }
            }
        }
        serde_json::json!({ "ok": true, "left": args.channels.len() }).to_string()
    }

    #[tool(
        description = "Case-insensitive substring search across all message bodies. Returns newest matches first."
    )]
    async fn search(&self, Parameters(args): Parameters<SearchArgs>) -> String {
        self.call(Op::Search {
            query: args.query,
            limit: args.limit,
        })
        .await
    }

    #[tool(
        description = "List the calling agent's pending scheduled messages with id, deliver_at, recipient, and body. Use `cancel` to undo one before it fires."
    )]
    async fn scheduled(&self) -> String {
        self.call(Op::Scheduled).await
    }

    #[tool(
        description = "Cancel a pending scheduled message by its id (from `scheduled` or the response of `schedule`). Only the caller who scheduled it can cancel."
    )]
    async fn cancel(&self, Parameters(args): Parameters<CancelArgs>) -> String {
        self.call(Op::Cancel {
            scheduled_id: args.scheduled_id,
        })
        .await
    }

    #[tool(
        description = "Schedule a delayed send. Provide either `delay_seconds` or `at` (ISO-8601 UTC)."
    )]
    async fn schedule(&self, Parameters(args): Parameters<ScheduleArgs>) -> String {
        let when = match (args.delay_seconds, args.at.as_deref()) {
            (Some(s), None) => When::DelaySeconds { delay_seconds: s },
            (None, Some(at)) => match chrono::DateTime::parse_from_rfc3339(at) {
                Ok(ts) => When::At {
                    at: ts.with_timezone(&chrono::Utc),
                },
                Err(e) => {
                    return serde_json::json!({
                        "ok": false,
                        "error": format!("invalid `at` timestamp: {e}")
                    })
                    .to_string();
                }
            },
            (Some(_), Some(_)) => {
                return serde_json::json!({
                    "ok": false,
                    "error": "provide either `delay_seconds` or `at`, not both"
                })
                .to_string();
            }
            (None, None) => {
                return serde_json::json!({
                    "ok": false,
                    "error": "either `delay_seconds` or `at` is required"
                })
                .to_string();
            }
        };
        self.call(Op::Schedule {
            to: args.to,
            body: args.body,
            when,
        })
        .await
    }
}

// ---- prompts ----
//
// Exposed via MCP so `claude mcp add sidebar -- sidebar mcp` makes
// `/mcp__sidebar__sidebar-start` available with no extra file copying.

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SidebarPollArgs {
    /// Polling interval — `5` or `5m` for minutes, `30s` for seconds,
    /// `1h` for hours. Bare integers are treated as minutes. Max 1 hour.
    interval: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SidebarListenArgs {
    /// Long-poll wait per inbox call — `30s`, `1m`, `5m`. Bare integers are
    /// treated as minutes. Capped at 5 minutes (server-side inbox limit).
    wait: String,
}

#[prompt_router(router = "prompt_router")]
impl SidebarMcp {
    #[prompt(
        name = "sidebar-start",
        description = "Bootstrap a sidebar session (whoami + participants), then check inbox at the top of every turn. No arguments."
    )]
    async fn sidebar_start(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            PromptMessageRole::User,
            SIDEBAR_START_PER_TURN,
        )]
    }

    #[prompt(
        name = "sidebar-poll",
        description = "Claude Code: arm a recurring inbox check via ScheduleWakeup. Pass an interval like `5` (minutes), `30s`, `5m`, `1h`."
    )]
    async fn sidebar_poll(
        &self,
        Parameters(args): Parameters<SidebarPollArgs>,
    ) -> Vec<PromptMessage> {
        let raw = args.interval.trim();
        let body = match parse_interval(raw) {
            Some((human, secs)) => render_sidebar_poll(&human, secs),
            None => format!(
                "The interval `{raw}` isn't a recognized format. Use values like `5` (minutes), `30s`, `5m`, `1h` (max 1 hour). Ask the user to re-run with a valid interval; do not call any tools or `ScheduleWakeup`."
            ),
        };
        vec![PromptMessage::new_text(PromptMessageRole::User, body)]
    }

    #[prompt(
        name = "sidebar-listen",
        description = "Codex / any-MCP-client: stay attentive by long-polling the inbox in a loop. Pass a wait like `1m`, `5m`. Doesn't need ScheduleWakeup."
    )]
    async fn sidebar_listen(
        &self,
        Parameters(args): Parameters<SidebarListenArgs>,
    ) -> Vec<PromptMessage> {
        let raw = args.wait.trim();
        let body = match parse_interval(raw) {
            // Server caps inbox wait_ms at 5 minutes; reject anything larger here
            // so the agent doesn't waste a tool call on a value the daemon would
            // truncate silently.
            Some((_, secs)) if secs > 300 => format!(
                "Wait `{raw}` exceeds the 5-minute cap on `mcp__sidebar__inbox` long-polls. Use `5m` or smaller. Ask the user to re-run; do not call any tools."
            ),
            Some((human, secs)) => render_sidebar_listen(&human, secs),
            None => format!(
                "The wait `{raw}` isn't a recognized format. Use values like `30s`, `1m`, `5m` (max 5 minutes). Ask the user to re-run with a valid wait; do not call any tools."
            ),
        };
        vec![PromptMessage::new_text(PromptMessageRole::User, body)]
    }
}

const SIDEBAR_START_PER_TURN: &str = "You are starting a sidebar session in this Claude Code window.\n\
\n\
Do these in one short message:\n\
1. Call `mcp__sidebar__whoami` to confirm your name.\n\
2. Call `mcp__sidebar__participants` to see who else is around.\n\
3. Tell the user: who you are, who else is here, and that you're in per-turn mode (no scheduled polling).\n\
\n\
For every turn from now on, before doing anything else, call `mcp__sidebar__inbox` (no `wait_ms`). For messages addressed to you (DMs, or `@<your-name>` in a channel/broadcast), respond concisely. If empty, continue with whatever the user asked.";

fn render_sidebar_poll(human: &str, seconds: u64) -> String {
    format!(
        "You are running a sidebar session with a scheduled inbox poll every {human}.\n\
\n\
If this is the first call (no prior `whoami` yet), do these in one short message:\n\
1. Call `mcp__sidebar__whoami` to confirm your name.\n\
2. Call `mcp__sidebar__participants` to see who else is around.\n\
3. Tell the user you'll poll every {human}.\n\
\n\
On every fire (including this one):\n\
1. Call `mcp__sidebar__inbox` (no `wait_ms`).\n\
2. For messages addressed to you (DMs or `@<your-name>` in channel/broadcast), respond concisely. Stay quiet on other channel chatter unless it changes something the user cares about.\n\
3. Call `ScheduleWakeup` with:\n\
   - `delaySeconds`: {seconds}\n\
   - `prompt`: `/mcp__sidebar__sidebar-poll {human}`\n\
   - `reason`: `sidebar inbox poll`\n\
\n\
Stopping: if the user has just told you to \"stop checking sidebar\", \"stop the sidebar loop\", or \"pause sidebar polling\", do not call `ScheduleWakeup` on this fire. The loop dies because nothing re-armed it.\n\
\n\
If `mcp__sidebar__inbox` returns `{{\"ok\": false, ...}}`, mention the error briefly and still re-arm — the daemon may have restarted and the next fire will likely succeed."
    )
}

fn render_sidebar_listen(human: &str, seconds: u64) -> String {
    let wait_ms = seconds * 1000;
    format!(
        "You are running a sidebar listen session — staying attentive by long-polling the inbox.\n\
\n\
If this is the first call (no prior `whoami` yet), do these in one short message:\n\
1. Call `mcp__sidebar__whoami` to confirm your name.\n\
2. Call `mcp__sidebar__participants` to see who else is around.\n\
3. Tell the user you're listening with {human} long-polls.\n\
\n\
Then enter the listen loop:\n\
1. Call `mcp__sidebar__inbox` with `wait_ms`: {wait_ms}. The call blocks up to {human}.\n\
2. When it returns:\n\
   - For messages addressed to you (DMs, or `@<your-name>` in channel/broadcast), respond concisely. Stay quiet on other channel chatter unless it changes something the user cares about.\n\
   - If `messages` is empty (poll timed out), that's fine — just proceed.\n\
3. Immediately call `mcp__sidebar__inbox` again with the same `wait_ms`. Repeat until either:\n\
   - The user tells you to stop (\"stop listening\", \"stop checking sidebar\", \"that's enough\").\n\
   - Your turn budget runs out — the user can re-invoke `/mcp__sidebar__sidebar-listen {human}` to resume.\n\
\n\
This pattern doesn't need `ScheduleWakeup`; it works in Codex, Claude Code, and any other MCP-compatible client. If `inbox` returns `{{\"ok\": false, ...}}` (e.g. daemon restarted), mention the error briefly and keep looping — the next call will likely succeed."
    )
}

/// Parse `5`, `5m`, `30s`, `1h` etc. into (canonical-form, seconds). Bare
/// integers are treated as minutes. Caps at 1 hour. Rejects 0.
fn parse_interval(s: &str) -> Option<(String, u64)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let last = s.chars().last()?;
    let (num_str, unit) = if last.is_ascii_digit() {
        (s, 'm')
    } else {
        (&s[..s.len() - last.len_utf8()], last)
    };
    let num: u64 = num_str.parse().ok()?;
    let secs = match unit {
        's' => num,
        'm' => num.checked_mul(60)?,
        'h' => num.checked_mul(3600)?,
        _ => return None,
    };
    if secs == 0 || secs > 3600 {
        return None;
    }
    Some((format!("{num}{unit}"), secs))
}

// ---- ServerHandler ----

#[tool_handler]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for SidebarMcp {}

impl SidebarMcp {
    /// Send an Op to the daemon and return a JSON-encoded string the caller
    /// can parse. On any error (daemon down, serialization, etc.) returns a
    /// shape like `{"ok":false,"error":"..."}` so callers get a consistent
    /// envelope.
    async fn call(&self, op: Op) -> String {
        match self.call_inner(op).await {
            Ok(value) => value.to_string(),
            Err(e) => err_json(&e.to_string()),
        }
    }

    async fn call_inner(&self, op: Op) -> Result<serde_json::Value> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            // Reconnecting after a drop — we want the daemon to assign us
            // the same name we held before if it's free, so pass our
            // current `agent_name` (which may itself be a suffixed form).
            let requested_name = self.agent_name.lock().await.clone();
            let client = Client::connect_mcp(&requested_name, env!("CARGO_PKG_VERSION"))
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "sidebar daemon not reachable: {e}. Start it with `sidebar serve`."
                    )
                })?;
            *self.agent_name.lock().await = client.assigned_name().to_string();
            *guard = Some(client);
        }
        let client = guard.as_mut().expect("connected above");
        let resp = match client.request(op).await {
            Ok(resp) => resp,
            Err(e) => {
                // The daemon may have restarted or the socket may have been
                // closed while this request was in flight. Drop the stale
                // client so the next tool call establishes a fresh session.
                *guard = None;
                return Err(e);
            }
        };
        if !resp.ok {
            anyhow::bail!(resp.error.unwrap_or_else(|| "unknown daemon error".into()));
        }
        Ok(format_response_data(resp.data.as_ref()))
    }
}

/// Render a `ResponseData` as the JSON shape that tool callers see.
fn format_response_data(data: Option<&ResponseData>) -> serde_json::Value {
    match data {
        Some(ResponseData::SendOk { message_id }) => {
            serde_json::json!({ "ok": true, "message_id": message_id })
        }
        Some(ResponseData::Messages { messages }) => serde_json::json!({
            "ok": true,
            "messages": messages.iter().map(format_message).collect::<Vec<_>>()
        }),
        Some(ResponseData::Agents { agents }) => {
            serde_json::json!({ "ok": true, "agents": agents })
        }
        Some(ResponseData::AgentsDetailed { agents_detailed }) => {
            serde_json::json!({ "ok": true, "agents": agents_detailed })
        }
        Some(ResponseData::Channels { channels }) => {
            serde_json::json!({ "ok": true, "channels": channels })
        }
        Some(ResponseData::ChannelsDetailed { channels_detailed }) => {
            serde_json::json!({ "ok": true, "channels": channels_detailed })
        }
        Some(ResponseData::Status(s)) => serde_json::json!({ "ok": true, "status": s }),
        Some(ResponseData::Scheduled { scheduled }) => {
            serde_json::json!({ "ok": true, "scheduled": scheduled })
        }
        Some(ResponseData::MessageDetail(_)) => {
            // Not exposed as an MCP tool; agents shouldn't surveil each
            // other's read state. This arm exists for exhaustiveness only.
            serde_json::json!({
                "ok": false,
                "error": "inspect is not exposed to agents",
            })
        }
        None => serde_json::json!({ "ok": true }),
    }
}

fn err_json(msg: &str) -> String {
    // Round-trip through serde to escape correctly; fallback is well-formed JSON.
    serde_json::json!({ "ok": false, "error": msg }).to_string()
}

fn format_message(m: &crate::types::Message) -> serde_json::Value {
    let to = match &m.to {
        Recipient::Agent(n) => format!("@{n}"),
        Recipient::Channel(n) => format!("#{n}"),
        Recipient::Broadcast => "*".to_string(),
    };
    serde_json::json!({
        "id": m.id,
        "from": m.from,
        "to": to,
        "body": m.body,
        "intent": m.intent.as_ref().map(|i| match i {
            crate::types::Intent::Fyi => "fyi",
            crate::types::Intent::Question => "question",
            crate::types::Intent::Task => "task",
            crate::types::Intent::Handoff => "handoff",
        }),
        "reply_to": m.reply_to,
        "created_at": m.created_at.to_rfc3339(),
    })
}

fn parse_intent(s: &str) -> Option<crate::types::Intent> {
    match s {
        "fyi" => Some(crate::types::Intent::Fyi),
        "question" => Some(crate::types::Intent::Question),
        "task" => Some(crate::types::Intent::Task),
        "handoff" => Some(crate::types::Intent::Handoff),
        _ => None,
    }
}

pub async fn serve(requested_name: String) -> Result<()> {
    // Best-effort eager connect — if the daemon's up, we register a session
    // immediately so the agent appears in `sidebar participants` and we
    // learn the (possibly suffixed) name the daemon assigned us. If the
    // daemon is down, we still start the MCP stub; tool calls retry the
    // connect and return a clean error to Claude/Codex.
    let (client, agent_name) =
        match Client::connect_mcp(&requested_name, env!("CARGO_PKG_VERSION")).await {
            Ok(c) => {
                let assigned = c.assigned_name().to_string();
                (Some(c), assigned)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "could not reach sidebar daemon on startup; will retry per tool call"
                );
                (None, requested_name.clone())
            }
        };
    let server = SidebarMcp {
        agent_name: Arc::new(Mutex::new(agent_name)),
        client: Arc::new(Mutex::new(client)),
        prompt_router: SidebarMcp::prompt_router(),
    };
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
