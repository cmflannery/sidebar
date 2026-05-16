use anyhow::Result;

use crate::Command;

pub async fn dispatch(cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve => serve().await,
        Command::Mcp => mcp().await,
        Command::Tail { json } => tail(json).await,
        Command::Send { to, body } => send(to, body).await,
        Command::Say { body } => say(body).await,
        Command::Participants => participants().await,
        Command::History { channel, with, limit } => history(channel, with, limit).await,
        Command::Pause => pause().await,
        Command::Resume => resume().await,
    }
}

async fn serve() -> Result<()> {
    anyhow::bail!("daemon not yet implemented — see ARCHITECTURE.md §3")
}

async fn mcp() -> Result<()> {
    anyhow::bail!("mcp stub not yet implemented — see ARCHITECTURE.md §4")
}

async fn tail(_json: bool) -> Result<()> {
    anyhow::bail!("tail not yet implemented — see ARCHITECTURE.md §11")
}

async fn send(_to: String, _body: String) -> Result<()> {
    anyhow::bail!("send not yet implemented")
}

async fn say(_body: String) -> Result<()> {
    anyhow::bail!("say not yet implemented")
}

async fn participants() -> Result<()> {
    anyhow::bail!("participants not yet implemented")
}

async fn history(_channel: Option<String>, _with: Option<String>, _limit: usize) -> Result<()> {
    anyhow::bail!("history not yet implemented")
}

async fn pause() -> Result<()> {
    anyhow::bail!("pause not yet implemented")
}

async fn resume() -> Result<()> {
    anyhow::bail!("resume not yet implemented")
}
