//! End-to-end integration tests against the `sidebar` binary.
//!
//! Each test gets its own SIDEBAR_HOME tmpdir; the daemon is spawned as a
//! subprocess and torn down at the end of the test.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static TEST_ID: AtomicU32 = AtomicU32::new(0);

fn sidebar_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sidebar"))
}

struct Sandbox {
    home: PathBuf,
    daemon: Child,
}

impl Sandbox {
    fn new() -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let home = std::env::temp_dir().join(format!("sidebar-test-{pid}-{id}"));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        // clippy::zombie_processes: the Drop impl kills+waits.
        #[allow(clippy::zombie_processes)]
        let daemon = Command::new(sidebar_bin())
            .arg("serve")
            .env("SIDEBAR_HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");

        let socket = home.join("sidebar.sock");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket.exists() {
                // Avoid the stale-socket race: also wait for the daemon's
                // first write to the db.
                std::thread::sleep(Duration::from_millis(50));
                return Self { home, daemon };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon did not bind socket within timeout");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(sidebar_bin())
            .args(args)
            .env("SIDEBAR_HOME", &self.home)
            .output()
            .expect("run sidebar cli")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command failed: {args:?}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // SIGKILL is fine for tests — each sandbox uses a fresh tmpdir.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn participants_returns_master_on_fresh_daemon() {
    let sb = Sandbox::new();
    let out = sb.stdout(&["participants"]);
    assert_eq!(out.trim(), "master");
}

#[test]
fn send_then_history_round_trips() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "#general", "hello world"]);
    sb.stdout(&["send", "#general", "second line"]);

    let history = sb.stdout(&["history", "--channel", "general", "--limit", "10"]);
    assert!(
        history.contains("hello world"),
        "history missing first: {history}"
    );
    assert!(
        history.contains("second line"),
        "history missing second: {history}"
    );
    assert!(
        history.contains("master"),
        "history missing sender: {history}"
    );
}

#[test]
fn sending_to_unknown_agent_auto_creates_them() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@bob", "hi bob"]);

    let participants = sb.stdout(&["participants"]);
    assert!(participants.contains("master"));
    assert!(participants.contains("bob"));
}

#[test]
fn broadcast_creates_no_extra_agents() {
    let sb = Sandbox::new();
    sb.stdout(&["say", "anyone home"]);

    let participants = sb.stdout(&["participants"]);
    let count = participants.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(
        count, 1,
        "broadcast shouldn't create agents; got:\n{participants}"
    );
}

#[test]
fn unknown_subcommand_fails() {
    let sb = Sandbox::new();
    let out = sb.run(&["nope-not-a-command"]);
    assert!(!out.status.success());
}

#[test]
fn pause_is_still_stubbed_and_errors_explicitly() {
    let sb = Sandbox::new();
    let out = sb.run(&["pause"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not yet implemented"), "stderr was: {err}");
}

/// Regression: the MCP stub must start cleanly even when the daemon is down.
/// Otherwise Claude Code reports "-32000" the moment sidebar is configured
/// before `sidebar serve` is running. Tool calls should return a friendly
/// error rather than the stub dying.
#[test]
fn mcp_stub_survives_missing_daemon() {
    let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let home = std::env::temp_dir().join(format!("sidebar-test-{pid}-{id}-nomac"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    // No daemon. Pipe an MCP handshake into `sidebar mcp` and expect it to
    // respond to initialize + tools/list and a tools/call that errors cleanly.
    let mut child = Command::new(sidebar_bin())
        .args(["mcp", "--as", "stranded"])
        .env("SIDEBAR_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    let stdin = child.stdin.as_mut().unwrap();
    let handshake = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"participants","arguments":{}}}"#,
        "\n",
    );
    Write::write_all(stdin, handshake.as_bytes()).unwrap();
    drop(child.stdin.take());

    let out = child.wait_with_output().expect("wait mcp");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Must respond to initialize without crashing.
    assert!(
        stdout.contains("\"id\":1") && stdout.contains("serverInfo"),
        "no initialize response: stdout=\n{stdout}"
    );
    // Must respond to the tool call (with an error payload), not die mid-handshake.
    assert!(
        stdout.contains("\"id\":2"),
        "no tools/call response (stub died?): stdout=\n{stdout}"
    );
    assert!(
        stdout.contains("daemon not reachable"),
        "expected friendly daemon-down error, got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
}
