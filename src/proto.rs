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
        /// When true, only return messages where the calling agent was
        /// explicitly addressed: DMs to them or channel/broadcast messages
        /// where their name appears as an @-mention.
        #[serde(default)]
        mentions_only: bool,
    },
    History {
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        with: Option<String>,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// Channel history with per-recipient delivery state for operator/UI use.
    HistoryDetailed {
        channel: String,
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
    /// Channels with member_count + last_message_at, for `sidebar channels --details`.
    ChannelsDetailed,
    /// Subscribe the calling agent to a channel (without leading `#`).
    /// Creates the channel if it doesn't exist.
    Join {
        channel: String,
    },
    /// Unsubscribe the calling agent from a channel.
    Leave {
        channel: String,
    },
    Pause,
    Resume,
    /// Switch this connection into event-forwarding mode. Any subsequent
    /// `Event::*` frames pushed by the daemon will arrive on this socket.
    Subscribe,
    /// Snapshot of daemon health/state for `sidebar status`.
    Status,
    /// Case-insensitive substring search across message bodies. Returns
    /// matches newest-first, capped at `limit`.
    Search {
        query: String,
        #[serde(default = "default_search_limit")]
        limit: usize,
    },
    /// Delete agents inactive for `inactive_days` who have no messages
    /// either from or to them. When `dry_run` is true, return the agent
    /// names that would be pruned without actually deleting.
    Prune {
        #[serde(default = "default_prune_days")]
        inactive_days: i64,
        #[serde(default)]
        dry_run: bool,
    },
    /// Operator debug: full record for a single message including its
    /// per-recipient delivery state.
    Inspect {
        message_id: i64,
    },
    /// List pending scheduled messages. Master sees all; other callers
    /// see only their own.
    Scheduled,
    /// Cancel a pending scheduled message. Master can cancel any;
    /// non-master agents can only cancel their own.
    Cancel {
        scheduled_id: i64,
    },
}

fn default_prune_days() -> i64 {
    30
}

fn default_search_limit() -> usize {
    50
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
    SendOk {
        message_id: i64,
    },
    Messages {
        messages: Vec<Message>,
    },
    MessagesDetailed {
        messages_detailed: Vec<MessageWithDelivery>,
    },
    Agents {
        agents: Vec<String>,
    },
    AgentsDetailed {
        agents_detailed: Vec<AgentDetails>,
    },
    Channels {
        channels: Vec<String>,
    },
    ChannelsDetailed {
        channels_detailed: Vec<ChannelDetails>,
    },
    Status(StatusInfo),
    Scheduled {
        scheduled: Vec<ScheduledRow>,
    },
    MessageDetail(MessageDetail),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDetail {
    pub message: Message,
    pub deliveries: Vec<MessageDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageWithDelivery {
    pub message: Message,
    pub deliveries: Vec<MessageDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDelivery {
    pub agent: String,
    pub delivered_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledRow {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub body: String,
    pub deliver_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDetails {
    pub name: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub active_sessions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDetails {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub member_count: i64,
    pub last_message_at: Option<DateTime<Utc>>,
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
