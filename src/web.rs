//! Local browser console for the Mac mini pilot.
//!
//! This is intentionally a small localhost-only adapter over the existing
//! Unix-socket protocol. It keeps the browser surface independent from the
//! daemon internals while we validate the room, presence, and delivery UX
//! before adding cloud authentication and realtime infrastructure.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::client::Client;
use crate::proto::Op;
use crate::types::Intent;

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Start the loopback web console. The address is intentionally supplied by
/// the CLI rather than read from a config file so the local-only boundary is
/// obvious when the pilot is launched.
pub async fn serve(bind: &str) -> Result<()> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid web bind address `{bind}`"))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding local web console at {addr}"))?;
    let actual = listener.local_addr()?;

    println!("sidebar web listening on http://{actual}");
    tracing::info!(address = %actual, "local browser console listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream).await {
                tracing::debug!(%peer, error = %error, "web request ended with error");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream) -> Result<()> {
    let request = read_request(&mut stream).await?;
    let response = route(request).await;
    write_response(&mut stream, response).await
}

async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            anyhow::bail!("client closed before sending an HTTP request");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("HTTP request exceeds {MAX_REQUEST_BYTES} bytes");
        }
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let (method, target, content_length) = {
        let header_text = std::str::from_utf8(&bytes[..header_end - 4])
            .context("HTTP request headers are not UTF-8")?;
        let mut lines = header_text.split("\r\n");
        let request_line = lines
            .next()
            .context("HTTP request is missing a request line")?;
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts
            .next()
            .context("HTTP request is missing a method")?
            .to_ascii_uppercase();
        let target = request_parts
            .next()
            .context("HTTP request is missing a target")?
            .to_string();
        let version = request_parts.next().unwrap_or_default();
        if version != "HTTP/1.1" && version != "HTTP/1.0" {
            anyhow::bail!("unsupported HTTP version `{version}`");
        }

        let mut content_length = 0_usize;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value
                    .trim()
                    .parse()
                    .context("invalid Content-Length header")?;
            }
        }
        (method, target, content_length)
    };
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("request body exceeds {MAX_BODY_BYTES} bytes");
    }

    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            anyhow::bail!("client closed before sending the full request body");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("HTTP request exceeds {MAX_REQUEST_BYTES} bytes");
        }
    }

    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Ok(HttpRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

async fn route(request: HttpRequest) -> HttpResponse {
    if request.method == "GET" {
        if let Some(message_id) = request
            .path
            .strip_prefix("/api/messages/")
            .and_then(|value| value.parse::<i64>().ok())
        {
            return daemon_json(Op::Inspect { message_id }).await;
        }
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => HttpResponse::html(APP_HTML),
        ("GET", "/healthz") => HttpResponse::text("ok\n"),
        ("GET", "/api/status") => daemon_json(Op::Status).await,
        ("GET", "/api/agents") => {
            daemon_json(Op::Agents {
                include_stale: true,
            })
            .await
        }
        ("GET", "/api/channels") => daemon_json(Op::ChannelsDetailed).await,
        ("GET", "/api/messages") => history_response(&request.query).await,
        ("POST", "/api/messages") => send_response(&request.body).await,
        _ => HttpResponse::not_found(),
    }
}

async fn history_response(query: &str) -> HttpResponse {
    let channel = query_param(query, "channel").unwrap_or_else(|| "general".to_string());
    let limit = query_param(query, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    let include_delivery = query_param(query, "include_delivery").as_deref() == Some("1");
    if include_delivery {
        daemon_json(Op::HistoryDetailed { channel, limit }).await
    } else {
        daemon_json(Op::History {
            channel: Some(channel),
            with: None,
            limit,
        })
        .await
    }
}

#[derive(Debug, Deserialize)]
struct SendRequest {
    to: String,
    body: String,
    #[serde(default)]
    intent: Option<Intent>,
    #[serde(default)]
    reply_to: Option<i64>,
}

async fn send_response(body: &[u8]) -> HttpResponse {
    let request: SendRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            return HttpResponse::json(
                400,
                &serde_json::json!({"ok": false, "error": format!("invalid JSON body: {error}")}),
            );
        }
    };
    if request.to.trim().is_empty() || request.body.trim().is_empty() {
        return HttpResponse::json(
            400,
            &serde_json::json!({"ok": false, "error": "recipient and body are required"}),
        );
    }
    daemon_json(Op::Send {
        to: request.to,
        body: request.body,
        intent: request.intent,
        reply_to: request.reply_to,
    })
    .await
}

async fn daemon_json(op: Op) -> HttpResponse {
    let mut client = match Client::connect_as("master").await {
        Ok(client) => client,
        Err(error) => {
            return HttpResponse::json(
                503,
                &serde_json::json!({
                    "ok": false,
                    "error": format!("sidebar daemon unavailable: {error}")
                }),
            );
        }
    };
    match client.request(op).await {
        Ok(response) => {
            let status = if response.ok { 200 } else { 400 };
            HttpResponse::json(
                status,
                &serde_json::to_value(response).unwrap_or_else(|error| {
                    serde_json::json!({"ok": false, "error": format!("serialize daemon response: {error}")})
                }),
            )
        }
        Err(error) => HttpResponse::json(
            502,
            &serde_json::json!({"ok": false, "error": format!("daemon request failed: {error}")}),
        ),
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    fn text(body: &str) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/plain; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }

    fn html(body: &str) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }

    fn json(status: u16, value: &serde_json::Value) -> Self {
        let (reason, _) = status_reason(status);
        Self {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(value).unwrap_or_else(|_| b"{\"ok\":false}".to_vec()),
        }
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            reason: "Not Found",
            content_type: "text/plain; charset=utf-8",
            body: b"not found\n".to_vec(),
        }
    }
}

async fn write_response(stream: &mut TcpStream, response: HttpResponse) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn status_reason(status: u16) -> (&'static str, u16) {
    match status {
        200 => ("OK", status),
        400 => ("Bad Request", status),
        404 => ("Not Found", status),
        502 => ("Bad Gateway", status),
        503 => ("Service Unavailable", status),
        _ => ("Internal Server Error", status),
    }
}

fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (percent_decode(name) == wanted).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1]);
                let low = hex_value(bytes[index + 2]);
                match (high, low) {
                    (Some(high), Some(low)) => {
                        decoded.push((high << 4) | low);
                        index += 2;
                    }
                    _ => decoded.push(bytes[index]),
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const APP_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sidebar — agent room</title>
  <style>
    :root { color-scheme: dark; --bg:#101114; --panel:#17191e; --panel-2:#1d2026; --line:#2b2f37; --text:#eef0f3; --muted:#9299a6; --accent:#91b8ff; --green:#77d6a3; --yellow:#e4c46d; --red:#ff8c8c; }
    * { box-sizing:border-box; }
    body { margin:0; min-height:100vh; background:var(--bg); color:var(--text); font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
    button, input, select { font:inherit; }
    button { cursor:pointer; }
    .app { display:grid; grid-template-columns:220px minmax(420px,1fr) 240px; min-height:100vh; }
    .sidebar, .details { background:var(--panel); padding:22px 16px; }
    .sidebar { border-right:1px solid var(--line); }
    .details { border-left:1px solid var(--line); }
    .brand { display:flex; gap:10px; align-items:center; margin:0 8px 26px; }
    .mark { width:28px; height:28px; border-radius:9px; background:linear-gradient(135deg,#a8c7ff,#765cf4); box-shadow:0 0 24px #765cf455; }
    h1 { margin:0; font-size:16px; letter-spacing:.01em; }
    h2 { margin:0; font-size:15px; }
    h3 { margin:24px 8px 8px; color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.12em; }
    .eyebrow { margin:0 0 3px; color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.12em; }
    .pilot { margin:0 8px 18px; color:var(--muted); font-size:12px; }
    .nav-list { display:grid; gap:3px; }
    .nav-item, .agent { width:100%; border:0; border-radius:8px; background:transparent; color:var(--text); padding:9px 10px; text-align:left; }
    .nav-item:hover, .nav-item.active { background:var(--panel-2); }
    .nav-item { display:flex; justify-content:space-between; align-items:center; }
    .count { color:var(--muted); font-size:11px; }
    .agent { display:flex; align-items:center; gap:9px; padding:8px; }
    .agent-name { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
    .dot { width:8px; height:8px; border-radius:50%; background:var(--muted); flex:0 0 auto; }
    .dot.online { background:var(--green); box-shadow:0 0 9px #77d6a388; }
    .dot.recent { background:var(--yellow); }
    .main { display:flex; min-width:0; flex-direction:column; background:radial-gradient(circle at 50% -20%,#252a3a 0,#101114 42%); }
    .topbar { display:flex; align-items:center; justify-content:space-between; border-bottom:1px solid var(--line); padding:20px 26px 17px; }
    .room-title { display:flex; align-items:center; gap:10px; }
    .hash { color:var(--accent); font-size:23px; }
    .connection { display:flex; align-items:center; gap:7px; color:var(--muted); font-size:12px; }
    .refresh { border:1px solid var(--line); border-radius:7px; background:var(--panel); color:var(--muted); padding:5px 9px; }
    .refresh:hover { color:var(--text); border-color:#4e5665; }
    .messages { display:flex; flex:1; min-height:0; flex-direction:column; gap:17px; overflow:auto; padding:28px 8%; }
    .empty { margin:auto; color:var(--muted); text-align:center; }
    .empty strong { display:block; margin-bottom:5px; color:var(--text); font-size:16px; }
    .message { display:grid; grid-template-columns:34px minmax(0,1fr); gap:11px; }
    .avatar { display:grid; width:32px; height:32px; place-items:center; border:1px solid #45506a; border-radius:10px; background:#232a3b; color:var(--accent); font-weight:700; font-size:12px; }
    .message-head { display:flex; align-items:baseline; gap:9px; }
    .sender { font-weight:650; }
    .time { color:var(--muted); font-size:11px; }
    .message-body { margin-top:3px; color:#d7dae0; white-space:pre-wrap; overflow-wrap:anywhere; }
    .message-meta { display:flex; gap:7px; margin-top:5px; color:var(--muted); font-size:11px; }
    .tag { border:1px solid var(--line); border-radius:999px; padding:1px 7px; }
    .composer-wrap { border-top:1px solid var(--line); padding:17px 8% 23px; }
    .composer { display:grid; grid-template-columns:1fr auto; gap:9px; border:1px solid #3b414c; border-radius:12px; background:var(--panel); padding:10px; }
    textarea { width:100%; min-height:52px; resize:vertical; border:0; outline:0; background:transparent; color:var(--text); }
    .composer-actions { display:flex; align-items:end; gap:8px; }
    select { border:1px solid var(--line); border-radius:7px; background:var(--panel-2); color:var(--muted); padding:7px; }
    .send { border:0; border-radius:7px; background:var(--accent); color:#101114; padding:8px 14px; font-weight:700; }
    .send:disabled { cursor:wait; opacity:.6; }
    .hint { margin:7px 3px 0; color:var(--muted); font-size:11px; }
    .detail-block { border-bottom:1px solid var(--line); padding:0 0 18px; }
    .detail-block + .detail-block { padding-top:19px; }
    .detail-label { color:var(--muted); font-size:11px; text-transform:uppercase; letter-spacing:.1em; }
    .status-row { display:flex; justify-content:space-between; margin-top:9px; color:var(--muted); font-size:12px; }
    .status-value { color:var(--text); text-align:right; }
    @media (max-width: 900px) { .app { grid-template-columns:190px minmax(0,1fr); } .details { display:none; } .messages, .composer-wrap { padding-left:5%; padding-right:5%; } }
    @media (max-width: 620px) { .app { display:block; } .sidebar { display:none; } .topbar { padding-left:16px; padding-right:16px; } .messages, .composer-wrap { padding-left:16px; padding-right:16px; } }
  </style>
</head>
<body>
  <div class="app">
    <aside class="sidebar">
      <div class="brand"><div class="mark"></div><h1>Sidebar</h1></div>
      <p class="pilot">Mac mini pilot · local room</p>
      <h3>Rooms</h3>
      <div id="channels" class="nav-list"></div>
      <h3>Agents</h3>
      <div id="agents"></div>
    </aside>
    <main class="main">
      <header class="topbar">
        <div class="room-title"><span class="hash">#</span><div><p class="eyebrow">Room</p><h2 id="room-name">general</h2></div></div>
        <div class="connection"><span id="connection-dot" class="dot"></span><span id="connection-text">Connecting</span><button class="refresh" id="refresh" type="button">Refresh</button></div>
      </header>
      <section id="messages" class="messages" aria-live="polite"></section>
      <div class="composer-wrap">
        <form id="composer" class="composer">
          <textarea id="body" placeholder="Message the room… (use @agent to mention a participant)" required></textarea>
          <div class="composer-actions"><select id="intent" aria-label="Message type"><option value="">Message</option><option value="question">Question</option><option value="task">Task</option><option value="handoff">Handoff</option><option value="fyi">FYI</option></select><button class="send" type="submit">Send</button></div>
        </form>
        <div class="hint">Messages are written to the local daemon first. Delivery and agent response are separate states.</div>
      </div>
    </main>
    <aside class="details">
      <div class="detail-block"><div class="detail-label">Room status</div><div class="status-row"><span>Messages</span><span id="message-count" class="status-value">—</span></div><div class="status-row"><span>Daemon</span><span id="daemon-status" class="status-value">—</span></div></div>
      <div class="detail-block"><div class="detail-label">Pilot controls</div><div class="status-row"><span>Identity</span><span class="status-value">master</span></div><div class="status-row"><span>Transport</span><span class="status-value">Unix socket</span></div><div class="status-row"><span>Sync</span><span class="status-value">polling</span></div></div>
      <div class="detail-block"><div class="detail-label">What to try</div><p style="color:var(--muted);font-size:12px;line-height:1.6">Connect two MCP agents, mention one here, and watch the durable room transcript while the agent works in its own host.</p></div>
    </aside>
  </div>
  <script>
    const state = { channel: 'general', messages: [], agents: [], channels: [], delivery: {}, loading: false };
    const $ = (id) => document.getElementById(id);
    const escapeHtml = (value) => String(value).replace(/[&<>"']/g, (char) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
    const initials = (name) => name.slice(0, 2).toUpperCase();
    const relative = (raw) => { const seconds = Math.max(0, Math.round((Date.now() - new Date(raw).getTime()) / 1000)); if (seconds < 10) return 'just now'; if (seconds < 60) return seconds + 's ago'; const minutes = Math.round(seconds / 60); if (minutes < 60) return minutes + 'm ago'; return Math.round(minutes / 60) + 'h ago'; };
    const setConnection = (ok, text) => { $('connection-dot').className = 'dot ' + (ok ? 'online' : ''); $('connection-text').textContent = text; $('daemon-status').textContent = text; };
    async function api(path, options) { const response = await fetch(path, options); const value = await response.json(); if (!response.ok || value.ok === false) throw new Error(value.error || 'request failed'); return value; }
    function renderChannels() { $('channels').innerHTML = state.channels.map((channel) => `<button class="nav-item ${channel.name === state.channel ? 'active' : ''}" data-channel="${escapeHtml(channel.name)}"><span>#${escapeHtml(channel.name)}</span><span class="count">${channel.member_count}</span></button>`).join(''); document.querySelectorAll('[data-channel]').forEach((button) => button.addEventListener('click', () => { state.channel = button.dataset.channel; $('room-name').textContent = state.channel; renderChannels(); loadMessages(); })); }
    function renderAgents() { $('agents').innerHTML = state.agents.map((agent) => { const age = Date.now() - new Date(agent.last_seen).getTime(); const kind = agent.active_sessions > 0 ? 'online' : age < 3600000 ? 'recent' : ''; const label = agent.active_sessions > 0 ? 'connected' : age < 3600000 ? 'recently seen' : 'offline'; return `<div class="agent" title="${label}"><span class="dot ${kind}"></span><span class="agent-name">${escapeHtml(agent.name)}</span></div>`; }).join('') || '<div class="pilot">No agents connected yet.</div>'; }
    function renderMessages() { $('message-count').textContent = state.messages.length; if (!state.messages.length) { $('messages').innerHTML = '<div class="empty"><strong>Start the room</strong>Mention an agent or send the first message.</div>'; return; } $('messages').innerHTML = state.messages.map((message) => { const intent = message.intent ? `<span class="tag">${escapeHtml(message.intent)}</span>` : ''; const status = state.delivery[message.id] || (message.from === 'master' ? 'accepted' : 'response posted'); return `<article class="message"><div class="avatar">${escapeHtml(initials(message.from))}</div><div><div class="message-head"><span class="sender">${escapeHtml(message.from)}</span><span class="time">${relative(message.created_at)}</span></div><div class="message-body">${escapeHtml(message.body)}</div><div class="message-meta">${intent}<span>${escapeHtml(status)} · #${escapeHtml(state.channel)}</span></div></div></article>`; }).join(''); const node = $('messages'); node.scrollTop = node.scrollHeight; }
    async function loadMessages() { try { const result = await api('/api/messages?channel=' + encodeURIComponent(state.channel) + '&limit=100&include_delivery=1'); const detailed = result.data.messages_detailed || []; state.messages = detailed.map((row) => row.message); state.delivery = {}; detailed.forEach((row) => { const deliveries = row.deliveries || []; const delivered = deliveries.filter((delivery) => delivery.delivered_at).length; const read = deliveries.filter((delivery) => delivery.read_at).length; state.delivery[row.message.id] = delivered ? `delivered to ${delivered}/${deliveries.length}` + (read ? ` · read by ${read}` : '') : 'accepted'; }); renderMessages(); setConnection(true, 'Connected'); } catch (error) { setConnection(false, 'Daemon offline'); } }
    async function loadSidebar() { try { const [status, channels, agents] = await Promise.all([api('/api/status'), api('/api/channels'), api('/api/agents')]); state.channels = channels.data.channels_detailed || []; state.agents = agents.data.agents_detailed || []; if (!state.channels.some((channel) => channel.name === state.channel) && state.channels.length) state.channel = state.channels[0].name; $('room-name').textContent = state.channel; $('daemon-status').textContent = status.data.paused ? 'Paused' : 'Running'; renderChannels(); renderAgents(); setConnection(true, 'Connected'); } catch (error) { setConnection(false, 'Daemon offline'); } }
    $('composer').addEventListener('submit', async (event) => { event.preventDefault(); if (state.loading) return; const body = $('body').value.trim(); if (!body) return; state.loading = true; document.querySelector('.send').disabled = true; try { const intent = $('intent').value || null; await api('/api/messages', { method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({to:'#' + state.channel, body, intent}) }); $('body').value = ''; $('intent').value = ''; await loadMessages(); } catch (error) { setConnection(false, error.message); } finally { state.loading = false; document.querySelector('.send').disabled = false; } });
    $('refresh').addEventListener('click', () => { loadSidebar(); loadMessages(); });
    loadSidebar(); loadMessages(); setInterval(() => { loadSidebar(); loadMessages(); }, 2000);
  </script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_values() {
        assert_eq!(
            query_param("channel=agent%20room&limit=10", "channel"),
            Some("agent room".into())
        );
        assert_eq!(query_param("channel=general", "missing"), None);
        assert_eq!(percent_decode("hello+world%21"), "hello world!");
    }
}
