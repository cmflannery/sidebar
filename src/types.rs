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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_at_as_agent() {
        assert!(matches!(Recipient::parse("@claude"), Recipient::Agent(n) if n == "claude"));
    }

    #[test]
    fn parses_hash_as_channel() {
        assert!(matches!(Recipient::parse("#general"), Recipient::Channel(n) if n == "general"));
    }

    #[test]
    fn parses_star_as_broadcast() {
        assert!(matches!(Recipient::parse("*"), Recipient::Broadcast));
    }

    #[test]
    fn parses_bare_as_agent() {
        assert!(matches!(Recipient::parse("codex"), Recipient::Agent(n) if n == "codex"));
    }

    #[test]
    fn empty_string_is_agent_with_empty_name() {
        // Forgiving — daemon-side validation will reject it.
        assert!(matches!(Recipient::parse(""), Recipient::Agent(n) if n.is_empty()));
    }

    #[test]
    fn round_trips_through_serde() {
        for r in [
            Recipient::Agent("claude".into()),
            Recipient::Channel("general".into()),
            Recipient::Broadcast,
        ] {
            let j = serde_json::to_string(&r).unwrap();
            let back: Recipient = serde_json::from_str(&j).unwrap();
            assert_eq!(format!("{r:?}"), format!("{back:?}"));
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
