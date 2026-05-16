//! SQLite-backed persistent store for sidebar.
//!
//! rusqlite is sync; the store wraps a single `Connection` in a `tokio::sync::Mutex`
//! and routes blocking work through `spawn_blocking`. This is fine for the
//! handful-of-agents v1 workload; swap in a pool (`deadpool-sqlite`) if it bites.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;

use crate::types::{Intent, Message, Recipient};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = include_str!("schema.sql");

/// Default retention for messages whose deliveries are all read.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;
/// Channel everyone joins on first sight.
pub const DEFAULT_CHANNEL: &str = "general";

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // fields surface in richer responses later
pub struct AgentRow {
    pub id: i64,
    pub name: String,
    pub display_name: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Persisted payload for a scheduled send. Stored as JSON in `scheduled.payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScheduledPayload {
    from: String,
    to: String,
    body: String,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    reply_to: Option<i64>,
}

/// One scheduled send that has just been delivered. The daemon uses this to
/// publish events to subscribers.
#[derive(Debug, Clone)]
pub struct DeliveredScheduled {
    pub message_id: i64,
    pub from: String,
    pub to: Recipient,
    pub body: String,
}

impl Store {
    /// Open (or create) the database at `path`, run schema, and seed the
    /// `master` agent if absent.
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let conn = Connection::open(&path)
                .with_context(|| format!("opening sqlite at {}", path.display()))?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.execute_batch(SCHEMA).context("applying schema")?;
            seed_defaults(&conn).context("seeding defaults")?;
            Ok(conn)
        })
        .await??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Ensure an agent row exists for `name`, updating `last_seen`. Auto-joins
    /// the agent to `#general` if newly created. Returns the agent id.
    pub async fn ensure_agent(&self, name: &str) -> Result<i64> {
        let conn = self.conn.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let conn = conn.blocking_lock();
            ensure_agent_blocking(&conn, &name)
        })
        .await?
    }

    /// List agents with rich detail (first_seen, last_seen). If `include_stale`
    /// is false, hides agents whose `last_seen` is older than `stale_after`.
    pub async fn list_agents_detailed(
        &self,
        include_stale: bool,
        stale_after: chrono::Duration,
    ) -> Result<Vec<crate::proto::AgentDetails>> {
        let conn = self.conn.clone();
        let cutoff = (Utc::now() - stale_after).to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<Vec<crate::proto::AgentDetails>> {
            let conn = conn.blocking_lock();
            let (sql, params): (&str, Vec<&dyn rusqlite::ToSql>) = if include_stale {
                (
                    "SELECT name, first_seen, last_seen FROM agents ORDER BY last_seen DESC",
                    vec![],
                )
            } else {
                (
                    "SELECT name, first_seen, last_seen FROM agents
                     WHERE last_seen >= ?1 ORDER BY last_seen DESC",
                    vec![&cutoff],
                )
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    Ok(crate::proto::AgentDetails {
                        name: r.get(0)?,
                        first_seen: parse_ts(&r.get::<_, String>(1)?),
                        last_seen: parse_ts(&r.get::<_, String>(2)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?
    }

    /// List all known agents.
    pub async fn list_agents(&self) -> Result<Vec<AgentRow>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<AgentRow>> {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare(
                "SELECT id, name, display_name, first_seen, last_seen FROM agents ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(AgentRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        display_name: r.get(2)?,
                        first_seen: parse_ts(&r.get::<_, String>(3)?),
                        last_seen: parse_ts(&r.get::<_, String>(4)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?
    }

    /// List channel names.
    pub async fn list_channels(&self) -> Result<Vec<String>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare("SELECT name FROM channels ORDER BY id")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?
    }

    /// Insert a message + per-recipient delivery rows in one transaction.
    /// Returns the new message id.
    pub async fn send_message(
        &self,
        from_name: &str,
        to: &Recipient,
        body: &str,
        intent: Option<Intent>,
        reply_to: Option<i64>,
    ) -> Result<i64> {
        let conn = self.conn.clone();
        let from_name = from_name.to_string();
        let to = to.clone();
        let body = body.to_string();
        let now = Utc::now().to_rfc3339();

        tokio::task::spawn_blocking(move || -> Result<i64> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;

            let from_id = ensure_agent_blocking(&tx, &from_name)?;

            let (to_agent, to_channel, is_broadcast) = match &to {
                Recipient::Agent(name) => {
                    let id = ensure_agent_blocking(&tx, name)?;
                    (Some(id), None, false)
                }
                Recipient::Channel(name) => {
                    let id = ensure_channel_blocking(&tx, name)?;
                    (None, Some(id), false)
                }
                Recipient::Broadcast => (None, None, true),
            };

            let intent_str = intent.map(intent_to_str);

            tx.execute(
                "INSERT INTO messages (from_agent, to_agent, to_channel, is_broadcast, body, intent, reply_to, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![from_id, to_agent, to_channel, i64::from(is_broadcast), body, intent_str, reply_to, now],
            )?;
            let msg_id = tx.last_insert_rowid();

            let mut recipient_ids: Vec<i64> = match &to {
                Recipient::Agent(_) => vec![to_agent.expect("agent id set above")],
                Recipient::Channel(_) => {
                    let cid = to_channel.expect("channel id set above");
                    let mut stmt = tx.prepare(
                        "SELECT a.id FROM agents a
                         JOIN memberships m ON m.agent_id = a.id
                         WHERE m.channel_id = ?1 AND a.id != ?2",
                    )?;
                    stmt.query_map(params![cid, from_id], |r| r.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                }
                Recipient::Broadcast => {
                    let mut stmt = tx.prepare("SELECT id FROM agents WHERE id != ?1")?;
                    stmt.query_map(params![from_id], |r| r.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                }
            };

            // Mention expansion: for channel and broadcast sends, treat
            // `@name` tokens in the body as additional recipients. This
            // lets agents @-ping someone who isn't subscribed to the
            // channel. DM mentions are redundant (the target already gets
            // it) so we skip there. The recipient agent is created on the
            // fly if it doesn't exist yet — same affordance as `send @new`.
            if !matches!(&to, Recipient::Agent(_)) {
                for name in extract_mentions(&body) {
                    let mid = ensure_agent_blocking(&tx, &name)?;
                    if mid != from_id && !recipient_ids.contains(&mid) {
                        recipient_ids.push(mid);
                    }
                }
            }

            for aid in &recipient_ids {
                tx.execute(
                    "INSERT OR IGNORE INTO deliveries (message_id, agent_id, delivered_at)
                     VALUES (?1, ?2, ?3)",
                    params![msg_id, aid, now],
                )?;
            }

            tx.commit()?;
            Ok(msg_id)
        })
        .await?
    }

    /// Read unread messages for `agent_name`, marking them read in the same
    /// transaction. Returns messages oldest-first.
    pub async fn fetch_inbox(&self, agent_name: &str) -> Result<Vec<Message>> {
        let conn = self.conn.clone();
        let agent_name = agent_name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;
            let agent_id = ensure_agent_blocking(&tx, &agent_name)?;
            let now = Utc::now().to_rfc3339();

            let messages = {
                let mut stmt = tx.prepare(
                    "SELECT m.id, fa.name AS from_name,
                            ta.name AS to_agent, tc.name AS to_channel, m.is_broadcast,
                            m.body, m.intent, m.reply_to, m.created_at
                     FROM messages m
                     JOIN agents fa ON fa.id = m.from_agent
                     LEFT JOIN agents ta ON ta.id = m.to_agent
                     LEFT JOIN channels tc ON tc.id = m.to_channel
                     JOIN deliveries d ON d.message_id = m.id
                     WHERE d.agent_id = ?1 AND d.read_at IS NULL
                     ORDER BY m.created_at ASC",
                )?;
                stmt.query_map(params![agent_id], row_to_message)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };

            tx.execute(
                "UPDATE deliveries SET read_at = ?1
                 WHERE agent_id = ?2 AND read_at IS NULL",
                params![now, agent_id],
            )?;
            tx.commit()?;
            Ok(messages)
        })
        .await?
    }

    /// History within a channel (newest-last, oldest-first, capped to `limit`).
    pub async fn history_channel(&self, channel_name: &str, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn.clone();
        let channel_name = channel_name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>> {
            let conn = conn.blocking_lock();
            let cid: Option<i64> = conn
                .query_row(
                    "SELECT id FROM channels WHERE name = ?1",
                    params![channel_name],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(cid) = cid else { return Ok(vec![]) };

            let lim = i64::try_from(limit).unwrap_or(i64::MAX);
            let mut stmt = conn.prepare(
                "SELECT m.id, fa.name AS from_name,
                        ta.name AS to_agent, tc.name AS to_channel, m.is_broadcast,
                        m.body, m.intent, m.reply_to, m.created_at
                 FROM messages m
                 JOIN agents fa ON fa.id = m.from_agent
                 LEFT JOIN agents ta ON ta.id = m.to_agent
                 LEFT JOIN channels tc ON tc.id = m.to_channel
                 WHERE m.to_channel = ?1
                 ORDER BY m.created_at DESC
                 LIMIT ?2",
            )?;
            let mut rows = stmt
                .query_map(params![cid, lim], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await?
    }

    /// History of DMs between two agents (oldest-first, capped).
    pub async fn history_dm(&self, a: &str, b: &str, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn.clone();
        let a = a.to_string();
        let b = b.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>> {
            let conn = conn.blocking_lock();
            let aid = ensure_agent_blocking(&conn, &a)?;
            let bid = ensure_agent_blocking(&conn, &b)?;

            let lim = i64::try_from(limit).unwrap_or(i64::MAX);
            let mut stmt = conn.prepare(
                "SELECT m.id, fa.name AS from_name,
                        ta.name AS to_agent, tc.name AS to_channel, m.is_broadcast,
                        m.body, m.intent, m.reply_to, m.created_at
                 FROM messages m
                 JOIN agents fa ON fa.id = m.from_agent
                 LEFT JOIN agents ta ON ta.id = m.to_agent
                 LEFT JOIN channels tc ON tc.id = m.to_channel
                 WHERE (m.from_agent = ?1 AND m.to_agent = ?2)
                    OR (m.from_agent = ?2 AND m.to_agent = ?1)
                 ORDER BY m.created_at DESC
                 LIMIT ?3",
            )?;
            let mut rows = stmt
                .query_map(params![aid, bid, lim], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await?
    }

    /// Snapshot counts for the status command. Returns
    /// (agents, channels, unread_deliveries, pending_scheduled).
    pub async fn status_counts(&self) -> Result<(i64, i64, i64, i64)> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(i64, i64, i64, i64)> {
            let conn = conn.blocking_lock();
            let agents: i64 = conn.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))?;
            let channels: i64 =
                conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;
            let unread: i64 = conn.query_row(
                "SELECT COUNT(*) FROM deliveries WHERE read_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let pending: i64 = conn.query_row(
                "SELECT COUNT(*) FROM scheduled WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )?;
            Ok((agents, channels, unread, pending))
        })
        .await?
    }

    /// Schedule a send for `deliver_at`. Returns the scheduled row id.
    pub async fn schedule(
        &self,
        from_name: &str,
        to: &str,
        body: &str,
        intent: Option<Intent>,
        reply_to: Option<i64>,
        deliver_at: DateTime<Utc>,
    ) -> Result<i64> {
        let conn = self.conn.clone();
        let payload = ScheduledPayload {
            from: from_name.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            intent: intent.map(|i| intent_to_str(i).to_string()),
            reply_to,
        };
        let payload_json = serde_json::to_string(&payload)?;
        let now = Utc::now().to_rfc3339();
        let deliver_at = deliver_at.to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let conn = conn.blocking_lock();
            conn.execute(
                "INSERT INTO scheduled (payload, deliver_at, created_at, status)
                 VALUES (?1, ?2, ?3, 'pending')",
                params![payload_json, deliver_at, now],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    /// Deliver all pending scheduled rows whose `deliver_at` is in the past.
    /// Inserts message + deliveries rows just like `send_message`, marks the
    /// scheduled row `delivered`. Returns events for the caller to broadcast.
    pub async fn deliver_due(&self) -> Result<Vec<DeliveredScheduled>> {
        let conn = self.conn.clone();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<Vec<DeliveredScheduled>> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;

            let due: Vec<(i64, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, payload FROM scheduled
                     WHERE status = 'pending' AND deliver_at <= ?1
                     ORDER BY deliver_at ASC
                     LIMIT 100",
                )?;
                stmt.query_map(params![now], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };

            let mut delivered = Vec::with_capacity(due.len());
            for (sched_id, payload_json) in due {
                let payload: ScheduledPayload = match serde_json::from_str(&payload_json) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, scheduled_id = sched_id, "bad scheduled payload, marking failed");
                        tx.execute(
                            "UPDATE scheduled SET status = 'failed' WHERE id = ?1",
                            params![sched_id],
                        )?;
                        continue;
                    }
                };

                let recipient = Recipient::parse(&payload.to);
                let from_id = ensure_agent_blocking(&tx, &payload.from)?;
                let (to_agent, to_channel, is_broadcast) = match &recipient {
                    Recipient::Agent(name) => {
                        let id = ensure_agent_blocking(&tx, name)?;
                        (Some(id), None, false)
                    }
                    Recipient::Channel(name) => {
                        let id = ensure_channel_blocking(&tx, name)?;
                        (None, Some(id), false)
                    }
                    Recipient::Broadcast => (None, None, true),
                };

                tx.execute(
                    "INSERT INTO messages (from_agent, to_agent, to_channel, is_broadcast, body, intent, reply_to, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![from_id, to_agent, to_channel, i64::from(is_broadcast), payload.body, payload.intent, payload.reply_to, now],
                )?;
                let msg_id = tx.last_insert_rowid();

                // Compute recipient agents (same logic as send_message).
                let recipient_ids: Vec<i64> = match &recipient {
                    Recipient::Agent(_) => vec![to_agent.expect("agent id set above")],
                    Recipient::Channel(_) => {
                        let cid = to_channel.expect("channel id set above");
                        let mut stmt = tx.prepare(
                            "SELECT a.id FROM agents a
                             JOIN memberships m ON m.agent_id = a.id
                             WHERE m.channel_id = ?1 AND a.id != ?2",
                        )?;
                        stmt.query_map(params![cid, from_id], |r| r.get::<_, i64>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?
                    }
                    Recipient::Broadcast => {
                        let mut stmt = tx.prepare("SELECT id FROM agents WHERE id != ?1")?;
                        stmt.query_map(params![from_id], |r| r.get::<_, i64>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?
                    }
                };

                for aid in &recipient_ids {
                    tx.execute(
                        "INSERT OR IGNORE INTO deliveries (message_id, agent_id, delivered_at)
                         VALUES (?1, ?2, ?3)",
                        params![msg_id, aid, now],
                    )?;
                }

                tx.execute(
                    "UPDATE scheduled SET status = 'delivered' WHERE id = ?1",
                    params![sched_id],
                )?;

                delivered.push(DeliveredScheduled {
                    message_id: msg_id,
                    from: payload.from,
                    to: recipient,
                    body: payload.body,
                });
            }

            tx.commit()?;
            Ok(delivered)
        })
        .await?
    }

    /// Subscribe `agent_name` to a channel. Creates the channel if needed.
    pub async fn join_channel(&self, agent_name: &str, channel: &str) -> Result<()> {
        let conn = self.conn.clone();
        let agent_name = agent_name.to_string();
        let channel = channel.to_string();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.blocking_lock();
            let agent_id = ensure_agent_blocking(&conn, &agent_name)?;
            let channel_id = ensure_channel_blocking(&conn, &channel)?;
            conn.execute(
                "INSERT OR IGNORE INTO memberships (agent_id, channel_id, joined_at)
                 VALUES (?1, ?2, ?3)",
                params![agent_id, channel_id, now],
            )?;
            Ok(())
        })
        .await?
    }

    /// Unsubscribe `agent_name` from a channel. No-op if not a member.
    pub async fn leave_channel(&self, agent_name: &str, channel: &str) -> Result<()> {
        let conn = self.conn.clone();
        let agent_name = agent_name.to_string();
        let channel = channel.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.blocking_lock();
            let agent_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM agents WHERE name = ?1",
                    params![agent_name],
                    |r| r.get(0),
                )
                .optional()?;
            let channel_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM channels WHERE name = ?1",
                    params![channel],
                    |r| r.get(0),
                )
                .optional()?;
            if let (Some(aid), Some(cid)) = (agent_id, channel_id) {
                conn.execute(
                    "DELETE FROM memberships WHERE agent_id = ?1 AND channel_id = ?2",
                    params![aid, cid],
                )?;
            }
            Ok(())
        })
        .await?
    }

    /// Case-insensitive substring search over message bodies. Newest-first.
    pub async fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn.clone();
        let pattern = format!("%{query}%");
        let lim = i64::try_from(limit).unwrap_or(i64::MAX);
        tokio::task::spawn_blocking(move || -> Result<Vec<Message>> {
            let conn = conn.blocking_lock();
            let mut stmt = conn.prepare(
                "SELECT m.id, fa.name AS from_name,
                        ta.name AS to_agent, tc.name AS to_channel, m.is_broadcast,
                        m.body, m.intent, m.reply_to, m.created_at
                 FROM messages m
                 JOIN agents fa ON fa.id = m.from_agent
                 LEFT JOIN agents ta ON ta.id = m.to_agent
                 LEFT JOIN channels tc ON tc.id = m.to_channel
                 WHERE m.body LIKE ?1 COLLATE NOCASE
                 ORDER BY m.created_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![pattern, lim], row_to_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?
    }

    /// Delete read messages older than `retention_days`. Returns rows deleted.
    pub async fn cleanup_old(&self, retention_days: i64) -> Result<usize> {
        let conn = self.conn.clone();
        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;
            let cutoff_str = cutoff.to_rfc3339();
            let dropped = tx.execute(
                r"
                DELETE FROM messages
                WHERE created_at < ?1
                  AND id IN (
                    SELECT m.id FROM messages m
                    LEFT JOIN deliveries d ON d.message_id = m.id
                    GROUP BY m.id
                    HAVING COUNT(d.read_at) = COUNT(d.message_id)
                  )
                ",
                params![cutoff_str],
            )?;
            tx.commit()?;
            Ok(dropped)
        })
        .await?
    }

    /// Open a session for `agent_name`. Returns session id.
    pub async fn open_session(&self, agent_name: &str) -> Result<i64> {
        let conn = self.conn.clone();
        let agent_name = agent_name.to_string();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let conn = conn.blocking_lock();
            let agent_id = ensure_agent_blocking(&conn, &agent_name)?;
            conn.execute(
                "INSERT INTO sessions (agent_id, started_at) VALUES (?1, ?2)",
                params![agent_id, now],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    pub async fn close_session(&self, session_id: i64) -> Result<()> {
        let conn = self.conn.clone();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.blocking_lock();
            conn.execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2 AND ended_at IS NULL",
                params![now, session_id],
            )?;
            Ok(())
        })
        .await?
    }

    pub async fn close_dangling_sessions(&self) -> Result<usize> {
        let conn = self.conn.clone();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = conn.blocking_lock();
            Ok(conn.execute(
                "UPDATE sessions SET ended_at = ?1 WHERE ended_at IS NULL",
                params![now],
            )?)
        })
        .await?
    }
}

// ---- helpers (sync, run inside spawn_blocking) ----

fn ensure_agent_blocking(conn: &Connection, name: &str) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM agents WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;

    let id = if let Some(id) = existing {
        conn.execute(
            "UPDATE agents SET last_seen = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        id
    } else {
        conn.execute(
            "INSERT INTO agents (name, first_seen, last_seen) VALUES (?1, ?2, ?2)",
            params![name, now],
        )?;
        let new_id = conn.last_insert_rowid();
        // Auto-join #general.
        if let Ok(cid) = ensure_channel_blocking(conn, DEFAULT_CHANNEL) {
            conn.execute(
                "INSERT OR IGNORE INTO memberships (agent_id, channel_id, joined_at)
                 VALUES (?1, ?2, ?3)",
                params![new_id, cid, now],
            )?;
        }
        new_id
    };
    Ok(id)
}

fn ensure_channel_blocking(conn: &Connection, name: &str) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM channels WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO channels (name, created_at) VALUES (?1, ?2)",
        params![name, now],
    )?;
    Ok(conn.last_insert_rowid())
}

fn seed_defaults(conn: &Connection) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO agents (name, display_name, first_seen, last_seen)
         VALUES ('master', 'master', ?1, ?1)",
        params![now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO channels (name, created_at) VALUES (?1, ?2)",
        params![DEFAULT_CHANNEL, now],
    )?;
    // Auto-join master to #general.
    if let (Ok(master_id), Ok(general_id)) = (
        conn.query_row("SELECT id FROM agents WHERE name = 'master'", [], |r| {
            r.get::<_, i64>(0)
        }),
        conn.query_row(
            "SELECT id FROM channels WHERE name = ?1",
            params![DEFAULT_CHANNEL],
            |r| r.get::<_, i64>(0),
        ),
    ) {
        conn.execute(
            "INSERT OR IGNORE INTO memberships (agent_id, channel_id, joined_at)
             VALUES (?1, ?2, ?3)",
            params![master_id, general_id, now],
        )?;
    }
    Ok(())
}

/// Extract `@name` mentions from a message body. Returns owned strings.
///
/// A mention is an `@` that is preceded by whitespace or appears at the
/// start of the body, followed by one or more name chars (alphanumeric,
/// `-`, or `_`). This avoids treating email addresses like
/// `user@example.com` as mentions.
fn extract_mentions(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let at_word_start =
            i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' || bytes[i - 1] == b'\n';
        if at_word_start && bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() {
                let c = bytes[end] as char;
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start {
                if let Ok(s) = std::str::from_utf8(&bytes[start..end]) {
                    let name = s.to_string();
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc))
}

fn intent_to_str(i: Intent) -> &'static str {
    match i {
        Intent::Fyi => "fyi",
        Intent::Question => "question",
        Intent::Task => "task",
        Intent::Handoff => "handoff",
    }
}

fn intent_from_str(s: &str) -> Option<Intent> {
    match s {
        "fyi" => Some(Intent::Fyi),
        "question" => Some(Intent::Question),
        "task" => Some(Intent::Task),
        "handoff" => Some(Intent::Handoff),
        _ => None,
    }
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let id: i64 = r.get(0)?;
    let from: String = r.get(1)?;
    let to_agent: Option<String> = r.get(2)?;
    let to_channel: Option<String> = r.get(3)?;
    let is_broadcast: i64 = r.get(4)?;
    let body: String = r.get(5)?;
    let intent: Option<String> = r.get(6)?;
    let reply_to: Option<i64> = r.get(7)?;
    let created_at: String = r.get(8)?;

    let to = if is_broadcast != 0 {
        Recipient::Broadcast
    } else if let Some(c) = to_channel {
        Recipient::Channel(c)
    } else if let Some(a) = to_agent {
        Recipient::Agent(a)
    } else {
        Recipient::Broadcast // shouldn't happen for well-formed rows
    };

    Ok(Message {
        id,
        from,
        to,
        body,
        intent: intent.and_then(|s| intent_from_str(&s)),
        reply_to,
        created_at: parse_ts(&created_at),
    })
}

#[cfg(test)]
mod mention_tests {
    use super::extract_mentions;

    #[test]
    fn simple_mention() {
        assert_eq!(extract_mentions("hi @codex check this"), vec!["codex"]);
    }

    #[test]
    fn at_start_of_message() {
        assert_eq!(extract_mentions("@alice ping"), vec!["alice"]);
    }

    #[test]
    fn multiple_mentions_deduped() {
        assert_eq!(
            extract_mentions("@alice and @bob, also @alice"),
            vec!["alice", "bob"]
        );
    }

    #[test]
    fn email_is_not_a_mention() {
        assert!(extract_mentions("reach me at user@example.com").is_empty());
    }

    #[test]
    fn trailing_punctuation_stripped() {
        assert_eq!(extract_mentions("@codex, look!"), vec!["codex"]);
    }

    #[test]
    fn allows_dashes_and_underscores() {
        assert_eq!(
            extract_mentions("hey @claude-code-2 and @bob_jr"),
            vec!["claude-code-2", "bob_jr"]
        );
    }

    #[test]
    fn bare_at_no_match() {
        assert!(extract_mentions("@ what is this").is_empty());
    }
}
