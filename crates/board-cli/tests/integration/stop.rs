use std::process::Command;
use std::time::{Duration, Instant};

use board_core::client::{BoardClient, LocalClient};

use super::{run_board_stop, FakeListener, FakeStop, TestDaemon};

#[test]
fn daemon_stop_rpc_error_preserves_live_socket_for_new_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::Error);

    let out = run_board_stop(fake.path());
    assert!(!out.status.success(), "RPC stop error must be non-zero");
    assert!(
        fake.path().exists(),
        "RPC failure must preserve live socket"
    );

    let mut client = LocalClient::connect(fake.path()).expect("live socket accepts a new RPC");
    let status = client.daemon_status().expect("new RPC after stop error");
    assert_eq!(status.version, "fake");
}

#[test]
fn daemon_stop_ack_with_live_listener_times_out_without_unlinking() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::StayLive);

    let out = run_board_stop(fake.path());
    assert!(
        !out.status.success(),
        "live listener timeout must be non-zero"
    );
    assert!(fake.path().exists(), "timeout must preserve live socket");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("still listening") || stderr.contains("timed out"));
}

#[test]
fn daemon_stop_real_disappearance_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::Disappear);

    let out = run_board_stop(fake.path());
    assert!(
        out.status.success(),
        "disappeared listener should stop cleanly"
    );
    assert!(
        !fake.path().exists(),
        "disappeared listener leaves no socket"
    );
}

#[test]
fn daemon_stop_removes_stale_socket_only_after_failed_connect() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::StayLive);
    let socket = fake.path().to_path_buf();
    drop(fake);
    assert!(
        socket.exists(),
        "dropped listener leaves stale socket inode"
    );

    let out = run_board_stop(&socket);
    assert!(out.status.success(), "stale socket should be cleaned up");
    assert!(!socket.exists(), "stale socket should be removed");
}

#[test]
fn daemon_stop_preserves_inode_replacement_after_ack() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::Replace);
    let socket = fake.path().to_path_buf();

    let out = run_board_stop(&socket);
    assert!(!out.status.success(), "replacement must fail closed");
    assert!(socket.exists(), "replacement path must be preserved");
    assert_eq!(
        std::fs::read(&socket).unwrap(),
        b"replacement daemon socket"
    );
}

/// D1: `board daemon stop` is the spelled-out form of the retained
/// `board daemon --stop` flag and behaves identically.
#[test]
fn daemon_stop_subcommand_matches_the_retained_flag() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::Disappear);

    let out = Command::new(super::BOARD_BIN)
        .args(["daemon", "stop"])
        .env("BOARD_SOCKET", fake.path())
        .output()
        .expect("run board daemon stop");
    assert!(out.status.success(), "{:?}", out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "boardd stopped"
    );
    assert!(!fake.path().exists());
}

/// C3: the daemon commands use the same output path as everything else, so
/// `--json` is honored instead of printing prose to stdout.
#[test]
fn daemon_stop_honors_json() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeListener::bind(dir.path(), FakeStop::Disappear);

    let out = Command::new(super::BOARD_BIN)
        .args(["daemon", "stop", "--json"])
        .env("BOARD_SOCKET", fake.path())
        .output()
        .expect("run board daemon stop --json");
    assert!(out.status.success(), "{:?}", out.stderr);
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("daemon stop emits JSON");
    assert_eq!(report["stopped"], true);
    assert_eq!(report["was_running"], true);

    // Stopping an absent daemon is still a success, reported the same way.
    let absent = Command::new(super::BOARD_BIN)
        .args(["daemon", "--stop", "--json"])
        .env("BOARD_SOCKET", dir.path().join("nothing-here.sock"))
        .output()
        .expect("run board daemon --stop --json");
    assert!(absent.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&absent.stdout).expect("daemon stop emits JSON");
    assert_eq!(report["stopped"], true);
    assert_eq!(report["was_running"], false);
}

/// An auto-started daemon in a private temp environment; stopped on drop so a
/// failing assertion never leaves it running.
struct AutoStarted {
    dir: tempfile::TempDir,
}

impl AutoStarted {
    /// Run `board` and capture its output, bounded: a detached daemon that
    /// kept the caller's stdout/stderr pipes open would otherwise hang the
    /// capture forever instead of failing.
    fn board(&self, args: &[&str]) -> std::process::Output {
        let mut command = self.command(args);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(command.output());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("board output never reached EOF: the daemon holds the caller's pipes")
            .expect("run board")
    }

    fn command(&self, args: &[&str]) -> Command {
        let dir = self.dir.path();
        let mut command = Command::new(super::BOARD_BIN);
        command
            .args(args)
            .current_dir(dir)
            .env("BOARD_SOCKET", self.socket())
            .env("BOARD_DB", dir.join("board.db"))
            .env("BOARD_LOG_DIR", dir.join("logs"))
            .env("HERDR_BOARD_CONFIG", dir.join("missing-config.toml"))
            .env("HOME", dir)
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .env_remove("BOARD_RUN_ID")
            .stdin(std::process::Stdio::null());
        command
    }

    fn socket(&self) -> std::path::PathBuf {
        self.dir.path().join("boardd.sock")
    }

    /// Whether a daemon still holds this directory's single-instance lock
    /// (taken first thing at startup, released at exit).
    fn daemon_holds_lock(&self) -> bool {
        let Ok(file) = std::fs::File::open(self.dir.path().join("board.db.lock")) else {
            return false;
        };
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
    }
}

impl Drop for AutoStarted {
    fn drop(&mut self) {
        // A daemon still starting up (slow under a loaded parallel test run)
        // answers `stop` with "not running" yet listens moments later; keep
        // stopping until nothing holds the instance lock, so no daemon
        // outlives the test holding its directory or the runner's pipes.
        // `stop` never auto-starts, so its own capture cannot be held open.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let _ = self.command(&["daemon", "stop"]).output();
            if !self.daemon_holds_lock() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// The detached auto-start and the identity-checked stop, end to end: the
/// first command starts boardd, `daemon stop` sees it go, and the endpoint's
/// on-disk trace (socket file or Windows marker) leaves with it.
#[test]
fn auto_started_daemon_serves_then_stops_cleanly() {
    let env = AutoStarted {
        dir: tempfile::tempdir().unwrap(),
    };

    // The CLI waits a fixed 3s for an auto-started daemon; a debug build
    // under a fully parallel test run can take longer to come up, so a
    // "could not connect" first attempt is retried (finding it live, or
    // starting a second one that exits on the instance lock).
    let mut listed = env.board(&["card", "list", "--json"]);
    for _ in 0..3 {
        if listed.status.success()
            || !String::from_utf8_lossy(&listed.stderr).contains("could not connect to boardd")
        {
            break;
        }
        listed = env.board(&["card", "list", "--json"]);
    }
    assert!(
        listed.status.success(),
        "auto-start: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(
        env.socket().exists(),
        "a live boardd leaves its endpoint trace"
    );
    let mut client = LocalClient::connect(&env.socket()).expect("auto-started boardd is live");
    assert!(client.daemon_status().is_ok());
    drop(client);

    let stopped = env.board(&["daemon", "stop", "--json"]);
    assert!(
        stopped.status.success(),
        "stop: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&stopped.stdout).expect("JSON");
    assert_eq!(report["was_running"], true);
    assert!(
        !env.socket().exists(),
        "a clean stop leaves no endpoint trace"
    );
    assert!(LocalClient::connect(&env.socket()).is_err());
}

#[test]
fn single_instance_second_exits_zero() {
    let td = TestDaemon::start(&[]);
    // A second daemon on the same DB must lose the flock race and exit 0.
    let dir = td._dir.path();
    let mut cmd = Command::new(super::BOARD_BIN);
    cmd.arg("daemon")
        .env("BOARD_DB", dir.join("board.db"))
        .env("BOARD_SOCKET", dir.join("boardd.sock"))
        .env("HOME", dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut second = cmd.spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = second.try_wait().unwrap() {
            assert!(status.success(), "second daemon should exit 0");
            break;
        }
        if Instant::now() >= deadline {
            let _ = second.kill();
            panic!("second daemon did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Original daemon still serving.
    assert!(td.client().daemon_status().is_ok());
}
