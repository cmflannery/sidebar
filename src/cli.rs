// Stubs are async by design — they will do socket I/O when implemented.
// Silencing until the bodies are real.
#![allow(clippy::unused_async)]

use anyhow::{Context, Result};

use crate::Command;
use crate::client::Client;
use crate::proto::{Op, ResponseData, TurnStatus};
use crate::types::Recipient;

pub async fn dispatch(cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve => serve().await,
        Command::Mcp { as_name } => mcp(as_name).await,
        Command::Supervise {
            as_name,
            wait_ms,
            once,
            command,
        } => supervise(as_name, wait_ms, once, command).await,
        Command::Tail { json, filter } => tail(json, filter).await,
        Command::Send { to, body } => send(to, body).await,
        Command::Schedule {
            to,
            body,
            in_seconds,
            at,
            as_name,
        } => schedule(to, body, in_seconds, at, as_name).await,
        Command::Inbox {
            as_name,
            wait_ms,
            mentions_only,
            json,
        } => inbox(as_name, wait_ms, mentions_only, json).await,
        Command::Say { body } => say(body).await,
        Command::Participants { json } => participants(json).await,
        Command::Agents { all, json } => agents(all, json).await,
        Command::Channels { details, json } => channels(details, json).await,
        Command::History {
            channel,
            with,
            limit,
            json,
        } => history(channel, with, limit, json).await,
        Command::Grep { query, limit, json } => grep(query, limit, json).await,
        Command::Join { channels, as_name } => join(channels, as_name).await,
        Command::Leave { channels, as_name } => leave(channels, as_name).await,
        Command::Inspect { message_id, json } => inspect(message_id, json).await,
        Command::Scheduled { as_name, json } => scheduled(as_name, json).await,
        Command::Cancel {
            scheduled_id,
            as_name,
        } => cancel(scheduled_id, as_name).await,
        Command::Prune {
            inactive_days,
            dry_run,
        } => prune(inactive_days, dry_run).await,
        Command::Pause => pause().await,
        Command::Resume => resume().await,
        Command::Status { json } => status(json).await,
        Command::Web { bind } => crate::web::serve(&bind).await,
        // Handled in main.rs before dispatch — clap_complete writes the
        // script and returns; we never reach here.
        Command::Completions { .. } => unreachable!("handled in main"),
    }
}

async fn status(json: bool) -> Result<()> {
    let mut client = match Client::connect_as("master").await {
        Ok(c) => c,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "daemon": "down", "error": e.to_string() })
                );
            } else {
                println!("daemon: not running ({e})");
                println!("start it with `sidebar serve` in another terminal");
            }
            return Ok(());
        }
    };
    let resp = client.request(Op::Status).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Status(s)) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&s)?);
        return Ok(());
    }
    let h = s.uptime_seconds / 3600;
    let m = (s.uptime_seconds % 3600) / 60;
    let sec = s.uptime_seconds % 60;
    println!("daemon:      running");
    println!("uptime:      {h}h {m}m {sec}s");
    println!("paused:      {}", s.paused);
    println!("agents:      {}", s.agent_count);
    println!("channels:    {}", s.channel_count);
    println!("unread msgs: {}", s.unread_count);
    println!("scheduled:   {} pending", s.pending_scheduled);
    println!("socket:      {}", s.socket_path);
    println!("db:          {}", s.db_path);
    Ok(())
}

async fn channels(details: bool, json: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    if details {
        let resp = client.request(Op::ChannelsDetailed).await?;
        if !resp.ok {
            anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
        }
        let Some(ResponseData::ChannelsDetailed { channels_detailed }) = resp.data else {
            anyhow::bail!("unexpected response: {resp:?}");
        };
        if json {
            println!("{}", serde_json::to_string(&channels_detailed)?);
            return Ok(());
        }
        if channels_detailed.is_empty() {
            println!("(no channels)");
            return Ok(());
        }
        let name_w = channels_detailed
            .iter()
            .map(|c| c.name.len() + 1) // +1 for `#`
            .max()
            .unwrap_or(8);
        let now = chrono::Utc::now();
        println!("{:<name_w$}  members  last activity", "CHANNEL");
        for c in channels_detailed {
            let display = format!("#{}", c.name);
            let last = c.last_message_at.map_or_else(
                || "—".to_string(),
                |t| format_relative(now.signed_duration_since(t)),
            );
            println!("{display:<name_w$}  {:>7}  {last}", c.member_count);
        }
        return Ok(());
    }

    let resp = client.request(Op::Channels).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Channels { channels }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&channels)?);
    } else {
        for c in channels {
            println!("#{c}");
        }
    }
    Ok(())
}

async fn agents(all: bool, json: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(Op::Agents { include_stale: all }).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::AgentsDetailed { agents_detailed }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&agents_detailed)?);
        return Ok(());
    }
    if agents_detailed.is_empty() {
        println!("(no agents seen in the last 7 days; --all to include stale)");
        return Ok(());
    }
    let now = chrono::Utc::now();
    let name_w = agents_detailed
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(4);
    println!("{:<name_w$}  last seen", "NAME");
    for a in agents_detailed {
        let delta = now.signed_duration_since(a.last_seen);
        let rel = format_relative(delta);
        println!("{:<name_w$}  {rel}", a.name);
    }
    Ok(())
}

fn format_relative(d: chrono::Duration) -> String {
    let s = d.num_seconds();
    if s < 5 {
        return "just now".into();
    }
    if s < 60 {
        return format!("{s}s ago");
    }
    let m = d.num_minutes();
    if m < 60 {
        return format!("{m}m ago");
    }
    let h = d.num_hours();
    if h < 48 {
        return format!("{h}h ago");
    }
    format!("{}d ago", d.num_days())
}

async fn serve() -> Result<()> {
    crate::daemon::serve().await
}

async fn mcp(as_name: Option<String>) -> Result<()> {
    let name = as_name
        .or_else(|| std::env::var("SIDEBAR_AGENT_NAME").ok())
        .unwrap_or_else(|| format!("agent-{}", std::process::id()));
    crate::mcp::serve(name).await
}

#[allow(clippy::too_many_lines)]
async fn supervise(as_name: String, wait_ms: u64, once: bool, command: Vec<String>) -> Result<()> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command as ProcessCommand;

    let Some((program, args)) = command.split_first() else {
        anyhow::bail!("supervise requires a host command after `--`");
    };
    let mut client = Client::connect_mcp(&as_name, env!("CARGO_PKG_VERSION")).await?;
    eprintln!(
        "supervisor `{}` listening as @{} (wait {}ms)",
        program,
        client.assigned_name(),
        wait_ms.min(300_000)
    );

    loop {
        let response = client
            .request(Op::Inbox {
                wait_ms: Some(wait_ms),
                mentions_only: true,
            })
            .await?;
        let Some(ResponseData::Messages { messages }) = response.data else {
            anyhow::bail!("unexpected inbox response: {response:?}");
        };

        for message in messages {
            let client_turn_id = format!("supervisor-{}-{}", std::process::id(), message.id);
            let begin = client
                .request(Op::BeginTurn {
                    message_id: message.id,
                    client_turn_id: Some(client_turn_id),
                })
                .await?;
            if !begin.ok {
                eprintln!(
                    "could not begin turn for message {}: {}",
                    message.id,
                    begin.error.unwrap_or_else(|| "unknown error".into())
                );
                continue;
            }
            let Some(ResponseData::Turn { turn }) = begin.data else {
                anyhow::bail!("unexpected begin_turn response: {begin:?}");
            };
            client
                .request(Op::UpdateTurn {
                    turn_id: turn.turn_id.clone(),
                    status: TurnStatus::Started,
                    response: None,
                    error: None,
                })
                .await?;

            let envelope = supervisor_prompt(&message);
            let mut child = ProcessCommand::new(program)
                .args(args)
                .env("SIDEBAR_TURN_ID", &turn.turn_id)
                .env("SIDEBAR_MESSAGE_ID", message.id.to_string())
                .env("SIDEBAR_AGENT_NAME", client.assigned_name())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("starting supervisor host command `{program}`"))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(envelope.as_bytes()).await?;
                stdin.shutdown().await?;
            }
            let output = child.wait_with_output().await?;
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let succeeded = output.status.success() && !stdout.is_empty();
            let update = if succeeded {
                Op::UpdateTurn {
                    turn_id: turn.turn_id.clone(),
                    status: TurnStatus::ResponseCompleted,
                    response: Some(stdout.clone()),
                    error: None,
                }
            } else {
                let error = if stderr.is_empty() {
                    format!("host command exited with status {}", output.status)
                } else {
                    stderr
                };
                Op::UpdateTurn {
                    turn_id: turn.turn_id.clone(),
                    status: TurnStatus::Failed,
                    response: None,
                    error: Some(error),
                }
            };
            let updated = client.request(update).await?;
            if updated.ok {
                if succeeded {
                    println!("turn {} completed", turn.turn_id);
                } else {
                    eprintln!("turn {} failed: host produced no response", turn.turn_id);
                }
            } else {
                eprintln!(
                    "could not finish turn {}: {}",
                    turn.turn_id,
                    updated.error.unwrap_or_else(|| "unknown error".into())
                );
            }
        }

        if once {
            break;
        }
    }
    Ok(())
}

fn supervisor_prompt(message: &crate::types::Message) -> String {
    let destination = match &message.to {
        crate::types::Recipient::Agent(name) => format!("@{name}"),
        crate::types::Recipient::Channel(name) => format!("#{name}"),
        crate::types::Recipient::Broadcast => "*".to_string(),
    };
    format!(
        "You are replying as a participant in a durable agent room.\n\nMessage id: {}\nFrom: {}\nTo: {}\n\nMessage:\n{}\n\nReturn only the final human-readable response. Do not include supervisor metadata or tool logs.",
        message.id, message.from, destination, message.body
    )
}

async fn tail(json: bool, filter: Option<String>) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixStream;

    let path = crate::paths::socket()?;
    let stream = UnixStream::connect(&path).await?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read).lines();

    // Hello as master, then subscribe.
    let mut bytes = serde_json::to_vec(&crate::proto::Hello::Cli {
        speaking_as: "master".to_string(),
    })?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;

    let mut req_bytes = serde_json::to_vec(&crate::proto::Request {
        id: 1,
        op: Op::Subscribe,
    })?;
    req_bytes.push(b'\n');
    write.write_all(&req_bytes).await?;

    let filter_lower = filter.as_ref().map(|s| s.to_lowercase());

    // Read first frame: should be the Subscribe response. After that, events.
    while let Some(line) = reader.next_line().await? {
        if json {
            // JSON mode skips filtering — scripts can grep on their side.
            println!("{line}");
            continue;
        }
        let formatted = match serde_json::from_str::<crate::proto::Event>(&line) {
            Ok(crate::proto::Event::Message {
                to,
                from,
                body,
                message_id: _,
            }) => {
                let now = chrono::Local::now().format("%H:%M:%S");
                let to_label = match to {
                    Recipient::Agent(n) => format!("@{n}"),
                    Recipient::Channel(n) => format!("#{n}"),
                    Recipient::Broadcast => "*".to_string(),
                };
                Some(format!("[{now}] {from} → {to_label}: {body}"))
            }
            Ok(crate::proto::Event::Paused) => Some(format!(
                "[{}] (paused)",
                chrono::Local::now().format("%H:%M:%S")
            )),
            Ok(crate::proto::Event::Resumed) => Some(format!(
                "[{}] (resumed)",
                chrono::Local::now().format("%H:%M:%S")
            )),
            // Quietly ignore non-event frames (HelloAck, Subscribe ack).
            Err(_) => None,
        };
        let Some(line) = formatted else { continue };
        if let Some(needle) = filter_lower.as_ref() {
            if !line.to_lowercase().contains(needle) {
                continue;
            }
        }
        println!("{line}");
    }
    Ok(())
}

async fn send(to: String, body: String) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client
        .request(Op::Send {
            to,
            body,
            intent: None,
            reply_to: None,
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    Ok(())
}

async fn inbox(
    as_name: String,
    wait_ms: Option<u64>,
    mentions_only: bool,
    json: bool,
) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client
        .request(Op::Inbox {
            wait_ms,
            mentions_only,
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Messages { messages }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    print_messages(&messages, json)
}

fn print_messages(messages: &[crate::types::Message], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(messages)?);
        return Ok(());
    }
    for m in messages {
        let to_label = match &m.to {
            Recipient::Agent(n) => format!("@{n}"),
            Recipient::Channel(n) => format!("#{n}"),
            Recipient::Broadcast => "*".to_string(),
        };
        let ts = m
            .created_at
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S");
        println!("[{ts}] {} → {to_label}: {}", m.from, m.body);
    }
    Ok(())
}

async fn join(channels: Vec<String>, as_name: String) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    for raw in channels {
        let channel = raw.trim_start_matches('#').to_string();
        let resp = client
            .request(Op::Join {
                channel: channel.clone(),
            })
            .await?;
        if !resp.ok {
            anyhow::bail!(
                "daemon error joining #{channel}: {}",
                resp.error.unwrap_or_default()
            );
        }
        println!("{as_name} joined #{channel}");
    }
    Ok(())
}

async fn leave(channels: Vec<String>, as_name: String) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    for raw in channels {
        let channel = raw.trim_start_matches('#').to_string();
        let resp = client
            .request(Op::Leave {
                channel: channel.clone(),
            })
            .await?;
        if !resp.ok {
            anyhow::bail!(
                "daemon error leaving #{channel}: {}",
                resp.error.unwrap_or_default()
            );
        }
        println!("{as_name} left #{channel}");
    }
    Ok(())
}

async fn grep(query: String, limit: usize, json: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(Op::Search { query, limit }).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Messages { messages }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    print_messages(&messages, json)
}

async fn say(body: String) -> Result<()> {
    send("*".to_string(), body).await
}

async fn schedule(
    to: String,
    body: String,
    in_seconds: Option<u64>,
    at: Option<String>,
    as_name: String,
) -> Result<()> {
    use crate::proto::When;
    let when = match (in_seconds, at) {
        (Some(s), None) => When::DelaySeconds { delay_seconds: s },
        (None, Some(at)) => {
            let ts = chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|e| anyhow::anyhow!("invalid --at timestamp: {e}"))?;
            When::At {
                at: ts.with_timezone(&chrono::Utc),
            }
        }
        (Some(_), Some(_)) => anyhow::bail!("use --in or --at, not both"),
        (None, None) => anyhow::bail!("either --in <seconds> or --at <ISO8601> is required"),
    };
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client.request(Op::Schedule { to, body, when }).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    if let Some(ResponseData::SendOk { message_id }) = resp.data {
        println!("scheduled id {message_id}");
    }
    Ok(())
}

async fn participants(json: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(Op::Participants).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Agents { agents }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&agents)?);
    } else {
        for a in agents {
            println!("{a}");
        }
    }
    Ok(())
}

async fn history(
    channel: Option<String>,
    with: Option<String>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client
        .request(Op::History {
            channel,
            with,
            limit,
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Messages { messages }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    print_messages(&messages, json)
}

async fn inspect(message_id: i64, json: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(Op::Inspect { message_id }).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::MessageDetail(detail)) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&detail)?);
        return Ok(());
    }
    let m = &detail.message;
    let to_label = match &m.to {
        Recipient::Agent(n) => format!("@{n}"),
        Recipient::Channel(n) => format!("#{n}"),
        Recipient::Broadcast => "*".to_string(),
    };
    let ts = m
        .created_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S");
    println!("message {} — {} → {to_label} at {ts}", m.id, m.from);
    if let Some(intent) = &m.intent {
        println!("intent: {intent:?}");
    }
    if let Some(reply) = m.reply_to {
        println!("reply_to: {reply}");
    }
    println!("body:");
    for line in m.body.lines() {
        println!("  {line}");
    }
    println!();
    if detail.deliveries.is_empty() {
        println!("(no deliveries — message went nowhere)");
    } else {
        let name_w = detail
            .deliveries
            .iter()
            .map(|d| d.agent.len())
            .max()
            .unwrap_or(5);
        println!("deliveries:");
        for d in &detail.deliveries {
            let delivered = d.delivered_at.map_or_else(
                || "(undelivered)".to_string(),
                |t| {
                    t.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                },
            );
            let read = d.read_at.map_or_else(
                || "unread".to_string(),
                |t| {
                    format!(
                        "read {}",
                        t.with_timezone(&chrono::Local).format("%H:%M:%S")
                    )
                },
            );
            println!("  {:<name_w$}  delivered {delivered}  {read}", d.agent);
        }
    }
    Ok(())
}

async fn scheduled(as_name: String, json: bool) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client.request(Op::Scheduled).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    let Some(ResponseData::Scheduled { scheduled }) = resp.data else {
        anyhow::bail!("unexpected response: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string(&scheduled)?);
        return Ok(());
    }
    if scheduled.is_empty() {
        println!("(no pending scheduled messages)");
        return Ok(());
    }
    println!("ID    FIRES                       FROM → TO         BODY");
    for s in scheduled {
        let when = s
            .deliver_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S");
        let body_preview: String = s.body.chars().take(40).collect();
        println!("{:<5} {when}  {} → {}  {body_preview}", s.id, s.from, s.to);
    }
    Ok(())
}

async fn cancel(scheduled_id: i64, as_name: String) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client.request(Op::Cancel { scheduled_id }).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    println!("cancelled scheduled id {scheduled_id}");
    Ok(())
}

async fn prune(inactive_days: i64, dry_run: bool) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client
        .request(Op::Prune {
            inactive_days,
            dry_run,
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    if dry_run {
        match resp.data {
            Some(ResponseData::Agents { agents }) => {
                if agents.is_empty() {
                    println!("(no inactive agents would be pruned)");
                } else {
                    println!(
                        "would prune {} agent(s) (use without --dry-run to apply):",
                        agents.len()
                    );
                    for a in agents {
                        println!("  {a}");
                    }
                }
            }
            other => anyhow::bail!("unexpected response: {other:?}"),
        }
    } else if let Some(ResponseData::SendOk { message_id: count }) = resp.data {
        println!("pruned {count} inactive agent(s)");
    }
    Ok(())
}

async fn pause() -> Result<()> {
    toggle(Op::Pause, "paused").await
}

async fn resume() -> Result<()> {
    toggle(Op::Resume, "resumed").await
}

async fn toggle(op: Op, label: &str) -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(op).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    println!("{label}");
    Ok(())
}
