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

const SCHEMA: &str = include_str!("schema.sql");

/// Default retention for messages whose deliveries are all read.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

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

impl Store {
    /// Open (or create) the database at `path`, run schema, and seed the
    /// `master` agent if absent.
    pub async fn open(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let conn = Connection::open(&path)
                .with_context(|| format!("opening sqlite at {}", path.display()))?;
            // WAL mode + foreign keys are non-negotiable defaults.
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.execute_batch(SCHEMA).context("applying schema")?;
            seed_master(&conn).context("seeding master agent")?;
            Ok(conn)
        })
        .await??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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

    /// Delete read messages older than `cutoff`, plus their delivery rows.
    /// Agents and channels are kept forever.
    pub async fn cleanup_old(&self, retention_days: i64) -> Result<usize> {
        let conn = self.conn.clone();
        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let mut conn = conn.blocking_lock();
            let tx = conn.transaction()?;
            // Messages are "fully read" when every delivery row for them has read_at set.
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

    /// Open a new session row for `agent_name`. Returns session id.
    pub async fn open_session(&self, agent_name: &str) -> Result<i64> {
        let conn = self.conn.clone();
        let agent_name = agent_name.to_string();
        let now = Utc::now().to_rfc3339();
        tokio::task::spawn_blocking(move || -> Result<i64> {
            let conn = conn.blocking_lock();
            let agent_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM agents WHERE name = ?1",
                    params![agent_name],
                    |r| r.get(0),
                )
                .optional()?;
            let agent_id = if let Some(id) = agent_id {
                id
            } else {
                conn.execute(
                    "INSERT INTO agents (name, first_seen, last_seen) VALUES (?1, ?2, ?2)",
                    params![agent_name, now],
                )?;
                conn.last_insert_rowid()
            };
            conn.execute(
                "UPDATE agents SET last_seen = ?1 WHERE id = ?2",
                params![now, agent_id],
            )?;
            conn.execute(
                "INSERT INTO sessions (agent_id, started_at) VALUES (?1, ?2)",
                params![agent_id, now],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    /// Mark a session as ended.
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

    /// Mark all open sessions as ended at `now`. Called on daemon startup
    /// to clean up sessions left dangling by an ungraceful shutdown.
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

fn seed_master(conn: &Connection) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO agents (name, display_name, first_seen, last_seen)
         VALUES ('master', 'master', ?1, ?1)",
        params![now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO channels (name, created_at) VALUES ('general', ?1)",
        params![now],
    )?;
    Ok(())
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc))
}
