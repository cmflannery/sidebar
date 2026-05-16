//! MCP stdio stub. Translates MCP tool calls into daemon socket ops.
//!
//! Wired into Claude Code / Codex via `claude mcp add sidebar -- sidebar mcp --as <name>`.

use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::client::Client;
use crate::proto::{Op, ResponseData, When};
use crate::types::Recipient;

#[derive(Clone)]
struct SidebarMcp {
    agent_name: String,
    /// Lazy connection. `None` means "not yet connected" or "previously dropped
    /// after a failure". Each tool call re-establishes if needed; this lets
    /// the stub survive a daemon restart instead of dying on stdin EOF.
    client: Arc<Mutex<Option<Client>>>,
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
struct ChannelArg {
    /// Channel name. Leading `#` is tolerated.
    channel: String,
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

#[tool_router(server_handler)]
impl SidebarMcp {
    #[tool(description = "Returns the calling agent's registered name in sidebar.")]
    async fn whoami(&self) -> String {
        self.agent_name.clone()
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
        description = "Read all unread messages for the calling agent. Marks them as read. Pass `wait_ms` to long-poll up to that many ms when the inbox is empty."
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
        description = "Subscribe the calling agent to a channel. Pass the name without a leading `#`. Auto-creates the channel if it doesn't exist."
    )]
    async fn join(&self, Parameters(args): Parameters<ChannelArg>) -> String {
        self.call(Op::Join {
            channel: args.channel.trim_start_matches('#').to_string(),
        })
        .await
    }

    #[tool(description = "Unsubscribe the calling agent from a channel.")]
    async fn leave(&self, Parameters(args): Parameters<ChannelArg>) -> String {
        self.call(Op::Leave {
            channel: args.channel.trim_start_matches('#').to_string(),
        })
        .await
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
            let client = Client::connect_mcp(&self.agent_name, env!("CARGO_PKG_VERSION"))
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "sidebar daemon not reachable: {e}. Start it with `sidebar serve`."
                    )
                })?;
            *guard = Some(client);
        }
        let client = guard.as_mut().expect("connected above");
        let resp = client.request(op).await?;
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
        Some(ResponseData::Status(s)) => serde_json::json!({ "ok": true, "status": s }),
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
        agent_name,
        client: Arc::new(Mutex::new(client)),
    };
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
