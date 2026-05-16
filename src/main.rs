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
        /// Print only events whose default-format line contains this
        /// substring (case-insensitive). Useful for `--filter @alice`
        /// to watch only messages mentioning alice, or `#standup`
        /// to scope to a channel. Ignored when --json is set.
        #[arg(long)]
        filter: Option<String>,
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
        /// Only return messages where the agent is explicitly addressed
        /// (DMs to them or channel/broadcast messages with their @-mention).
        #[arg(long)]
        mentions_only: bool,
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

    /// List channels. With --details, show member counts and last activity.
    Channels {
        #[arg(long)]
        details: bool,
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

    /// Subscribe an agent to one or more channels (leading `#` is tolerated).
    Join {
        /// Channel names. `sidebar join standup deploys releases`.
        #[arg(required = true)]
        channels: Vec<String>,
        /// Agent to subscribe (default: master).
        #[arg(long = "as", default_value = "master")]
        as_name: String,
    },

    /// Unsubscribe an agent from one or more channels.
    Leave {
        #[arg(required = true)]
        channels: Vec<String>,
        #[arg(long = "as", default_value = "master")]
        as_name: String,
    },

    /// Operator debug: print a single message with its per-recipient
    /// delivery state (delivered/read timestamps per agent).
    Inspect {
        message_id: i64,
        #[arg(long)]
        json: bool,
    },

    /// List pending scheduled messages (master sees all; --as <name>
    /// scopes to that agent's own).
    Scheduled {
        #[arg(long = "as", default_value = "master")]
        as_name: String,
        #[arg(long)]
        json: bool,
    },

    /// Cancel a pending scheduled message by id. Master can cancel any;
    /// otherwise --as must match the agent that scheduled it.
    Cancel {
        scheduled_id: i64,
        #[arg(long = "as", default_value = "master")]
        as_name: String,
    },

    /// Drop agent rows inactive for N days that have no messages either
    /// from or to them. Master is never pruned.
    Prune {
        #[arg(long, default_value_t = 30)]
        inactive_days: i64,
        /// List what would be pruned without deleting.
        #[arg(long)]
        dry_run: bool,
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
