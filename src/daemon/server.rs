//! Unix-socket server for the sidebar daemon.
//!
//! Listens on the shared socket path; accepts both MCP-stub and CLI connections.
//! Each line on the wire is a JSON frame (NDJSON). See ARCHITECTURE.md §5.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::daemon::store::Store;
use crate::proto::{Event, Hello, HelloAck, Op, Request, Response, ResponseData};
use crate::types::{Recipient, validate_name};

#[derive(Clone)]
pub struct Daemon {
    pub store: Store,
    pub events: broadcast::Sender<Event>,
    /// When true, Op::Send is rejected and scheduler deliveries are held.
    pub paused: Arc<AtomicBool>,
    /// Used by `Op::Status` to report uptime.
    pub started_at: std::time::Instant,
    /// MCP session names currently in use. New connections that request
    /// a held name get suffixed (-2, -3, …). Released on disconnect.
    pub active_names: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

impl Daemon {
    pub fn new(store: Store) -> Self {
        let (events, _rx) = broadcast::channel(256);
        Self {
            store,
            events,
            paused: Arc::new(AtomicBool::new(false)),
            started_at: std::time::Instant::now(),
            active_names: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Reserve a unique name for an MCP session. If `requested` is free,
    /// returns it; otherwise tries `requested-2`, `requested-3`, … until
    /// it finds one. The chosen name is held until `release_name` runs.
    pub async fn reserve_unique_name(&self, requested: &str) -> String {
        let mut names = self.active_names.lock().await;
        if !names.contains(requested) {
            names.insert(requested.to_string());
            return requested.to_string();
        }
        for i in 2.. {
            let candidate = format!("{requested}-{i}");
            if !names.contains(&candidate) {
                names.insert(candidate.clone());
                return candidate;
            }
        }
        unreachable!("infinite loop bound by u64::MAX is unreachable in practice")
    }

    pub async fn release_name(&self, name: &str) {
        self.active_names.lock().await.remove(name);
    }
}

/// Bind the socket, accept connections, dispatch each to `handle_conn`.
/// Returns when `shutdown` resolves; cleans up the socket file on exit.
pub async fn run(
    daemon: Daemon,
    socket_path: PathBuf,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let listener = bind_socket(&socket_path).await?;
    info!(socket = %socket_path.display(), "daemon listening");

    tokio::pin!(shutdown);

    let daemon = Arc::new(daemon);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("shutdown signal received, stopping accept loop");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        let d = Arc::clone(&daemon);
                        tokio::spawn(async move {
                            if let Err(e) = handle_conn(d, stream).await {
                                warn!(error = %e, "connection ended with error");
                            }
                        });
                    }
                    Err(e) => error!(error = %e, "accept failed"),
                }
            }
        }
    }

    cleanup_socket(&socket_path);
    Ok(())
}

async fn bind_socket(path: &PathBuf) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            anyhow::bail!(
                "another sidebar daemon appears to be running at {}",
                path.display()
            );
        }
        warn!(path = %path.display(), "stale socket file, removing");
        std::fs::remove_file(path).context("removing stale socket")?;
    }
    UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
}

fn cleanup_socket(path: &PathBuf) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(error = %e, path = %path.display(), "failed to remove socket file");
        }
    }
}

async fn handle_conn(daemon: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (read, write) = stream.into_split();
    let mut reader = BufReader::new(read).lines();

    // First frame must be Hello.
    let Some(hello_line) = reader.next_line().await? else {
        return Ok(());
    };
    let hello: Hello = serde_json::from_str(&hello_line).context("parsing hello")?;

    let (agent_name, session_id, holds_name) = match &hello {
        Hello::Mcp { agent, .. } => {
            if let Err(e) = validate_name(agent.trim()) {
                anyhow::bail!("invalid mcp agent name `{agent}`: {e}");
            }
            // Uniquify the name if it's already in use by another active session.
            let assigned = daemon.reserve_unique_name(agent.trim()).await;
            let sid = daemon.store.open_session(&assigned).await?;
            if assigned == *agent {
                info!(agent = %assigned, session = sid, "mcp client registered");
            } else {
                info!(
                    requested = %agent, assigned = %assigned, session = sid,
                    "mcp name collision; assigned suffixed name"
                );
            }
            (assigned, Some(sid), true)
        }
        Hello::Cli { speaking_as } => {
            daemon.store.ensure_agent(speaking_as).await?;
            info!(as_who = %speaking_as, "cli client connected");
            (speaking_as.clone(), None, false)
        }
    };

    // mpsc fan-in for everything we write to this connection: responses + forwarded events.
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer_task = tokio::spawn(writer_task(write, out_rx));

    // Send the HelloAck before any request processing.
    let mut ack_bytes = serde_json::to_vec(&HelloAck {
        agent: agent_name.clone(),
    })?;
    ack_bytes.push(b'\n');
    if out_tx.send(ack_bytes).await.is_err() {
        // writer task gone; nothing more to do
        if holds_name {
            daemon.release_name(&agent_name).await;
        }
        return Ok(());
    }

    let req_result = request_loop(&daemon, &agent_name, &mut reader, out_tx).await;

    if let Some(sid) = session_id {
        if let Err(e) = daemon.store.close_session(sid).await {
            warn!(error = %e, session = sid, "failed to close session");
        }
    }
    if holds_name {
        daemon.release_name(&agent_name).await;
    }

    // out_tx dropped at end of request_loop → writer_task drains and exits.
    let _ = writer_task.await;
    req_result
}

async fn writer_task(mut write: OwnedWriteHalf, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = write.write_all(&bytes).await {
            // BrokenPipe / ConnectionReset are normal — the client closed
            // (Ctrl-C on `sidebar tail`, MCP stub exited, etc.). Don't
            // pollute logs at WARN for those.
            match e.kind() {
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset => {
                    tracing::debug!(error = %e, "writer task: client closed");
                }
                _ => warn!(error = %e, "writer task: write failed"),
            }
            break;
        }
    }
}

async fn request_loop(
    daemon: &Arc<Daemon>,
    agent_name: &str,
    reader: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>,
    out_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let (resp, subscribe_after) = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let is_subscribe = matches!(req.op, Op::Subscribe);
                let resp = dispatch(daemon, agent_name, req).await;
                let ok = resp.ok;
                (resp, is_subscribe && ok)
            }
            Err(e) => (
                Response {
                    id: parse_id_best_effort(&line),
                    ok: false,
                    error: Some(format!("bad request: {e}")),
                    data: None,
                },
                false,
            ),
        };

        let mut bytes = serde_json::to_vec(&resp)?;
        bytes.push(b'\n');
        if out_tx.send(bytes).await.is_err() {
            break; // writer task gone
        }

        if subscribe_after {
            // Spawn an event forwarder for this connection. It owns its own
            // sender clone; when the connection ends, its mpsc tx + the
            // broadcast::Receiver are dropped, and it shuts down.
            let mut events_rx = daemon.events.subscribe();
            let tx = out_tx.clone();
            tokio::spawn(async move {
                loop {
                    match events_rx.recv().await {
                        Ok(evt) => {
                            let mut bytes = match serde_json::to_vec(&evt) {
                                Ok(b) => b,
                                Err(e) => {
                                    warn!(error = %e, "serialize event");
                                    continue;
                                }
                            };
                            bytes.push(b'\n');
                            if tx.send(bytes).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "event subscriber lagged");
                        }
                    }
                }
            });
        }
    }
    Ok(())
}

/// Maximum a long-poll inbox can block before returning empty. Caps misbehaved
/// clients; agents that genuinely want a long wait can re-call.
const MAX_INBOX_WAIT_MS: u64 = 300_000; // 5 minutes

/// Soft cap on message body size. Local-only sidebar has no malicious
/// network attackers, but agents occasionally try to forward 50KB+ of
/// LLM output as a single message — bounded here so the daemon can't
/// be DoS'd by a runaway producer, and so the inbox response stays
/// human-grokkable.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Maximum delay for a scheduled send. Anything beyond this is almost
/// certainly a typo (year 9999) and would sit in the scheduled table
/// past retention horizons. Past timestamps are NOT clamped — they
/// just deliver on the next scheduler tick.
const MAX_SCHEDULE_DELAY_SECONDS: i64 = 60 * 60 * 24 * 365; // 1 year

/// Hard cap on `history` and `search` result counts. Generous enough to
/// answer "show me the last day" queries; small enough to keep the inbox
/// response inside one network frame and the daemon's working set bounded.
const MAX_QUERY_LIMIT: usize = 1000;

/// Cap on the substring length for `search`. A 1 MB query would still
/// match short bodies, but the LIKE scan and resulting memory copy is
/// pure waste.
const MAX_SEARCH_QUERY_LEN: usize = 256;

async fn fetch_inbox_with_long_poll(
    daemon: &Daemon,
    agent_name: &str,
    wait_ms: Option<u64>,
    mentions_only: bool,
) -> Result<ResponseData> {
    // Subscribe before the initial fetch. If a message is committed between
    // the fetch and the subscription, the old ordering could miss its event
    // and make the caller wait for the full timeout despite an unread row.
    let mut rx = daemon.events.subscribe();
    let messages = daemon.store.fetch_inbox(agent_name, mentions_only).await?;
    let wait = wait_ms.unwrap_or(0).min(MAX_INBOX_WAIT_MS);
    if !messages.is_empty() || wait == 0 {
        return Ok(ResponseData::Messages { messages });
    }

    let deadline = Instant::now() + Duration::from_millis(wait);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(ResponseData::Messages { messages: vec![] });
        }
        tokio::select! {
            biased;
            () = tokio::time::sleep(remaining) => {
                return Ok(ResponseData::Messages { messages: vec![] });
            }
            evt = rx.recv() => match evt {
                Ok(Event::Message { .. }) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    let msgs = daemon.store.fetch_inbox(agent_name, mentions_only).await?;
                    if !msgs.is_empty() {
                        return Ok(ResponseData::Messages { messages: msgs });
                    }
                    // False positive — event was for someone else; keep waiting.
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Ok(ResponseData::Messages { messages: vec![] });
                }
                // Non-Message events are not inbox deliveries.
                _ => {}
            }
        }
    }
}

fn parse_id_best_effort(line: &str) -> u64 {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        .unwrap_or(0)
}

#[allow(clippy::too_many_lines)] // big match over all ops; splitting hurts readability more than it helps
async fn dispatch(daemon: &Daemon, agent_name: &str, req: Request) -> Response {
    let id = req.id;
    let result: Result<ResponseData> = match req.op {
        Op::Participants => daemon
            .store
            .list_agents()
            .await
            .map(|rows| ResponseData::Agents {
                agents: rows.into_iter().map(|a| a.name).collect(),
            }),
        Op::Agents { include_stale } => {
            let stale_after = chrono::Duration::days(7);
            daemon
                .store
                .list_agents_detailed(include_stale, stale_after)
                .await
                .map(|agents_detailed| ResponseData::AgentsDetailed { agents_detailed })
        }
        Op::Channels => daemon
            .store
            .list_channels()
            .await
            .map(|channels| ResponseData::Channels { channels }),
        Op::ChannelsDetailed => daemon
            .store
            .list_channels_detailed()
            .await
            .map(|channels_detailed| ResponseData::ChannelsDetailed { channels_detailed }),
        Op::Send {
            to,
            body,
            intent,
            reply_to,
        } => {
            if daemon.is_paused() {
                return Response {
                    id,
                    ok: false,
                    error: Some("daemon is paused; resume with `sidebar resume`".into()),
                    data: None,
                };
            }
            let recipient = Recipient::parse(&to);
            if let Err(e) = recipient.validate() {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!("invalid recipient `{to}`: {e}")),
                    data: None,
                };
            }
            if body.len() > MAX_BODY_BYTES {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!(
                        "body is {} bytes; max is {MAX_BODY_BYTES}",
                        body.len()
                    )),
                    data: None,
                };
            }
            match daemon
                .store
                .send_message(agent_name, &recipient, &body, intent, reply_to)
                .await
            {
                Ok(message_id) => {
                    let _ = daemon.events.send(Event::Message {
                        to: recipient,
                        from: agent_name.to_string(),
                        body: body.clone(),
                        message_id,
                    });
                    Ok(ResponseData::SendOk { message_id })
                }
                Err(e) => Err(e),
            }
        }
        Op::Inbox {
            wait_ms,
            mentions_only,
        } => fetch_inbox_with_long_poll(daemon, agent_name, wait_ms, mentions_only).await,
        Op::History {
            channel,
            with,
            limit,
        } => {
            if limit > MAX_QUERY_LIMIT {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!("limit {limit} exceeds max of {MAX_QUERY_LIMIT}")),
                    data: None,
                };
            }
            match (channel, with) {
                (Some(c), _) => daemon
                    .store
                    .history_channel(&c, limit)
                    .await
                    .map(|messages| ResponseData::Messages { messages }),
                (None, Some(other)) => daemon
                    .store
                    .history_dm(agent_name, &other, limit)
                    .await
                    .map(|messages| ResponseData::Messages { messages }),
                (None, None) => Err(anyhow::anyhow!("history requires --channel or --with")),
            }
        }
        Op::HistoryDetailed { channel, limit } => {
            if limit > MAX_QUERY_LIMIT {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!("limit {limit} exceeds max of {MAX_QUERY_LIMIT}")),
                    data: None,
                };
            }
            daemon
                .store
                .history_channel_detailed(&channel, limit)
                .await
                .map(|messages_detailed| ResponseData::MessagesDetailed { messages_detailed })
        }
        Op::Subscribe => Ok(ResponseData::SendOk { message_id: 0 }),
        Op::Schedule { to, body, when } => {
            let recipient = Recipient::parse(&to);
            if let Err(e) = recipient.validate() {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!("invalid recipient `{to}`: {e}")),
                    data: None,
                };
            }
            if body.len() > MAX_BODY_BYTES {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!(
                        "body is {} bytes; max is {MAX_BODY_BYTES}",
                        body.len()
                    )),
                    data: None,
                };
            }
            let now = chrono::Utc::now();
            let deliver_at = match when {
                crate::proto::When::DelaySeconds { delay_seconds } => {
                    let secs = i64::try_from(delay_seconds).unwrap_or(i64::MAX);
                    now + chrono::Duration::seconds(secs)
                }
                crate::proto::When::At { at } => at,
            };
            let delay = (deliver_at - now).num_seconds();
            if delay > MAX_SCHEDULE_DELAY_SECONDS {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!(
                        "scheduled delivery is {} days in the future; max is 365",
                        delay / 86_400
                    )),
                    data: None,
                };
            }
            daemon
                .store
                .schedule(agent_name, &to, &body, None, None, deliver_at)
                .await
                .map(|id| ResponseData::SendOk { message_id: id })
        }
        Op::Join { channel } => {
            let channel = channel.trim();
            if let Err(e) = validate_name(channel) {
                Err(anyhow::anyhow!("invalid channel `{channel}`: {e}"))
            } else {
                daemon
                    .store
                    .join_channel(agent_name, channel)
                    .await
                    .map(|()| ResponseData::SendOk { message_id: 0 })
            }
        }
        Op::Leave { channel } => {
            let channel = channel.trim();
            if let Err(e) = validate_name(channel) {
                Err(anyhow::anyhow!("invalid channel `{channel}`: {e}"))
            } else {
                daemon
                    .store
                    .leave_channel(agent_name, channel)
                    .await
                    .map(|()| ResponseData::SendOk { message_id: 0 })
            }
        }
        Op::Search { query, limit } => {
            if query.len() > MAX_SEARCH_QUERY_LEN {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!(
                        "search query is {} chars; max is {MAX_SEARCH_QUERY_LEN}",
                        query.len()
                    )),
                    data: None,
                };
            }
            if limit > MAX_QUERY_LIMIT {
                return Response {
                    id,
                    ok: false,
                    error: Some(format!("limit {limit} exceeds max of {MAX_QUERY_LIMIT}")),
                    data: None,
                };
            }
            daemon
                .store
                .search_messages(&query, limit)
                .await
                .map(|messages| ResponseData::Messages { messages })
        }
        Op::Inspect { message_id } => match daemon.store.inspect_message(message_id).await {
            Ok(Some(detail)) => Ok(ResponseData::MessageDetail(detail)),
            Ok(None) => Err(anyhow::anyhow!("no message with id {message_id}")),
            Err(e) => Err(e),
        },
        Op::Scheduled => {
            let only = if agent_name == "master" {
                None
            } else {
                Some(agent_name)
            };
            daemon
                .store
                .list_scheduled(only)
                .await
                .map(|scheduled| ResponseData::Scheduled { scheduled })
        }
        Op::Cancel { scheduled_id } => match daemon
            .store
            .cancel_scheduled(scheduled_id, agent_name)
            .await
        {
            Ok(true) => Ok(ResponseData::SendOk {
                message_id: scheduled_id,
            }),
            Ok(false) => Err(anyhow::anyhow!(
                "scheduled id {scheduled_id} not found, already fired, or not yours to cancel"
            )),
            Err(e) => Err(e),
        },
        Op::Prune {
            inactive_days,
            dry_run,
        } => {
            if inactive_days < 1 {
                return Response {
                    id,
                    ok: false,
                    error: Some("inactive_days must be at least 1".into()),
                    data: None,
                };
            }
            if dry_run {
                daemon
                    .store
                    .prunable_agents(inactive_days)
                    .await
                    .map(|agents| ResponseData::Agents { agents })
            } else {
                daemon
                    .store
                    .prune_inactive_agents(inactive_days)
                    .await
                    .map(|count| ResponseData::SendOk {
                        message_id: i64::try_from(count).unwrap_or(i64::MAX),
                    })
            }
        }
        Op::Pause => {
            daemon.paused.store(true, Ordering::Release);
            let _ = daemon.events.send(Event::Paused);
            Ok(ResponseData::SendOk { message_id: 0 })
        }
        Op::Resume => {
            daemon.paused.store(false, Ordering::Release);
            let _ = daemon.events.send(Event::Resumed);
            Ok(ResponseData::SendOk { message_id: 0 })
        }
        Op::Status => match daemon.store.status_counts().await {
            Ok((agent_count, channel_count, unread_count, pending_scheduled)) => {
                let socket = crate::paths::socket()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let db = crate::paths::db()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let uptime =
                    i64::try_from(daemon.started_at.elapsed().as_secs()).unwrap_or(i64::MAX);
                Ok(ResponseData::Status(crate::proto::StatusInfo {
                    paused: daemon.is_paused(),
                    agent_count,
                    channel_count,
                    unread_count,
                    pending_scheduled,
                    uptime_seconds: uptime,
                    db_path: db,
                    socket_path: socket,
                }))
            }
            Err(e) => Err(e),
        },
    };

    match result {
        Ok(data) => Response {
            id,
            ok: true,
            error: None,
            data: Some(data),
        },
        Err(e) => Response {
            id,
            ok: false,
            error: Some(e.to_string()),
            data: None,
        },
    }
}
