//! Interactive REPL — what you get when you type `sidebar` with no
//! subcommand. Auto-starts the daemon if one isn't running; otherwise
//! joins the existing one. Slash commands map to the same operations
//! as the one-shot CLI subcommands.

use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use rustyline::DefaultEditor;
use rustyline::ExternalPrinter;
use rustyline::error::ReadlineError;

use crate::client::Client;
use crate::paths;
use crate::proto::{Event, Hello, Op, Request, ResponseData};
use crate::types::Recipient;

// --- ANSI helpers -----------------------------------------------------------
// Only emit escapes when stdout is a TTY and NO_COLOR isn't set.
fn use_color() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
fn paint(prefix: &str, s: &str) -> String {
    if use_color() {
        format!("{prefix}{s}{RESET}")
    } else {
        s.to_string()
    }
}

// Per-name colors so each agent reads distinctly in the tail. Stable hash
// over the bytes picks a hue on the color wheel, emitted as truecolor —
// scales to arbitrarily many agents (collisions become near-miss hues
// instead of identical palette slots). Hues near pure red (0°/360°) are
// skipped so we don't visually clash with error red.
fn name_color(name: &str) -> String {
    // FNV-1a, then a finalize pass — FNV's high bits mix poorly on short
    // ASCII, so without the avalanche the modulo distribution skews badly
    // (every "claude-*" lands on the same hue).
    let mut h: u32 = 2_166_136_261;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16_777_619);
    }
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;

    // Map to a 320° arc starting at 20° (skip the ±20° wedge around red).
    let hue_deg: u16 = 20 + u16::try_from(h % 320).unwrap_or(0);
    let (r, g, b) = hsl_to_rgb(hue_deg, 0.62, 0.62);
    format!("\x1b[38;2;{r};{g};{b}m")
}
// HSL→RGB with saturation/lightness in [0,1] and hue in degrees ∈ [0, 360).
// Standard formula; variable names follow Wikipedia's `Hsl_and_hsv` page.
fn hsl_to_rgb(hue_deg: u16, sat: f32, light: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * light - 1.0).abs()) * sat;
    // Sector picked from integer math so we dodge f32→u32 cast lints.
    let sector = (hue_deg / 60) % 6;
    let hue_prime = f32::from(hue_deg) / 60.0;
    let second = chroma * (1.0 - (hue_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match sector {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let lightness_shift = light - chroma / 2.0;
    // Value is clamped to [0, 255] before the cast — narrow allow with reason.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is clamped to [0, 255] immediately before the cast"
    )]
    let to_u8 = |v: f32| ((v + lightness_shift) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}
fn paint_name(name: &str) -> String {
    if use_color() {
        format!("{BOLD}{}@{name}{RESET}", name_color(name))
    } else {
        format!("@{name}")
    }
}
// Minimal markdown renderer for message bodies. Covers what people actually
// type into chat:
//   `code`           inline code         — magenta
//   **bold**         bold                — ANSI bold
//   ```...```        fenced code block   — dim, lightly indented
//   [text](url)      link                — underlined text, URL dropped
// Skips emphasis (single `*`), headings, lists, tables — chat doesn't use
// them and they'd be jarring inline. NO_COLOR / non-TTY: returns input
// unchanged so terminal recordings stay clean.
const UNDERLINE: &str = "\x1b[4m";
const MAGENTA: &str = "\x1b[35m";
fn render_markdown(body: &str) -> String {
    render_markdown_inner(body, use_color())
}

fn render_markdown_inner(body: &str, color: bool) -> String {
    if !color {
        return body.to_string();
    }
    let paint = |prefix: &str, s: &str| format!("{prefix}{s}{RESET}");
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        // Triple-backtick fenced block (greedy, multi-line).
        if i + 3 <= chars.len() && chars[i..i + 3] == ['`', '`', '`'] {
            if let Some(end) = find_run(&chars, i + 3, '`', 3) {
                let inner: String = chars[i + 3..end].iter().collect();
                let trimmed = inner.trim_matches('\n');
                out.push('\n');
                for line in trimmed.lines() {
                    out.push_str(&paint(DIM, &format!("    {line}")));
                    out.push('\n');
                }
                i = end + 3;
                continue;
            }
        }
        // Inline `code`.
        if chars[i] == '`' {
            if let Some(end) = find_char(&chars, i + 1, '`') {
                let inner: String = chars[i + 1..end].iter().collect();
                out.push_str(&paint(MAGENTA, &inner));
                i = end + 1;
                continue;
            }
        }
        // **bold**.
        if i + 2 <= chars.len() && chars[i..i + 2] == ['*', '*'] {
            if let Some(end) = find_run(&chars, i + 2, '*', 2) {
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str(&paint(BOLD, &inner));
                i = end + 2;
                continue;
            }
        }
        // [text](url) → underlined text, URL hidden.
        if chars[i] == '[' {
            if let Some(close_bracket) = find_char(&chars, i + 1, ']') {
                if close_bracket + 1 < chars.len() && chars[close_bracket + 1] == '(' {
                    if let Some(close_paren) = find_char(&chars, close_bracket + 2, ')') {
                        let text: String = chars[i + 1..close_bracket].iter().collect();
                        out.push_str(&paint(UNDERLINE, &text));
                        i = close_paren + 1;
                        continue;
                    }
                }
            }
        }
        // Escape: \` \* \[ — emit the literal next char without re-parsing.
        if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == target)
        .map(|p| from + p)
}

fn find_run(chars: &[char], from: usize, target: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i + n <= chars.len() {
        if chars[i..i + n].iter().all(|&c| c == target) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn recipient_suffix(r: &Recipient) -> String {
    match r {
        Recipient::Channel(n) => paint(DIM, &format!("  (#{n})")),
        Recipient::Broadcast => paint(DIM, "  (broadcast)"),
        Recipient::Agent(_) => String::new(),
    }
}

/// REPL entry point. `identity` is the name we speak as for this session.
pub async fn run(identity: String) -> Result<()> {
    ensure_daemon().await?;
    let mut current = identity;
    let mut client = Client::connect_as(&current)
        .await
        .context("connecting to daemon")?;

    print_banner(&current);

    // Editor + background tail.
    let mut rl = DefaultEditor::new().context("rustyline init")?;
    let history_path = paths::home()?.join("repl-history");
    let _ = rl.load_history(&history_path);

    // ExternalPrinter needs a real TTY. Skip the live-tail when stdin is
    // piped (smoke tests, scripted input) — the REPL stays usable, you just
    // don't see incoming messages stream in above the prompt.
    let _tail_handle = match rl.create_external_printer() {
        Ok(printer) => Some(spawn_tail(printer)),
        Err(_) => None,
    };

    loop {
        let prompt = if use_color() {
            let c = name_color(&current);
            format!("{BOLD}{c}{current}{RESET}{DIM} ❯ {RESET}")
        } else {
            format!("{current}> ")
        };
        match rl.readline(&prompt) {
            Ok(line) => {
                let _ = rl.add_history_entry(&line);
                if line.trim().is_empty() {
                    continue;
                }
                match handle_line(&mut client, &mut current, &line).await {
                    Ok(LoopControl::Continue) => {}
                    Ok(LoopControl::Quit) => break,
                    Err(e) => println!("error: {e}"),
                }
            }
            Err(ReadlineError::Interrupted) => {} // ^C — discard partial line
            Err(ReadlineError::Eof) => break,     // ^D
            Err(e) => {
                println!("input error: {e}");
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
    println!("bye.");
    Ok(())
}

fn print_banner(current: &str) {
    // Pre-rendered block letters; keep the leading spaces.
    let logo = r"
 ___ ___ ___  ___ ___   _   ___
/ __|_ _|   \| __| _ ) / \ | _ \
\__ \| || |) | _|| _ \/ _ \|   /
|___/___|___/|___|___/_/ \_\_|_\
";
    println!("{}", paint(&format!("{BOLD}{CYAN}"), logo));
    println!(
        "{}",
        paint(
            DIM,
            &format!(
                "local MCP bus for coding agents · v{}",
                env!("CARGO_PKG_VERSION")
            )
        )
    );
    println!(
        "connected as {}. {} for commands. naked text → {}. {} to quit.",
        paint_name(current),
        paint(BOLD, "/help"),
        paint(BOLD, "#general"),
        paint(BOLD, "^D"),
    );
    println!(
        "{}",
        paint(DIM, "─────────────────────────────────────────────")
    );
}

enum LoopControl {
    Continue,
    Quit,
}

async fn handle_line(client: &mut Client, current: &mut String, line: &str) -> Result<LoopControl> {
    let line = line.trim();
    if let Some(slash) = line.strip_prefix('/') {
        let mut parts = slash.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();
        return run_slash(client, current, cmd, rest).await;
    }

    // Naked text → #general (chat default).
    let reply = client
        .request(Op::Send {
            to: "#general".to_string(),
            body: line.to_string(),
            intent: None,
            reply_to: None,
        })
        .await?;
    if !reply.ok {
        println!("send failed: {}", reply.error.unwrap_or_default());
    }
    Ok(LoopControl::Continue)
}

async fn run_slash(
    client: &mut Client,
    current: &mut String,
    cmd: &str,
    rest: &str,
) -> Result<LoopControl> {
    match cmd {
        "help" | "?" => {
            print_help();
            Ok(LoopControl::Continue)
        }
        "quit" | "q" | "exit" => Ok(LoopControl::Quit),
        "whoami" => {
            println!("{current}");
            Ok(LoopControl::Continue)
        }
        "switch" => slash_switch(client, current, rest).await,
        "send" => slash_send(client, rest).await,
        "say" => slash_say(client, rest).await,
        "inbox" => slash_inbox(client, current, rest).await,
        "history" => slash_history(client, rest).await,
        "participants" => slash_participants(client).await,
        "agents" => slash_agents(client, rest).await,
        "channels" => slash_channels(client, rest).await,
        "join" => slash_join_leave(client, current, rest, true).await,
        "leave" => slash_join_leave(client, current, rest, false).await,
        "grep" => slash_grep(client, rest).await,
        "schedule" => slash_schedule(client, rest).await,
        "scheduled" => slash_scheduled(client).await,
        "cancel" => slash_cancel(client, rest).await,
        "inspect" => slash_inspect(client, rest).await,
        "status" => slash_status(client).await,
        "pause" => slash_toggle(client, Op::Pause, "paused").await,
        "resume" => slash_toggle(client, Op::Resume, "resumed").await,
        other => {
            println!("unknown command: /{other}. /help for the list.");
            Ok(LoopControl::Continue)
        }
    }
}

fn print_help() {
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "session",
            &[
                ("/help", "this list"),
                ("/whoami", "show current identity"),
                ("/switch <name>", "act as a different agent"),
                ("/quit (or ^D)", "exit"),
            ],
        ),
        (
            "messages",
            &[
                (
                    "/send <to> <body>",
                    "send a message (`@name`, `#channel`, `*`)",
                ),
                ("/say <body>", "broadcast"),
                ("/inbox", "read your inbox (oldest 500 unread)"),
                ("/history --channel <name>", "channel history"),
                ("/history --with <agent>", "DM thread history"),
                ("/grep <query>", "case-insensitive substring search"),
                ("/schedule <to> <secs> <body>", "delayed send (in seconds)"),
                ("/scheduled", "list your pending scheduled rows"),
                ("/cancel <id>", "cancel a pending scheduled row"),
            ],
        ),
        (
            "channels & agents",
            &[
                ("/join <ch> [ch...]", "subscribe to channels"),
                ("/leave <ch> [ch...]", "unsubscribe"),
                ("/participants", "names of known agents"),
                ("/agents", "agents with last-seen"),
                ("/channels [--details]", "channel list"),
            ],
        ),
        (
            "operator",
            &[
                ("/status", "daemon health snapshot"),
                ("/inspect <message-id>", "per-recipient delivery state"),
                ("/pause", "stop the bus"),
                ("/resume", "release"),
            ],
        ),
    ];
    for (heading, items) in groups {
        println!(
            "\n{}",
            paint(&format!("{BOLD}{CYAN}"), &format!("{heading}:"))
        );
        for (cmd, desc) in *items {
            println!("  {:<32}  {}", paint(BOLD, cmd), paint(DIM, desc));
        }
    }
    println!(
        "\n{}",
        paint(
            DIM,
            "anything not starting with / is broadcast to #general."
        )
    );
}

async fn slash_switch(
    client: &mut Client,
    current: &mut String,
    rest: &str,
) -> Result<LoopControl> {
    let name = rest.trim();
    if name.is_empty() {
        println!("usage: /switch <agent-name>");
        return Ok(LoopControl::Continue);
    }
    *client = Client::connect_as(name)
        .await
        .context("reconnect after /switch")?;
    *current = name.to_string();
    println!("acting as {current} now.");
    Ok(LoopControl::Continue)
}

async fn slash_send(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let (to, body) = match rest.split_once(char::is_whitespace) {
        Some((t, b)) if !b.trim().is_empty() => (t.to_string(), b.trim().to_string()),
        _ => {
            println!("usage: /send <to> <body>");
            return Ok(LoopControl::Continue);
        }
    };
    let reply = client
        .request(Op::Send {
            to,
            body,
            intent: None,
            reply_to: None,
        })
        .await?;
    if reply.ok {
        if let Some(ResponseData::SendOk { message_id }) = reply.data {
            println!("sent (id {message_id})");
        }
    } else {
        println!("send failed: {}", reply.error.unwrap_or_default());
    }
    Ok(LoopControl::Continue)
}

async fn slash_say(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let body = rest.trim();
    if body.is_empty() {
        println!("usage: /say <body>");
        return Ok(LoopControl::Continue);
    }
    let reply = client
        .request(Op::Send {
            to: "*".to_string(),
            body: body.to_string(),
            intent: None,
            reply_to: None,
        })
        .await?;
    if !reply.ok {
        println!("broadcast failed: {}", reply.error.unwrap_or_default());
    }
    Ok(LoopControl::Continue)
}

async fn slash_inbox(
    client: &mut Client,
    current: &mut String,
    _rest: &str,
) -> Result<LoopControl> {
    let _ = current; // identity is implicit in the connected client
    let reply = client
        .request(Op::Inbox {
            wait_ms: None,
            mentions_only: false,
        })
        .await?;
    let Some(ResponseData::Messages { messages }) = reply.data else {
        println!("(daemon error: {})", reply.error.unwrap_or_default());
        return Ok(LoopControl::Continue);
    };
    if messages.is_empty() {
        println!("(empty)");
    } else {
        for m in messages {
            print_message_line(&m);
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_history(client: &mut Client, rest: &str) -> Result<LoopControl> {
    // tiny parser: --channel X | --with Y, optional --limit N
    let mut channel = None;
    let mut with = None;
    let mut limit: usize = 50;
    let mut iter = rest.split_whitespace();
    while let Some(tok) = iter.next() {
        match tok {
            "--channel" => channel = iter.next().map(str::to_string),
            "--with" => with = iter.next().map(str::to_string),
            "--limit" => {
                if let Some(n) = iter.next().and_then(|s| s.parse().ok()) {
                    limit = n;
                }
            }
            _ => {}
        }
    }
    if channel.is_none() && with.is_none() {
        println!("usage: /history --channel <name>  OR  /history --with <agent>");
        return Ok(LoopControl::Continue);
    }
    let reply = client
        .request(Op::History {
            channel,
            with,
            limit,
        })
        .await?;
    if let Some(ResponseData::Messages { messages }) = reply.data {
        for m in messages {
            print_message_line(&m);
        }
    } else if let Some(err) = reply.error {
        println!("({err})");
    }
    Ok(LoopControl::Continue)
}

async fn slash_participants(client: &mut Client) -> Result<LoopControl> {
    let reply = client.request(Op::Participants).await?;
    if let Some(ResponseData::Agents { agents }) = reply.data {
        for a in agents {
            println!("  {a}");
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_agents(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let include_stale = rest.contains("--all");
    let reply = client.request(Op::Agents { include_stale }).await?;
    let Some(ResponseData::AgentsDetailed { agents_detailed }) = reply.data else {
        println!("(error)");
        return Ok(LoopControl::Continue);
    };
    if agents_detailed.is_empty() {
        println!("(no agents seen in the last 7 days; /agents --all to include stale)");
        return Ok(LoopControl::Continue);
    }
    let now = chrono::Utc::now();
    let w = agents_detailed
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(4);
    println!("{:<w$}  last seen", "NAME");
    for a in agents_detailed {
        let delta = now.signed_duration_since(a.last_seen);
        println!("{:<w$}  {}", a.name, relative(delta));
    }
    Ok(LoopControl::Continue)
}

async fn slash_channels(client: &mut Client, rest: &str) -> Result<LoopControl> {
    if rest.contains("--details") {
        let reply = client.request(Op::ChannelsDetailed).await?;
        if let Some(ResponseData::ChannelsDetailed { channels_detailed }) = reply.data {
            if channels_detailed.is_empty() {
                println!("(no channels)");
                return Ok(LoopControl::Continue);
            }
            let now = chrono::Utc::now();
            let w = channels_detailed
                .iter()
                .map(|c| c.name.len() + 1)
                .max()
                .unwrap_or(8);
            println!("{:<w$}  members  last activity", "CHANNEL");
            for c in channels_detailed {
                let last = c.last_message_at.map_or_else(
                    || "—".to_string(),
                    |t| relative(now.signed_duration_since(t)),
                );
                println!(
                    "{:<w$}  {:>7}  {last}",
                    format!("#{}", c.name),
                    c.member_count
                );
            }
        }
    } else {
        let reply = client.request(Op::Channels).await?;
        if let Some(ResponseData::Channels { channels }) = reply.data {
            for c in channels {
                println!("#{c}");
            }
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_join_leave(
    client: &mut Client,
    current: &mut String,
    rest: &str,
    join: bool,
) -> Result<LoopControl> {
    let names: Vec<&str> = rest.split_whitespace().collect();
    if names.is_empty() {
        println!(
            "usage: /{} <channel> [channel...]",
            if join { "join" } else { "leave" }
        );
        return Ok(LoopControl::Continue);
    }
    for raw in names {
        let channel = raw.trim_start_matches('#').to_string();
        let op = if join {
            Op::Join {
                channel: channel.clone(),
            }
        } else {
            Op::Leave {
                channel: channel.clone(),
            }
        };
        let reply = client.request(op).await?;
        if reply.ok {
            println!(
                "{current} {} #{channel}",
                if join { "joined" } else { "left" }
            );
        } else {
            println!(
                "({} #{channel} failed: {})",
                if join { "join" } else { "leave" },
                reply.error.unwrap_or_default()
            );
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_grep(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let query = rest.trim();
    if query.is_empty() {
        println!("usage: /grep <substring>");
        return Ok(LoopControl::Continue);
    }
    let reply = client
        .request(Op::Search {
            query: query.to_string(),
            limit: 50,
        })
        .await?;
    if let Some(ResponseData::Messages { messages }) = reply.data {
        if messages.is_empty() {
            println!("(no matches)");
        } else {
            for m in messages {
                print_message_line(&m);
            }
        }
    } else if let Some(err) = reply.error {
        println!("({err})");
    }
    Ok(LoopControl::Continue)
}

async fn slash_schedule(client: &mut Client, rest: &str) -> Result<LoopControl> {
    // Minimal form: /schedule <to> <seconds> <body...>
    let mut iter = rest.splitn(3, char::is_whitespace);
    let (to, secs, body) = match (iter.next(), iter.next(), iter.next()) {
        (Some(t), Some(s), Some(b)) if !b.trim().is_empty() => (t, s, b.trim()),
        _ => {
            println!("usage: /schedule <to> <seconds> <body>");
            return Ok(LoopControl::Continue);
        }
    };
    let Ok(delay): Result<u64, _> = secs.parse() else {
        println!("seconds must be a non-negative integer");
        return Ok(LoopControl::Continue);
    };
    let reply = client
        .request(Op::Schedule {
            to: to.to_string(),
            body: body.to_string(),
            when: crate::proto::When::DelaySeconds {
                delay_seconds: delay,
            },
        })
        .await?;
    if let Some(ResponseData::SendOk { message_id }) = reply.data {
        println!("scheduled id {message_id}");
    } else if let Some(err) = reply.error {
        println!("(error: {err})");
    }
    Ok(LoopControl::Continue)
}

async fn slash_scheduled(client: &mut Client) -> Result<LoopControl> {
    let reply = client.request(Op::Scheduled).await?;
    if let Some(ResponseData::Scheduled { scheduled }) = reply.data {
        if scheduled.is_empty() {
            println!("(no pending scheduled messages)");
            return Ok(LoopControl::Continue);
        }
        println!("ID    FIRES                       FROM → TO         BODY");
        for s in scheduled {
            let when = s
                .deliver_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S");
            let preview: String = s.body.chars().take(40).collect();
            println!("{:<5} {when}  {} → {}  {preview}", s.id, s.from, s.to);
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_cancel(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let Ok(id) = rest.trim().parse::<i64>() else {
        println!("usage: /cancel <scheduled-id>");
        return Ok(LoopControl::Continue);
    };
    let reply = client.request(Op::Cancel { scheduled_id: id }).await?;
    if reply.ok {
        println!("cancelled scheduled id {id}");
    } else {
        println!("({})", reply.error.unwrap_or_default());
    }
    Ok(LoopControl::Continue)
}

async fn slash_inspect(client: &mut Client, rest: &str) -> Result<LoopControl> {
    let Ok(id) = rest.trim().parse::<i64>() else {
        println!("usage: /inspect <message-id>");
        return Ok(LoopControl::Continue);
    };
    let reply = client.request(Op::Inspect { message_id: id }).await?;
    let Some(ResponseData::MessageDetail(d)) = reply.data else {
        println!("({})", reply.error.unwrap_or_default());
        return Ok(LoopControl::Continue);
    };
    let m = &d.message;
    let to_label = recipient_label(&m.to);
    let ts = m
        .created_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S");
    println!("message {} — {} → {to_label} at {ts}", m.id, m.from);
    println!("body:");
    for l in m.body.lines() {
        println!("  {l}");
    }
    if !d.deliveries.is_empty() {
        let w = d
            .deliveries
            .iter()
            .map(|x| x.agent.len())
            .max()
            .unwrap_or(5);
        println!("deliveries:");
        for d2 in &d.deliveries {
            let delivered = d2.delivered_at.map_or_else(
                || "(undelivered)".to_string(),
                |t| {
                    t.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                },
            );
            let read = d2.read_at.map_or_else(
                || "unread".to_string(),
                |t| {
                    format!(
                        "read {}",
                        t.with_timezone(&chrono::Local).format("%H:%M:%S")
                    )
                },
            );
            println!("  {:<w$}  delivered {delivered}  {read}", d2.agent);
        }
    }
    Ok(LoopControl::Continue)
}

async fn slash_status(client: &mut Client) -> Result<LoopControl> {
    let reply = client.request(Op::Status).await?;
    let Some(ResponseData::Status(s)) = reply.data else {
        println!("(error)");
        return Ok(LoopControl::Continue);
    };
    let h = s.uptime_seconds / 3600;
    let m = (s.uptime_seconds % 3600) / 60;
    let sec = s.uptime_seconds % 60;
    println!(
        "uptime {h}h {m}m {sec}s · paused {} · agents {} · channels {} · unread {} · scheduled {}",
        s.paused, s.agent_count, s.channel_count, s.unread_count, s.pending_scheduled
    );
    Ok(LoopControl::Continue)
}

async fn slash_toggle(client: &mut Client, op: Op, label: &str) -> Result<LoopControl> {
    let reply = client.request(op).await?;
    if reply.ok {
        println!("{label}");
    } else {
        println!("(error: {})", reply.error.unwrap_or_default());
    }
    Ok(LoopControl::Continue)
}

// ---- background tail ----

fn spawn_tail(mut printer: impl ExternalPrinter + Send + 'static) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = tail_loop(&mut printer).await {
            let _ = printer.print(format!("(tail subscriber stopped: {e})\n"));
        }
    })
}

async fn tail_loop(printer: &mut (impl ExternalPrinter + Send)) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let path = paths::socket()?;
    let stream = UnixStream::connect(&path).await?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read).lines();

    let mut hello = serde_json::to_vec(&Hello::Cli {
        speaking_as: "repl-tail".to_string(),
    })?;
    hello.push(b'\n');
    write.write_all(&hello).await?;

    let mut req = serde_json::to_vec(&Request {
        id: 1,
        op: Op::Subscribe,
    })?;
    req.push(b'\n');
    write.write_all(&req).await?;

    while let Some(line) = reader.next_line().await? {
        let evt: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue, // HelloAck, Subscribe ack, etc.
        };
        let now = chrono::Local::now().format("%H:%M:%S");
        let ts = paint(DIM, &format!("[{now}]"));
        let rendered = match evt {
            Event::Message { to, from, body, .. } => format!(
                "{ts} {}: {}{}\n",
                paint_name(&from),
                render_markdown(&body),
                recipient_suffix(&to)
            ),
            Event::Paused => format!("{ts} {}\n", paint(YELLOW, "(paused)")),
            Event::Resumed => format!("{ts} {}\n", paint(YELLOW, "(resumed)")),
        };
        let _ = printer.print(rendered);
    }
    Ok(())
}

// ---- formatting helpers ----

fn print_message_line(m: &crate::types::Message) {
    let ts = m
        .created_at
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S");
    println!(
        "{} {}: {}{}",
        paint(DIM, &format!("[{ts}]")),
        paint_name(&m.from),
        render_markdown(&m.body),
        recipient_suffix(&m.to),
    );
}

fn recipient_label(r: &Recipient) -> String {
    match r {
        Recipient::Agent(n) => format!("@{n}"),
        Recipient::Channel(n) => format!("#{n}"),
        Recipient::Broadcast => "*".to_string(),
    }
}

fn relative(d: chrono::Duration) -> String {
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

// ---- daemon auto-start ----

async fn ensure_daemon() -> Result<()> {
    if Client::connect_as("master").await.is_ok() {
        return Ok(());
    }

    let socket = paths::socket()?;
    let log = paths::ensure_home()?.join("daemon.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening daemon log at {}", log.display()))?;

    let exe = std::env::current_exe().context("locating sidebar binary")?;
    let mut child = std::process::Command::new(&exe)
        .arg("serve")
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .with_context(|| format!("spawning `{} serve`", exe.display()))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if Client::connect_as("master").await.is_ok() {
            std::mem::forget(child);
            println!("(started a sidebar daemon — logging to {})", log.display());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(75)).await;
    }

    let _ = child.kill();
    let _ = child.wait();
    anyhow::bail!(
        "started `{} serve` but couldn't connect within 3s — check {} for errors",
        exe.display(),
        socket.display(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_passthrough() {
        assert_eq!(
            render_markdown_inner("hello **world**", false),
            "hello **world**"
        );
    }

    #[test]
    fn inline_code_is_magenta() {
        let out = render_markdown_inner("run `sidebar serve` now", true);
        assert!(out.contains("\x1b[35msidebar serve\x1b[0m"), "got: {out:?}");
    }

    #[test]
    fn bold_is_bolded() {
        let out = render_markdown_inner("the **important** bit", true);
        assert!(out.contains("\x1b[1mimportant\x1b[0m"), "got: {out:?}");
    }

    #[test]
    fn link_text_is_underlined_url_hidden() {
        let out = render_markdown_inner("see [docs](https://example.com) here", true);
        assert!(out.contains("\x1b[4mdocs\x1b[0m"), "got: {out:?}");
        assert!(
            !out.contains("example.com"),
            "url should be hidden: {out:?}"
        );
    }

    #[test]
    fn fenced_block_each_line_dim_indented() {
        let out = render_markdown_inner("before\n```\nline1\nline2\n```\nafter", true);
        assert!(out.contains("\x1b[2m    line1\x1b[0m"), "got: {out:?}");
        assert!(out.contains("\x1b[2m    line2\x1b[0m"), "got: {out:?}");
    }

    #[test]
    fn unclosed_backtick_left_alone() {
        let out = render_markdown_inner("a ` lonely backtick", true);
        assert_eq!(out, "a ` lonely backtick");
    }

    #[test]
    fn escaped_backtick_renders_literal() {
        let out = render_markdown_inner(r"escaped \` here", true);
        assert_eq!(out, "escaped ` here");
    }

    #[test]
    fn empty_input() {
        assert_eq!(render_markdown_inner("", true), "");
    }
}
