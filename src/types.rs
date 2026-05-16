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
    /// Parse the wire `to` field. Trims outer whitespace and the inner
    /// name after the `@` or `#` prefix.
    /// - `@name`   → Agent
    /// - `#name`   → Channel
    /// - `*`       → Broadcast
    /// - bare name → Agent (forgiving)
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if s == "*" {
            Self::Broadcast
        } else if let Some(rest) = s.strip_prefix('@') {
            Self::Agent(rest.trim().to_string())
        } else if let Some(rest) = s.strip_prefix('#') {
            Self::Channel(rest.trim().to_string())
        } else {
            Self::Agent(s.to_string())
        }
    }

    /// Reject empty names or names containing whitespace. Broadcast is
    /// always valid. Returns the offending value for error messages.
    pub fn validate(&self) -> Result<(), &'static str> {
        let name = match self {
            Self::Agent(n) | Self::Channel(n) => n,
            Self::Broadcast => return Ok(()),
        };
        validate_name(name)
    }
}

/// Shared name validator. Used by `Recipient::validate` and channel /
/// agent ops that don't go through `Recipient`.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name must not be empty");
    }
    if name.chars().any(char::is_whitespace) {
        return Err("name must not contain whitespace");
    }
    Ok(())
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
    fn trims_surrounding_whitespace() {
        assert!(matches!(Recipient::parse("  @alice  "), Recipient::Agent(n) if n == "alice"));
        assert!(matches!(Recipient::parse("  #foo  "), Recipient::Channel(n) if n == "foo"));
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(Recipient::Agent(String::new()).validate().is_err());
        assert!(Recipient::Channel(String::new()).validate().is_err());
        assert!(Recipient::Broadcast.validate().is_ok());
    }

    #[test]
    fn validate_rejects_whitespace_inside() {
        assert!(Recipient::Agent("hi there".into()).validate().is_err());
    }

    #[test]
    fn validate_accepts_dashes_and_underscores() {
        assert!(Recipient::Agent("claude-code-2".into()).validate().is_ok());
        assert!(Recipient::Agent("bob_jr".into()).validate().is_ok());
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
