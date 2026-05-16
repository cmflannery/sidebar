use anyhow::Result;
use clap::{Parser, Subcommand};

mod cli;
mod client;
mod daemon;
mod mcp;
mod paths;
mod proto;
mod types;

#[derive(Parser)]
#[command(
    name = "sidebar",
    version,
    about = "Local MCP server for inter-agent messaging"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon (long-lived). Owns SQLite + broker + scheduler.
    Serve,

    /// MCP stdio stub. Wire this into Claude Code / Codex MCP config.
    Mcp {
        /// Agent name to register with the daemon. Falls back to
        /// $SIDEBAR_AGENT_NAME, then `agent-<pid>`.
        #[arg(long = "as", value_name = "NAME")]
        as_name: Option<String>,
    },

    /// Stream messages live to terminal.
    Tail {
        #[arg(long)]
        json: bool,
    },

    /// Send a message as `master` to an agent or channel.
    Send { to: String, body: String },

    /// Broadcast a message as `master`.
    Say { body: String },

    /// List known participants.
    Participants,

    /// Print history (channel or DM thread).
    History {
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        with: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Hold new message delivery.
    Pause,

    /// Resume message delivery.
    Resume,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sidebar=info".into()),
        )
        .init();

    let cli = Cli::parse();
    cli::dispatch(cli.command).await
}
