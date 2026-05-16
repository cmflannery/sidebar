//! Unix-socket server for the sidebar daemon.
//!
//! Listens on the shared socket path; accepts both MCP-stub and CLI connections.
//! Each line on the wire is a JSON frame (NDJSON). See ARCHITECTURE.md §5.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::daemon::store::Store;
use crate::proto::{Event, Hello, Op, Request, Response, ResponseData};

#[derive(Clone)]
pub struct Daemon {
    pub store: Store,
    #[allow(dead_code)] // wired up when broker/tail land
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

/// Bind the unix socket, replacing a stale file if one exists and no daemon is alive.
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
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read).lines();

    // First frame must be Hello.
    let Some(hello_line) = reader.next_line().await? else {
        return Ok(()); // client closed without saying hello
    };
    let hello: Hello = serde_json::from_str(&hello_line).context("parsing hello")?;

    let (agent_name, session_id) = match &hello {
        Hello::Mcp { agent, .. } => {
            let sid = daemon.store.open_session(agent).await?;
            info!(agent = %agent, session = sid, "mcp client registered");
            (agent.clone(), Some(sid))
        }
        Hello::Cli { speaking_as } => {
            info!(as_who = %speaking_as, "cli client connected");
            (speaking_as.clone(), None)
        }
    };

    // Request loop.
    let result = run_request_loop(&daemon, &agent_name, &mut reader, &mut write).await;

    if let Some(sid) = session_id {
        if let Err(e) = daemon.store.close_session(sid).await {
            warn!(error = %e, session = sid, "failed to close session");
        }
    }

    result
}

async fn run_request_loop(
    daemon: &Daemon,
    _agent_name: &str,
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(daemon, req).await,
            Err(e) => Response {
                id: parse_id_best_effort(&line),
                ok: false,
                error: Some(format!("bad request: {e}")),
                data: None,
            },
        };
        let mut bytes = serde_json::to_vec(&resp)?;
        bytes.push(b'\n');
        write.write_all(&bytes).await?;
    }
    Ok(())
}

fn parse_id_best_effort(line: &str) -> u64 {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        .unwrap_or(0)
}

async fn dispatch(daemon: &Daemon, req: Request) -> Response {
    let id = req.id;
    let result: Result<ResponseData> = match req.op {
        Op::Participants => match daemon.store.list_agents().await {
            Ok(rows) => Ok(ResponseData::Agents {
                agents: rows.into_iter().map(|a| a.name).collect(),
            }),
            Err(e) => Err(e),
        },
        // Stubs for ops we haven't wired up yet.
        Op::Send { .. }
        | Op::Inbox { .. }
        | Op::History { .. }
        | Op::Schedule { .. }
        | Op::Channels
        | Op::Pause
        | Op::Resume => Err(anyhow::anyhow!("op not yet implemented")),
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
