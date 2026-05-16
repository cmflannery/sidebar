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

    /// Schedule a delayed send. Either --in or --at must be set.
    Schedule {
        /// Recipient: `@name`, `#channel`, or `*`.
        #[arg(long)]
        to: String,
        /// Body.
        body: String,
        /// Send N seconds from now.
        #[arg(long = "in", value_name = "SECONDS")]
        in_seconds: Option<u64>,
        /// Send at an ISO-8601 timestamp (UTC).
        #[arg(long)]
        at: Option<String>,
        /// Speak as this agent (default: master).
        #[arg(long = "as", default_value = "master")]
        as_name: String,
    },

    /// Read unread messages addressed to an agent (default: `master`).
    /// Marks messages as read on return.
    Inbox {
        /// Speak as this agent.
        #[arg(long = "as", default_value = "master")]
        as_name: String,
        /// Long-poll up to this many milliseconds for new messages.
        #[arg(long)]
        wait_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },

    /// Broadcast a message as `master`.
    Say { body: String },

    /// List known participants (names only).
    Participants {
        #[arg(long)]
        json: bool,
    },

    /// Table view of agents with first/last-seen times.
    Agents {
        /// Include agents not seen in the last 7 days.
        #[arg(long)]
        all: bool,
        /// Emit a JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Print history (channel or DM thread).
    History {
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        with: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Search message bodies for a substring (case-insensitive).
    Grep {
        /// Substring to look for.
        query: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Hold new message delivery.
    Pause,

    /// Resume message delivery.
    Resume,

    /// Show daemon health, counts, and paths.
    Status {
        /// Emit JSON instead of a key/value table.
        #[arg(long)]
        json: bool,
    },
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
