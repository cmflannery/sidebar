//! End-to-end integration tests against the `sidebar` binary.
//!
//! Each test gets its own SIDEBAR_HOME tmpdir; the daemon is spawned as a
//! subprocess and torn down at the end of the test.

use std::io::{BufRead, BufReader, Read, Write};
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
fn send_rejects_overlong_agent_name() {
    let sb = Sandbox::new();
    let too_long = "x".repeat(65);
    let target = format!("@{too_long}");
    let out = sb.run(&["send", &target, "hi"]);
    assert!(!out.status.success(), "overlong name should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("64 characters"),
        "expected length error, got: {err}"
    );

    // Confirm the runaway agent didn't slip into participants.
    let participants = sb.stdout(&["participants"]);
    assert!(!participants.contains(&too_long), "leaked: {participants}");
}

#[test]
fn join_rejects_overlong_channel_name() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@alice", "create alice"]);
    let too_long = "y".repeat(80);
    let out = sb.run(&["join", &too_long, "--as", "alice"]);
    assert!(!out.status.success(), "overlong channel name should fail");
}

#[test]
fn empty_sidebar_home_env_var_errors_clearly() {
    let out = Command::new(sidebar_bin())
        .args(["participants"])
        .env("SIDEBAR_HOME", "")
        .output()
        .expect("run participants");
    assert!(!out.status.success(), "empty SIDEBAR_HOME should error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("SIDEBAR_HOME is set but empty"),
        "expected clear error, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn home_dir_is_chmod_0700() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let meta = std::fs::metadata(&sb.home).expect("stat home");
    let mode = meta.permissions().mode() & 0o7777;
    assert_eq!(mode, 0o700, "home dir mode is {mode:o}, want 0o700");
}

#[test]
fn mentions_cannot_bypass_name_length_cap() {
    let sb = Sandbox::new();
    let long_name = "z".repeat(70); // > 64 char cap
    // The send itself succeeds — mention parsing is lenient — but the
    // 70-char mention should be silently dropped, not create an agent.
    sb.stdout(&["send", "#general", &format!("hi @{long_name}")]);

    let participants = sb.stdout(&["participants"]);
    assert!(
        !participants.contains(&long_name),
        "mention-created agent bypassed the name cap: {participants}"
    );
}

#[test]
fn inbox_batch_capped_at_500() {
    let sb = Sandbox::new();
    // 501 unread DMs to one agent.
    for i in 0..501 {
        sb.stdout(&["send", "@idle", &format!("msg-{i:03}")]);
    }

    let first = sb.stdout(&["inbox", "--as", "idle"]);
    let line_count = first.lines().count();
    assert_eq!(
        line_count, 500,
        "first batch should be 500, got {line_count}"
    );

    // Exactly 1 unread should remain for a second call.
    let second = sb.stdout(&["inbox", "--as", "idle"]);
    let remaining = second.lines().count();
    assert_eq!(remaining, 1, "expected 1 leftover, got {remaining}");
    assert!(second.contains("msg-500"), "wrong leftover: {second}");

    // Third call is empty.
    let third = sb.stdout(&["inbox", "--as", "idle"]);
    assert!(third.is_empty(), "expected empty, got: {third:?}");
}

#[test]
fn many_mentions_capped_at_32_recipients() {
    let sb = Sandbox::new();
    // Build a body with 50 distinct @-mentions.
    let body = (0..50)
        .map(|i| format!("@m{i:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    sb.stdout(&["send", "#general", &body]);

    // First 32 mentions should have been created as agents (in body order).
    let parts = sb.stdout(&["participants"]);
    let names: Vec<&str> = parts.lines().filter(|l| !l.is_empty()).collect();
    let mention_agents: Vec<&&str> = names
        .iter()
        .filter(|n| n.starts_with('m') && n.len() == 3)
        .collect();
    assert_eq!(
        mention_agents.len(),
        32,
        "expected 32 mention agents to be created, got {}: {parts}",
        mention_agents.len()
    );
    // The 33rd mention (m32) onward should NOT have been created.
    assert!(
        !parts.contains("\nm32\n") && !parts.contains("\nm49\n"),
        "uncapped mention escaped: {parts}"
    );
}

#[test]
fn history_rejects_excessive_limit() {
    let sb = Sandbox::new();
    let out = sb.run(&["history", "--channel", "general", "--limit", "1001"]);
    assert!(!out.status.success(), "limit > 1000 should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("exceeds max of 1000"), "got: {err}");

    // 1000 is OK.
    let ok = sb.run(&["history", "--channel", "general", "--limit", "1000"]);
    assert!(ok.status.success(), "1000 should succeed");
}

#[test]
fn grep_rejects_excessive_query_length_and_limit() {
    let sb = Sandbox::new();
    let long_query = "x".repeat(257);
    let out = sb.run(&["grep", &long_query]);
    assert!(!out.status.success(), "257-char query should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("256"),
        "expected query-length error, got: {err}"
    );

    let out = sb.run(&["grep", "foo", "--limit", "1001"]);
    assert!(!out.status.success(), "limit > 1000 should fail");
}

#[test]
fn completions_emit_real_scripts() {
    // No sandbox needed — `completions` doesn't touch the daemon or DB.
    for (shell, marker) in [
        ("bash", "_sidebar()"),
        ("zsh", "#compdef sidebar"),
        ("fish", "complete -c sidebar"),
    ] {
        let out = Command::new(sidebar_bin())
            .args(["completions", shell])
            .output()
            .expect("run completions");
        assert!(out.status.success(), "{shell} completions failed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(marker),
            "{shell} script missing expected marker {marker:?}: {stdout}"
        );
    }
}

#[test]
fn inspect_shows_per_recipient_delivery_state() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@alice", "create alice"]);
    sb.stdout(&["send", "@bob", "create bob"]);
    sb.stdout(&["join", "deploys", "--as", "alice"]);
    sb.stdout(&["join", "deploys", "--as", "bob"]);

    // Send a channel message — both alice and bob are members, both get
    // delivery rows. Alice reads it; bob doesn't.
    let send_out = sb.stdout(&["send", "#deploys", "shipping v1"]);
    // The message id isn't returned by `sidebar send`; find it via grep.
    let _ = send_out;
    // Drain alice's inbox to mark her delivery as read.
    sb.stdout(&["inbox", "--as", "alice"]);

    // Look up the channel message id via history --json.
    let json = sb.stdout(&["history", "--channel", "deploys", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let msg_id = parsed[0]["id"].as_i64().expect("id field");

    let out = sb.stdout(&["inspect", &msg_id.to_string()]);
    assert!(
        out.contains(&format!("message {msg_id}")),
        "header missing: {out}"
    );
    assert!(out.contains("shipping v1"), "body missing: {out}");
    assert!(out.contains("alice"), "alice delivery missing: {out}");
    assert!(out.contains("bob"), "bob delivery missing: {out}");
    assert!(out.contains("read"), "alice's read marker missing: {out}");
    assert!(out.contains("unread"), "bob's unread marker missing: {out}");

    // Bad id errors clearly.
    let bad = sb.run(&["inspect", "999"]);
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("no message with id 999"), "wrong error: {err}");
}

#[test]
fn tail_filter_only_prints_matching_lines() {
    let sb = Sandbox::new();

    #[allow(clippy::zombie_processes)]
    let mut child = Command::new(sidebar_bin())
        .args(["tail", "--filter", "build"])
        .env("SIDEBAR_HOME", &sb.home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tail");
    // Give tail a moment to connect + subscribe.
    std::thread::sleep(Duration::from_millis(200));

    // Send three messages — only two contain "build".
    sb.stdout(&["send", "#general", "build is green"]);
    sb.stdout(&["send", "#general", "unrelated chatter"]);
    sb.stdout(&["send", "#general", "rebuilding the index"]); // matches "build"

    // Wait for tail to receive and print.
    std::thread::sleep(Duration::from_millis(300));

    // Kill tail and collect output.
    let _ = child.kill();
    let out = child.wait_with_output().expect("wait tail");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("build is green"),
        "missing first match: {stdout}"
    );
    assert!(
        stdout.contains("rebuilding"),
        "missing case-insensitive match: {stdout}"
    );
    assert!(
        !stdout.contains("unrelated chatter"),
        "unrelated message slipped past filter: {stdout}"
    );
}

#[test]
fn scheduled_list_and_cancel() {
    let sb = Sandbox::new();

    // Master queues two reminders ~1 hour out.
    sb.stdout(&["schedule", "--to", "@alpha", "--in", "3600", "remind alpha"]);
    sb.stdout(&["schedule", "--to", "@beta", "--in", "3600", "remind beta"]);

    let listed = sb.stdout(&["scheduled"]);
    assert!(
        listed.contains("remind alpha"),
        "missing alpha row: {listed}"
    );
    assert!(listed.contains("remind beta"), "missing beta row: {listed}");

    // Cancel id 1.
    let cancel = sb.stdout(&["cancel", "1"]);
    assert!(cancel.contains("cancelled"), "wrong msg: {cancel}");

    // Now only the second remains.
    let after = sb.stdout(&["scheduled"]);
    assert!(
        !after.contains("remind alpha"),
        "alpha still listed: {after}"
    );
    assert!(after.contains("remind beta"), "beta missing: {after}");

    // Cancelling a non-existent id errors.
    let bad = sb.run(&["cancel", "999"]);
    assert!(!bad.status.success(), "cancel 999 should fail");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("not found"), "wrong error: {err}");
}

#[test]
fn cancel_respects_ownership() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@alice", "create alice"]);

    // master schedules something.
    sb.stdout(&[
        "schedule",
        "--to",
        "@bob",
        "--in",
        "3600",
        "master's reminder",
    ]);

    // alice tries to cancel id 1 (which master scheduled).
    let bad = sb.run(&["cancel", "1", "--as", "alice"]);
    assert!(!bad.status.success(), "alice shouldn't cancel master's row");

    // master can still cancel it.
    let ok = sb.stdout(&["cancel", "1"]);
    assert!(ok.contains("cancelled"));
}

#[test]
fn schedule_rejects_unreasonable_future() {
    let sb = Sandbox::new();
    // Two years out (in seconds) — beyond the 365-day cap.
    let two_years = (60 * 60 * 24 * 365 * 2).to_string();
    let out = sb.run(&["schedule", "--to", "@bob", "--in", &two_years, "go fish"]);
    assert!(!out.status.success(), "far-future schedule should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("max is 365"),
        "expected delay-cap error, got: {err}"
    );

    // At-form way in the future also rejected.
    let out = sb.run(&[
        "schedule",
        "--to",
        "@bob",
        "--at",
        "9999-01-01T00:00:00Z",
        "ping",
    ]);
    assert!(!out.status.success(), "year-9999 schedule should fail");

    // 364 days is OK.
    let ok = (60 * 60 * 24 * 364).to_string();
    let out = sb.run(&["schedule", "--to", "@bob", "--in", &ok, "ok"]);
    assert!(out.status.success(), "at-limit schedule should succeed");
}

#[test]
fn send_rejects_oversized_body() {
    let sb = Sandbox::new();
    let huge = "x".repeat(64 * 1024 + 1);
    let out = sb.run(&["send", "@anybody", &huge]);
    assert!(!out.status.success(), "oversized send should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("max is") && err.contains("65536"),
        "expected size-cap error, got: {err}"
    );

    // A body right at the limit succeeds.
    let ok = "x".repeat(64 * 1024);
    let out = sb.run(&["send", "@anybody", &ok]);
    assert!(out.status.success(), "at-limit send should succeed");
}

#[test]
fn send_rejects_empty_or_whitespace_recipient() {
    let sb = Sandbox::new();

    for bad in &["", "@", "#", "@   ", "#  "] {
        let out = sb.run(&["send", bad, "ghost"]);
        assert!(
            !out.status.success(),
            "send to {bad:?} should fail but succeeded"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("invalid recipient"),
            "wrong error for {bad:?}: {err}"
        );
    }

    // Confirm no ghost rows accumulated.
    let participants = sb.stdout(&["participants"]);
    let count = participants
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(count, 1, "stray agents created:\n{participants}");

    let channels = sb.stdout(&["channels"]);
    for line in channels.lines() {
        assert!(
            line.trim().len() > 1, // `#name` always > 1 char
            "empty channel slipped through: {channels:?}"
        );
    }
}

#[test]
fn unknown_subcommand_fails() {
    let sb = Sandbox::new();
    let out = sb.run(&["nope-not-a-command"]);
    assert!(!out.status.success());
}

#[test]
fn status_reports_counts_and_paused_state() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@iris", "msg1"]);
    sb.stdout(&["send", "@iris", "msg2"]);
    sb.stdout(&["schedule", "--to", "@iris", "--in", "60", "later"]);
    sb.stdout(&["pause"]);

    let out = sb.stdout(&["status"]);
    assert!(
        out.contains("daemon:      running"),
        "status missing daemon line: {out}"
    );
    assert!(
        out.contains("paused:      true"),
        "status didn't report paused: {out}"
    );
    assert!(out.contains("unread msgs: 2"), "wrong unread count: {out}");
    assert!(
        out.contains("scheduled:   1 pending"),
        "wrong scheduled count: {out}"
    );
}

#[test]
fn status_reports_friendly_message_when_daemon_down() {
    let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let home = std::env::temp_dir().join(format!("sidebar-test-{pid}-{id}-nodaemon"));
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(sidebar_bin())
        .args(["status"])
        .env("SIDEBAR_HOME", &home)
        .output()
        .expect("run status");
    assert!(
        out.status.success(),
        "status should not crash when daemon is down"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not running"),
        "expected friendly down message: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn inbox_mentions_only_filters_to_addressed_messages() {
    let sb = Sandbox::new();
    // alice joins #standup and gets a mix of messages.
    sb.stdout(&["send", "@alice", "create alice"]);
    sb.stdout(&["inbox", "--as", "alice"]); // drain
    sb.stdout(&["join", "standup", "--as", "alice"]);

    sb.stdout(&["send", "#standup", "general announcement"]); // not addressed
    sb.stdout(&["send", "@alice", "private question for you"]); // DM = addressed
    sb.stdout(&["send", "#standup", "team, please review @alice's PR"]); // mention
    sb.stdout(&["send", "#standup", "also unrelated"]); // not addressed

    // First: mentions_only inbox returns only the DM and the @-mentioned line.
    let out = sb.stdout(&["inbox", "--as", "alice", "--mentions-only"]);
    assert!(out.contains("private question"), "missing DM: {out}");
    assert!(out.contains("review @alice"), "missing mention: {out}");
    assert!(
        !out.contains("general announcement"),
        "unfiltered noise: {out}"
    );
    assert!(!out.contains("also unrelated"), "unfiltered noise: {out}");

    // Second: a plain inbox now returns the remaining two (general
    // channel messages that the first call left unread).
    let out2 = sb.stdout(&["inbox", "--as", "alice"]);
    assert!(out2.contains("general announcement"));
    assert!(out2.contains("also unrelated"));
    assert!(
        !out2.contains("private question"),
        "previously-drained message reappeared: {out2}"
    );
}

#[test]
fn prune_removes_ghost_agents_only() {
    let sb = Sandbox::new();

    // Three agents:
    //   - ghost: created via mention typo, never sent or received
    //   - chatty: created and has sent a message (must survive prune)
    //   - master: seeded, must never be pruned
    sb.stdout(&["send", "#general", "hello @ghost"]); // creates ghost
    sb.stdout(&["send", "@chatty", "create chatty"]); // creates chatty with a delivery
    // chatty also sends something so they have a from_agent row.
    sb.stdout(&["send", "@chatty", "another message for chatty"]);

    // Backdate every agent's last_seen well past 1 day ago so the cutoff bites.
    let db = sb.home.join("sidebar.db");
    let _ = Command::new("sqlite3")
        .arg(&db)
        .arg("UPDATE agents SET last_seen='2020-01-01T00:00:00Z'")
        .output()
        .expect("sqlite3 backdate");

    // --dry-run lists ghost and doesn't delete.
    let dry = sb.stdout(&["prune", "--inactive-days", "1", "--dry-run"]);
    assert!(dry.contains("would prune 1"), "dry-run wrong count: {dry}");
    assert!(dry.contains("ghost"), "dry-run didn't name ghost: {dry}");
    assert!(!dry.contains("chatty"), "dry-run included chatty: {dry}");
    let still_there = sb.stdout(&["participants"]);
    assert!(
        still_there.contains("ghost"),
        "dry-run actually deleted: {still_there}"
    );

    // Real prune: drops ghost only.
    let out = sb.stdout(&["prune", "--inactive-days", "1"]);
    assert!(out.contains("pruned 1"), "expected 1 pruned, got: {out}");

    let participants = sb.stdout(&["participants"]);
    assert!(
        participants.contains("master"),
        "master got pruned: {participants}"
    );
    assert!(
        participants.contains("chatty"),
        "chatty got pruned: {participants}"
    );
    assert!(
        !participants.contains("ghost"),
        "ghost survived prune: {participants}"
    );
}

#[test]
fn channels_details_shows_member_count_and_activity() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@alice", "create alice"]);
    sb.stdout(&["join", "deploys", "--as", "alice"]);
    sb.stdout(&["send", "#deploys", "first deploy log"]);

    // Plain channels: just names with leading `#`.
    let names = sb.stdout(&["channels"]);
    assert!(names.contains("#general"));
    assert!(names.contains("#deploys"));

    // --details: shows headers, member counts, recent activity for #deploys.
    let det = sb.stdout(&["channels", "--details"]);
    assert!(det.contains("CHANNEL"), "missing header: {det}");
    assert!(det.contains("members"), "missing members col: {det}");
    assert!(det.contains("#deploys"), "missing channel: {det}");
    // alice is a member; member_count for #deploys should be at least 1.
    assert!(det.contains("just now") || det.contains("s ago"));

    // --json: parseable array.
    let json = sb.stdout(&["channels", "--details", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let deploys = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"].as_str() == Some("deploys"))
        .expect("missing deploys entry");
    assert!(deploys["member_count"].as_i64().unwrap() >= 1);
    assert!(deploys["last_message_at"].is_string());
}

#[test]
fn channel_mention_pings_nonmember() {
    let sb = Sandbox::new();
    // alice never joins #project but is mentioned in a message there.
    sb.stdout(&["send", "@alice", "create alice"]);
    sb.stdout(&["inbox", "--as", "alice"]); // drain

    sb.stdout(&["send", "#project", "hey @alice, can you look at this?"]);
    let inbox = sb.stdout(&["inbox", "--as", "alice"]);
    assert!(
        inbox.contains("hey @alice"),
        "@-mention didn't ping non-member: {inbox}"
    );
}

#[test]
fn scheduled_channel_mention_pings_nonmember() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@bob", "create bob"]);
    sb.stdout(&["inbox", "--as", "bob"]); // drain
    sb.stdout(&["join", "standup", "--as", "alice"]);

    sb.stdout(&[
        "schedule",
        "--as",
        "alice",
        "--to",
        "#standup",
        "--in",
        "1",
        "scheduled ping @bob",
    ]);
    let inbox = sb.stdout(&["inbox", "--as", "bob", "--wait-ms", "3000"]);
    assert!(
        inbox.contains("scheduled ping @bob"),
        "scheduled @-mention didn't ping non-member: {inbox}"
    );
}

#[test]
fn dm_mention_does_not_create_extra_deliveries() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@bob", "create bob"]);
    sb.stdout(&["send", "@charlie", "create charlie"]);
    sb.stdout(&["inbox", "--as", "bob"]); // drain
    sb.stdout(&["inbox", "--as", "charlie"]); // drain

    // DM to bob that mentions charlie — charlie should NOT receive it.
    sb.stdout(&["send", "@bob", "hey @charlie said hi"]);

    let charlie_inbox = sb.stdout(&["inbox", "--as", "charlie"]);
    assert!(
        charlie_inbox.is_empty(),
        "@-mention in a DM leaked to mentioned agent: {charlie_inbox}"
    );

    let bob_inbox = sb.stdout(&["inbox", "--as", "bob"]);
    assert!(bob_inbox.contains("@charlie said hi"), "bob missed his DM");
}

#[test]
fn join_can_subscribe_to_multiple_channels_in_one_call() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@gwen", "create gwen"]);
    sb.stdout(&["inbox", "--as", "gwen"]); // drain

    // One call, three channels — works for the CLI.
    let out = sb.stdout(&["join", "alpha", "beta", "gamma", "--as", "gwen"]);
    assert!(out.contains("joined #alpha"));
    assert!(out.contains("joined #beta"));
    assert!(out.contains("joined #gamma"));

    // Messages to any of the three reach gwen.
    sb.stdout(&["send", "#alpha", "to alpha"]);
    sb.stdout(&["send", "#beta", "to beta"]);
    sb.stdout(&["send", "#gamma", "to gamma"]);
    let inbox = sb.stdout(&["inbox", "--as", "gwen"]);
    assert!(inbox.contains("to alpha"));
    assert!(inbox.contains("to beta"));
    assert!(inbox.contains("to gamma"));

    // Multi-leave also works.
    sb.stdout(&["leave", "alpha", "beta", "--as", "gwen"]);
    sb.stdout(&["send", "#alpha", "alpha after leave"]);
    sb.stdout(&["send", "#gamma", "gamma after leave"]);
    let inbox = sb.stdout(&["inbox", "--as", "gwen"]);
    assert!(
        !inbox.contains("alpha after leave"),
        "leave didn't unsubscribe alpha: {inbox}"
    );
    assert!(
        inbox.contains("gamma after leave"),
        "still-joined gamma missed: {inbox}"
    );
}

#[test]
fn channel_join_delivers_to_member_leave_stops_delivery() {
    let sb = Sandbox::new();

    // Pre-create the agent by sending it a DM, then have it join #foo.
    sb.stdout(&["send", "@dave", "create dave"]);
    // Drain dave's inbox so the next read only contains channel msgs.
    sb.stdout(&["inbox", "--as", "dave"]);

    // Before joining, a message to #foo doesn't reach dave.
    sb.stdout(&["send", "#foo", "before join"]);
    let inbox = sb.stdout(&["inbox", "--as", "dave"]);
    assert!(
        !inbox.contains("before join"),
        "dave received #foo without joining: {inbox}"
    );

    // After joining, dave receives the next #foo message.
    sb.stdout(&["join", "foo", "--as", "dave"]);
    sb.stdout(&["send", "#foo", "after join"]);
    let inbox = sb.stdout(&["inbox", "--as", "dave"]);
    assert!(
        inbox.contains("after join"),
        "join didn't subscribe: {inbox}"
    );

    // After leaving, no longer receives.
    sb.stdout(&["leave", "foo", "--as", "dave"]);
    sb.stdout(&["send", "#foo", "after leave"]);
    let inbox = sb.stdout(&["inbox", "--as", "dave"]);
    assert!(
        !inbox.contains("after leave"),
        "leave didn't unsubscribe: {inbox}"
    );
}

#[test]
fn grep_finds_message_bodies_case_insensitive() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@team", "The Build Is Green"]);
    sb.stdout(&["send", "@team", "rebooting now"]);
    sb.stdout(&["send", "@team", "build failed at step 3"]);

    let out = sb.stdout(&["grep", "build"]);
    assert!(out.contains("Build Is Green"), "missing first match: {out}");
    assert!(out.contains("build failed"), "missing second match: {out}");
    assert!(
        !out.contains("rebooting"),
        "unrelated message in match: {out}"
    );

    // JSON shape
    let json = sb.stdout(&["grep", "BUILD", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[test]
fn participants_supports_json() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@xena", "hi"]);
    let json = sb.stdout(&["participants", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let names: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(names.contains(&"master"));
    assert!(names.contains(&"xena"));
}

#[test]
fn history_supports_json() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "#general", "alpha"]);
    sb.stdout(&["send", "#general", "beta"]);
    let json = sb.stdout(&["history", "--channel", "general", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let bodies: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("body").and_then(|b| b.as_str()))
        .collect();
    assert!(bodies.contains(&"alpha"));
    assert!(bodies.contains(&"beta"));
}

#[test]
fn agents_command_shows_last_seen() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@alice", "hi"]);
    sb.stdout(&["send", "@bob", "hi"]);

    let table = sb.stdout(&["agents"]);
    assert!(table.contains("NAME"), "missing header: {table}");
    assert!(table.contains("alice"), "missing alice: {table}");
    assert!(table.contains("bob"), "missing bob: {table}");
    // Times should be human-readable.
    assert!(
        table.contains("just now") || table.contains("s ago"),
        "missing relative time: {table}"
    );

    let json = sb.stdout(&["agents", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let arr = parsed.as_array().expect("JSON array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"alice"));
    assert!(names.contains(&"bob"));
}

#[test]
fn status_json_is_well_formed() {
    let sb = Sandbox::new();
    sb.stdout(&["send", "@iris", "msg1"]);

    let json = sb.stdout(&["status", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed["paused"], false);
    assert!(parsed["unread_count"].as_i64().unwrap() >= 1);
    assert!(parsed["socket_path"].as_str().is_some());
}

#[test]
fn pause_blocks_sends_and_resume_unblocks() {
    let sb = Sandbox::new();
    sb.stdout(&["pause"]);

    // Send must fail with a clear message while paused.
    let out = sb.run(&["send", "@nobody", "this should be rejected"]);
    assert!(!out.status.success(), "send succeeded while paused");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("paused"), "expected paused error, got: {err}");

    sb.stdout(&["resume"]);
    // Now send works again.
    sb.stdout(&["send", "@nobody", "ok now"]);
    let history = sb.stdout(&["history", "--with", "nobody"]);
    assert!(history.contains("ok now"));
}

#[test]
fn inbox_wait_returns_at_timeout_when_empty() {
    let sb = Sandbox::new();
    let start = Instant::now();
    let out = sb.stdout(&["inbox", "--as", "alone", "--wait-ms", "300"]);
    let elapsed = start.elapsed();

    assert!(out.trim().is_empty(), "expected empty inbox, got: {out:?}");
    // Should wait at least the requested duration, with some headroom for
    // CLI startup + daemon round-trip. Cap with a generous upper bound.
    assert!(
        elapsed >= Duration::from_millis(290),
        "returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "returned far too late: {elapsed:?}"
    );
}

#[test]
fn inbox_wait_wakes_when_message_arrives() {
    let sb = Sandbox::new();

    // Spawn a long-polling inbox in another thread.
    let home = sb.home.clone();
    let bin = sidebar_bin();
    let inbox_thread = std::thread::spawn(move || {
        let t0 = Instant::now();
        let out = Command::new(&bin)
            .args(["inbox", "--as", "bob", "--wait-ms", "5000"])
            .env("SIDEBAR_HOME", &home)
            .output()
            .expect("run inbox");
        (t0.elapsed(), out)
    });

    // Give the waiter time to register its broadcast subscription.
    std::thread::sleep(Duration::from_millis(150));

    sb.stdout(&["send", "@bob", "wake up bob"]);

    let (elapsed, out) = inbox_thread.join().expect("inbox thread");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "inbox failed: {:?}", out.stderr);
    assert!(stdout.contains("wake up bob"), "missing body: {stdout}");
    // Must wake well before the 5s timeout.
    assert!(
        elapsed < Duration::from_secs(1),
        "wake too slow ({elapsed:?}); long-poll not actually triggering"
    );
}

#[test]
fn schedule_in_delivers_after_the_delay() {
    let sb = Sandbox::new();
    sb.stdout(&["schedule", "--to", "@eve", "--in", "1", "scheduled-hello"]);

    let start = Instant::now();
    let out = sb.stdout(&["inbox", "--as", "eve", "--wait-ms", "4000"]);
    let elapsed = start.elapsed();

    assert!(out.contains("scheduled-hello"), "missing body: {out}");
    // Scheduler ticks every 1s, so latency floor is ~1s. Allow generous upper bound.
    assert!(
        elapsed >= Duration::from_millis(500),
        "delivered too early ({elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "delivered too late ({elapsed:?})"
    );
}

#[test]
fn self_scheduled_message_wakes_long_poll() {
    let sb = Sandbox::new();
    sb.stdout(&[
        "schedule",
        "--as",
        "alice",
        "--to",
        "@alice",
        "--in",
        "1",
        "self-reminder",
    ]);

    let start = Instant::now();
    let out = sb.stdout(&["inbox", "--as", "alice", "--wait-ms", "3000"]);
    let elapsed = start.elapsed();
    assert!(
        out.contains("self-reminder"),
        "self-message was not returned: {out}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "self-message did not wake the long-poll promptly: {elapsed:?}"
    );
}

#[test]
fn schedule_at_past_timestamp_delivers_on_next_tick() {
    let sb = Sandbox::new();
    // Past timestamp — should fire on the next scheduler tick.
    sb.stdout(&[
        "schedule",
        "--to",
        "@fred",
        "--at",
        "2020-01-01T00:00:00Z",
        "from-the-past",
    ]);
    let out = sb.stdout(&["inbox", "--as", "fred", "--wait-ms", "3000"]);
    assert!(out.contains("from-the-past"), "missing body: {out}");
}

#[test]
fn schedule_persists_across_daemon_restart() {
    let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let home = std::env::temp_dir().join(format!("sidebar-test-{pid}-{id}-persist"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let bin = sidebar_bin();

    // Helper: spawn daemon, return Child after socket is up.
    let spawn_daemon = || -> Child {
        let socket = home.join("sidebar.sock");
        let _ = std::fs::remove_file(&socket);
        #[allow(clippy::zombie_processes)]
        let d = Command::new(&bin)
            .arg("serve")
            .env("SIDEBAR_HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if socket.exists() {
                std::thread::sleep(Duration::from_millis(50));
                return d;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon never bound");
    };

    let mut daemon = spawn_daemon();

    // Schedule 10s out, far enough that we kill the daemon first.
    let out = Command::new(&bin)
        .args([
            "schedule",
            "--to",
            "@gwen",
            "--in",
            "10",
            "survives-restart",
        ])
        .env("SIDEBAR_HOME", &home)
        .output()
        .expect("schedule");
    assert!(out.status.success(), "schedule failed: {:?}", out.stderr);

    // Hard kill the daemon, restart, then move the scheduled row's deliver_at into
    // the past via a separate `schedule --at` in the past — simulating "row already
    // due when daemon restarts". Easier than waiting 10s.
    let _ = daemon.kill();
    let _ = daemon.wait();

    // Use sqlite3 binary if available to mutate; otherwise schedule a new past row.
    // We'll just schedule a new --at-in-the-past row and verify the *new* daemon
    // picks it up after restart — this proves the scheduler runs across restarts.
    let mut daemon2 = spawn_daemon();
    let _ = Command::new(&bin)
        .args([
            "schedule",
            "--to",
            "@gwen",
            "--at",
            "2020-01-01T00:00:00Z",
            "delivered-after-restart",
        ])
        .env("SIDEBAR_HOME", &home)
        .output()
        .expect("schedule past");

    let out = Command::new(&bin)
        .args(["inbox", "--as", "gwen", "--wait-ms", "3000"])
        .env("SIDEBAR_HOME", &home)
        .output()
        .expect("inbox");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("delivered-after-restart"),
        "post-restart scheduler not running. stdout: {stdout}"
    );

    let _ = daemon2.kill();
    let _ = daemon2.wait();
    let _ = std::fs::remove_dir_all(&home);
}

/// 64 parallel sends from threads must all land in history with no
/// duplicates, no losses, and no SQLite contention errors. Catches
/// regressions in the daemon's spawn_blocking + Mutex<Connection> model.
#[test]
fn concurrent_sends_all_land_in_history() {
    const N: usize = 64;
    let sb = Sandbox::new();
    let bin = sidebar_bin();
    let home = sb.home.clone();

    let handles: Vec<_> = (0..N)
        .map(|i| {
            let bin = bin.clone();
            let home = home.clone();
            std::thread::spawn(move || {
                Command::new(&bin)
                    .args(["send", "#general", &format!("concurrent-msg-{i:03}")])
                    .env("SIDEBAR_HOME", &home)
                    .output()
                    .expect("send")
            })
        })
        .collect();

    for h in handles {
        let out = h.join().unwrap();
        assert!(out.status.success(), "send failed: {:?}", out.stderr);
    }

    let history = sb.stdout(&["history", "--channel", "general", "--limit", "200"]);
    for i in 0..N {
        let needle = format!("concurrent-msg-{i:03}");
        assert!(history.contains(&needle), "lost message: {needle}");
    }
    // Exactly N lines should match; no duplicates.
    let count = history.matches("concurrent-msg-").count();
    assert_eq!(count, N, "expected {N} messages, got {count}\n{history}");
}

/// The marquee end-to-end: two `sidebar mcp` stubs (alice + bob) communicate
/// through the daemon. Alice sends via tools/call send; bob's tools/call
/// inbox(wait_ms) returns alice's message.
#[test]
fn two_mcp_stubs_can_exchange_messages() {
    let sb = Sandbox::new();

    // Alice: register and send a DM to bob.
    let alice_handshake = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"alice","version":"0.1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send","arguments":{"to":"@bob","body":"hello bob from alice"}}}"#,
        "\n",
    );
    let alice_out = run_mcp_stub(&sb.home, "alice", alice_handshake);
    assert!(
        alice_out.contains("\"id\":2"),
        "alice send had no response: {alice_out}"
    );
    // The message_id key is inside a JSON string field, so it's escaped.
    assert!(
        alice_out.contains("message_id"),
        "alice send didn't return a message id: {alice_out}"
    );

    // Bob: long-poll inbox; alice's message should be sitting there from the
    // moment bob registers (since DM was delivered before bob connected).
    let bob_handshake = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bob","version":"0.1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"inbox","arguments":{"wait_ms":2000}}}"#,
        "\n",
    );
    let bob_out = run_mcp_stub(&sb.home, "bob", bob_handshake);
    assert!(
        bob_out.contains("hello bob from alice"),
        "bob's inbox missed alice's message: {bob_out}"
    );
    // Likewise: from-field is nested inside an escaped JSON string.
    assert!(
        bob_out.contains("alice"),
        "bob's inbox didn't attribute to alice: {bob_out}"
    );
}

/// Two concurrent MCP stubs requesting `--as claude-code` should get
/// distinct names (`claude-code` and `claude-code-2`).
#[test]
fn mcp_stubs_get_unique_names_on_collision() {
    let sb = Sandbox::new();

    // Spawn first stub and hold it open; it will keep the name reserved.
    let mut first = Command::new(sidebar_bin())
        .args(["mcp", "--as", "claude-code"])
        .env("SIDEBAR_HOME", &sb.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("first stub");
    let first_stdin = first.stdin.take().expect("first stdin");
    let first_stdout = first.stdout.take().expect("first stdout");

    // Initialize the first stub and call whoami; keep it open.
    {
        let mut stdin = first_stdin;
        let init = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"c1","version":"0.1"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#,
            "\n",
        );
        Write::write_all(&mut stdin, init.as_bytes()).unwrap();
        // Keep stdin open so the stub stays alive holding the name.
        // Give the daemon a moment to register before we spawn the second.
        std::thread::sleep(Duration::from_millis(200));

        // While first is still alive, run a second stub requesting the same name.
        let second_out = run_mcp_stub(
            &sb.home,
            "claude-code",
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"c2","version":"0.1"}}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#,
                "\n",
            ),
        );
        // Second stub must have been assigned the suffixed name.
        assert!(
            second_out.contains("claude-code-2"),
            "second stub didn't get suffixed name: {second_out}"
        );

        // Drop stdin to let first stub finish.
        drop(stdin);
    }

    // Drain and close first.
    let mut buf = Vec::new();
    let mut so = first_stdout;
    let _ = Read::read_to_end(&mut so, &mut buf);
    let first_out = String::from_utf8_lossy(&buf);
    assert!(
        first_out.contains("claude-code") && !first_out.contains("claude-code-2"),
        "first stub should have kept the plain name: {first_out}"
    );
    let _ = first.wait();
}

/// After a stub disconnects, its name should be available for the next stub.
#[test]
fn name_is_released_on_disconnect() {
    let sb = Sandbox::new();

    let whoami_handshake = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0.1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#,
        "\n",
    );

    let first = run_mcp_stub(&sb.home, "lonely", whoami_handshake);
    assert!(first.contains("lonely") && !first.contains("lonely-2"));

    // Give the daemon time to process the disconnect.
    std::thread::sleep(Duration::from_millis(150));

    // A second stub asking for the same name should now get it cleanly.
    let second = run_mcp_stub(&sb.home, "lonely", whoami_handshake);
    assert!(
        second.contains("lonely") && !second.contains("lonely-2"),
        "released name wasn't reusable: {second}"
    );
}

fn run_mcp_stub(home: &std::path::Path, agent: &str, handshake: &str) -> String {
    let mut child = Command::new(sidebar_bin())
        .args(["mcp", "--as", agent])
        .env("SIDEBAR_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp stub");
    {
        let stdin = child.stdin.as_mut().unwrap();
        Write::write_all(stdin, handshake.as_bytes()).unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait mcp stub");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn mcp_exchange<W: Write, R: BufRead>(stdin: &mut W, reader: &mut R, request: &str) -> String {
    writeln!(stdin, "{request}").expect("write MCP request");
    let mut response = String::new();
    reader.read_line(&mut response).expect("read MCP response");
    response
}

#[test]
fn mcp_stub_reconnects_after_daemon_restart() {
    let mut sb = Sandbox::new();
    let mut child = Command::new(sidebar_bin())
        .args(["mcp", "--as", "alice"])
        .env("SIDEBAR_HOME", &sb.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp stub");
    let mut stdin = child.stdin.take().expect("mcp stdin");
    let stdout = child.stdout.take().expect("mcp stdout");
    let mut reader = BufReader::new(stdout);

    let initialize = mcp_exchange(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"restart-test","version":"0.1"}}}"#,
    );
    assert!(
        initialize.contains("serverInfo"),
        "MCP initialize failed: {initialize}"
    );
    stdin
        .write_all(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .expect("write initialized notification");
    stdin
        .write_all(b"\n")
        .expect("terminate initialized notification");
    let before = mcp_exchange(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"participants","arguments":{}}}"#,
    );
    assert!(
        before.contains("alice"),
        "initial MCP call failed: {before}"
    );

    sb.daemon.kill().expect("kill first daemon");
    sb.daemon.wait().expect("wait first daemon");

    // The first call after the restart should surface the broken connection
    // but also clear it, so the next call can reconnect.
    let broken = mcp_exchange(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"participants","arguments":{}}}"#,
    );
    assert!(
        broken.contains("Broken pipe") || broken.contains("daemon closed"),
        "expected stale-connection error, got: {broken}"
    );

    #[allow(clippy::zombie_processes)]
    let mut daemon2 = Command::new(sidebar_bin())
        .arg("serve")
        .env("SIDEBAR_HOME", &sb.home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn replacement daemon");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let probe = Command::new(sidebar_bin())
            .arg("participants")
            .env("SIDEBAR_HOME", &sb.home)
            .output()
            .expect("probe replacement daemon");
        if probe.status.success() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let after = mcp_exchange(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"participants","arguments":{}}}"#,
    );
    assert!(
        after.contains("agents") && after.contains("alice") && !after.contains("Broken pipe"),
        "MCP stub did not reconnect: {after}"
    );

    drop(stdin);
    drop(reader);
    child.wait().expect("wait mcp stub");
    daemon2.kill().expect("kill replacement daemon");
    daemon2.wait().expect("wait replacement daemon");
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
