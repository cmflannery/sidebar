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
use crate::proto::{Op, ResponseData};
use crate::types::Recipient;

#[derive(Clone)]
struct SidebarMcp {
    agent_name: String,
    client: Arc<Mutex<Client>>,
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
    /// Long-poll wait in milliseconds. (Currently ignored; returns immediately.)
    #[serde(default)]
    wait_ms: Option<u64>,
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

    #[tool(description = "Read all unread messages for the calling agent. Marks them as read.")]
    async fn inbox(&self, Parameters(args): Parameters<InboxArgs>) -> String {
        self.call(Op::Inbox {
            wait_ms: args.wait_ms,
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
}

impl SidebarMcp {
    /// Send an Op to the daemon over the persistent socket; return a JSON
    /// string the caller can parse, or a human-readable error.
    async fn call(&self, op: Op) -> String {
        let mut client = self.client.lock().await;
        match client.request(op).await {
            Ok(resp) => {
                if resp.ok {
                    match &resp.data {
                        Some(ResponseData::SendOk { message_id }) => {
                            serde_json::json!({ "ok": true, "message_id": message_id }).to_string()
                        }
                        Some(ResponseData::Messages { messages }) => {
                            serde_json::to_string(&serde_json::json!({
                                "ok": true,
                                "messages": messages.iter().map(format_message).collect::<Vec<_>>()
                            }))
                            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{e}\"}}"))
                        }
                        Some(ResponseData::Agents { agents }) => {
                            serde_json::json!({ "ok": true, "agents": agents }).to_string()
                        }
                        Some(ResponseData::Channels { channels }) => {
                            serde_json::json!({ "ok": true, "channels": channels }).to_string()
                        }
                        None => serde_json::json!({ "ok": true }).to_string(),
                    }
                } else {
                    serde_json::json!({
                        "ok": false,
                        "error": resp.error.unwrap_or_else(|| "unknown error".into()),
                    })
                    .to_string()
                }
            }
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }).to_string(),
        }
    }
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

pub async fn serve(agent_name: String) -> Result<()> {
    let client = Client::connect_mcp(&agent_name, env!("CARGO_PKG_VERSION")).await?;
    let server = SidebarMcp {
        agent_name,
        client: Arc::new(Mutex::new(client)),
    };
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
