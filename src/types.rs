#![allow(dead_code)] // skeleton; wired up incrementally as features land

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub display_name: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub from: String,
    pub to: Recipient,
    pub body: String,
    pub intent: Option<Intent>,
    pub reply_to: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "lowercase")]
pub enum Recipient {
    Agent(String),
    Channel(String),
    Broadcast,
}

impl Recipient {
    /// Parse the wire `to` field.
    /// - `@name`   → Agent
    /// - `#name`   → Channel
    /// - `*`       → Broadcast
    /// - bare name → Agent (forgiving)
    pub fn parse(s: &str) -> Self {
        if s == "*" {
            Self::Broadcast
        } else if let Some(rest) = s.strip_prefix('@') {
            Self::Agent(rest.to_string())
        } else if let Some(rest) = s.strip_prefix('#') {
            Self::Channel(rest.to_string())
        } else {
            Self::Agent(s.to_string())
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Fyi,
    Question,
    Task,
    Handoff,
}
