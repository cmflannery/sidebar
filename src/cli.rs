// Stubs are async by design — they will do socket I/O when implemented.
// Silencing until the bodies are real.
#![allow(clippy::unused_async)]

use anyhow::Result;

use crate::Command;
use crate::client::Client;
use crate::proto::{Op, ResponseData};
use crate::types::Recipient;

pub async fn dispatch(cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve => serve().await,
        Command::Mcp { as_name } => mcp(as_name).await,
        Command::Tail { json } => tail(json).await,
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
            json,
        } => inbox(as_name, wait_ms, json).await,
        Command::Say { body } => say(body).await,
        Command::Participants { json } => participants(json).await,
        Command::Agents { all, json } => agents(all, json).await,
        Command::History {
            channel,
            with,
            limit,
            json,
        } => history(channel, with, limit, json).await,
        Command::Grep { query, limit, json } => grep(query, limit, json).await,
        Command::Join { channel, as_name } => join(channel, as_name).await,
        Command::Leave { channel, as_name } => leave(channel, as_name).await,
        Command::Pause => pause().await,
        Command::Resume => resume().await,
        Command::Status { json } => status(json).await,
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

async fn tail(json: bool) -> Result<()> {
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

    // Read first frame: should be the Subscribe response. After that, events.
    while let Some(line) = reader.next_line().await? {
        if json {
            println!("{line}");
            continue;
        }
        match serde_json::from_str::<crate::proto::Event>(&line) {
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
                println!("[{now}] {from} → {to_label}: {body}");
            }
            Ok(crate::proto::Event::Paused) => {
                println!("[{}] (paused)", chrono::Local::now().format("%H:%M:%S"));
            }
            Ok(crate::proto::Event::Resumed) => {
                println!("[{}] (resumed)", chrono::Local::now().format("%H:%M:%S"));
            }
            // Quietly ignore non-event frames (HelloAck, Subscribe ack).
            Err(_) => {}
        }
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

async fn inbox(as_name: String, wait_ms: Option<u64>, json: bool) -> Result<()> {
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client.request(Op::Inbox { wait_ms }).await?;
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

async fn join(channel: String, as_name: String) -> Result<()> {
    let channel = channel.trim_start_matches('#').to_string();
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client
        .request(Op::Join {
            channel: channel.clone(),
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    println!("{as_name} joined #{channel}");
    Ok(())
}

async fn leave(channel: String, as_name: String) -> Result<()> {
    let channel = channel.trim_start_matches('#').to_string();
    let mut client = Client::connect_as(&as_name).await?;
    let resp = client
        .request(Op::Leave {
            channel: channel.clone(),
        })
        .await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    println!("{as_name} left #{channel}");
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
