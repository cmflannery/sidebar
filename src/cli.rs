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
        Command::Say { body } => say(body).await,
        Command::Participants => participants().await,
        Command::History {
            channel,
            with,
            limit,
        } => history(channel, with, limit).await,
        Command::Pause => pause().await,
        Command::Resume => resume().await,
    }
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
                let to_label = match to {
                    Recipient::Agent(n) => format!("@{n}"),
                    Recipient::Channel(n) => format!("#{n}"),
                    Recipient::Broadcast => "*".to_string(),
                };
                println!("{from} → {to_label}: {body}");
            }
            Ok(crate::proto::Event::Paused) => println!("(paused)"),
            Ok(crate::proto::Event::Resumed) => println!("(resumed)"),
            // Quietly ignore non-event frames (e.g. the Subscribe ack).
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

async fn say(body: String) -> Result<()> {
    send("*".to_string(), body).await
}

async fn participants() -> Result<()> {
    let mut client = Client::connect_as("master").await?;
    let resp = client.request(Op::Participants).await?;
    if !resp.ok {
        anyhow::bail!("daemon error: {}", resp.error.unwrap_or_default());
    }
    match resp.data {
        Some(ResponseData::Agents { agents }) => {
            for a in agents {
                println!("{a}");
            }
            Ok(())
        }
        other => anyhow::bail!("unexpected response data: {other:?}"),
    }
}

async fn history(channel: Option<String>, with: Option<String>, limit: usize) -> Result<()> {
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
    match resp.data {
        Some(ResponseData::Messages { messages }) => {
            for m in messages {
                let to_label = match m.to {
                    Recipient::Agent(n) => format!("@{n}"),
                    Recipient::Channel(n) => format!("#{n}"),
                    Recipient::Broadcast => "*".to_string(),
                };
                println!(
                    "[{}] {} → {}: {}",
                    m.created_at.format("%H:%M:%S"),
                    m.from,
                    to_label,
                    m.body
                );
            }
            Ok(())
        }
        other => anyhow::bail!("unexpected response data: {other:?}"),
    }
}

async fn pause() -> Result<()> {
    anyhow::bail!("pause not yet implemented")
}

async fn resume() -> Result<()> {
    anyhow::bail!("resume not yet implemented")
}
