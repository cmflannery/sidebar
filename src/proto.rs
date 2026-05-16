//! Wire protocol between the daemon and its clients (MCP stubs + CLI).
//!
//! Length-prefixed NDJSON over a unix domain socket. Frames in this module
//! are the JSON shapes serialized on the wire. See ARCHITECTURE.md §5.

#![allow(dead_code)] // skeleton; wired up incrementally as features land

use serde::{Deserialize, Serialize};

use chrono::{DateTime, Utc};

use crate::types::{Intent, Message, Recipient};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hello", rename_all = "lowercase")]
pub enum Hello {
    Mcp {
        agent: String,
        version: String,
    },
    Cli {
        #[serde(rename = "as")]
        speaking_as: String,
    },
}

/// Acknowledges a Hello and tells the client which name the daemon
/// actually assigned. For MCP clients, this can be a suffixed version
/// of the requested name when another session already holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Send {
        to: String,
        body: String,
        #[serde(default)]
        intent: Option<Intent>,
        #[serde(default)]
        reply_to: Option<i64>,
    },
    Inbox {
        #[serde(default)]
        wait_ms: Option<u64>,
    },
    History {
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        with: Option<String>,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    Schedule {
        to: String,
        body: String,
        #[serde(flatten)]
        when: When,
    },
    Participants,
    /// Richer agent listing with first_seen / last_seen timestamps. Defaults
    /// to hiding agents not seen in `stale_threshold_seconds`.
    Agents {
        #[serde(default)]
        include_stale: bool,
    },
    Channels,
    Pause,
    Resume,
    /// Switch this connection into event-forwarding mode. Any subsequent
    /// `Event::*` frames pushed by the daemon will arrive on this socket.
    Subscribe,
    /// Snapshot of daemon health/state for `sidebar status`.
    Status,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum When {
    DelaySeconds { delay_seconds: u64 },
    At { at: chrono::DateTime<chrono::Utc> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseData {
    SendOk { message_id: i64 },
    Messages { messages: Vec<Message> },
    Agents { agents: Vec<String> },
    AgentsDetailed { agents_detailed: Vec<AgentDetails> },
    Channels { channels: Vec<String> },
    Status(StatusInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDetails {
    pub name: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub paused: bool,
    pub agent_count: i64,
    pub channel_count: i64,
    pub unread_count: i64,
    pub pending_scheduled: i64,
    pub uptime_seconds: i64,
    pub db_path: String,
    pub socket_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Message {
        to: Recipient,
        from: String,
        body: String,
        message_id: i64,
    },
    Paused,
    Resumed,
}
