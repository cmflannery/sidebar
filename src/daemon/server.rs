//! Unix-socket server for the sidebar daemon.
//!
//! Listens on the shared socket path; accepts both MCP-stub and CLI connections.
//! Each line on the wire is a JSON frame (NDJSON). See ARCHITECTURE.md §5.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::daemon::store::Store;
use crate::proto::{Event, Hello, Op, Request, Response, ResponseData};
use crate::types::Recipient;

#[derive(Clone)]
pub struct Daemon {
    pub store: Store,
    pub events: broadcast::Sender<Event>,
}

impl Daemon {
    pub fn new(store: Store) -> Self {
        let (events, _rx) = broadcast::channel(256);
        Self { store, events }
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

    let (agent_name, session_id) = match &hello {
        Hello::Mcp { agent, .. } => {
            let sid = daemon.store.open_session(agent).await?;
            info!(agent = %agent, session = sid, "mcp client registered");
            (agent.clone(), Some(sid))
        }
        Hello::Cli { speaking_as } => {
            daemon.store.ensure_agent(speaking_as).await?;
            info!(as_who = %speaking_as, "cli client connected");
            (speaking_as.clone(), None)
        }
    };

    // mpsc fan-in for everything we write to this connection: responses + forwarded events.
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer_task = tokio::spawn(writer_task(write, out_rx));

    let req_result = request_loop(&daemon, &agent_name, &mut reader, out_tx).await;

    if let Some(sid) = session_id {
        if let Err(e) = daemon.store.close_session(sid).await {
            warn!(error = %e, session = sid, "failed to close session");
        }
    }

    // out_tx dropped at end of request_loop → writer_task drains and exits.
    let _ = writer_task.await;
    req_result
}

async fn writer_task(mut write: OwnedWriteHalf, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = write.write_all(&bytes).await {
            warn!(error = %e, "writer task: write failed");
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

fn parse_id_best_effort(line: &str) -> u64 {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        .unwrap_or(0)
}

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
        Op::Channels => daemon
            .store
            .list_channels()
            .await
            .map(|channels| ResponseData::Channels { channels }),
        Op::Send {
            to,
            body,
            intent,
            reply_to,
        } => {
            let recipient = Recipient::parse(&to);
            match daemon
                .store
                .send_message(agent_name, &recipient, &body, intent, reply_to)
                .await
            {
                Ok(send_result) => {
                    let evt = Event::Message {
                        to: recipient,
                        from: agent_name.to_string(),
                        body: body.clone(),
                        message_id: send_result.message_id,
                    };
                    // best-effort emit; ignore "no subscribers" error
                    let _ = daemon.events.send(evt);
                    Ok(ResponseData::SendOk {
                        message_id: send_result.message_id,
                    })
                }
                Err(e) => Err(e),
            }
        }
        Op::Inbox { wait_ms: _ } => {
            // wait_ms long-poll deferred; v1 returns whatever is unread now.
            daemon
                .store
                .fetch_inbox(agent_name)
                .await
                .map(|messages| ResponseData::Messages { messages })
        }
        Op::History {
            channel,
            with,
            limit,
        } => match (channel, with) {
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
        },
        Op::Subscribe => Ok(ResponseData::SendOk { message_id: 0 }),
        Op::Schedule { .. } | Op::Pause | Op::Resume => {
            Err(anyhow::anyhow!("op not yet implemented"))
        }
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
