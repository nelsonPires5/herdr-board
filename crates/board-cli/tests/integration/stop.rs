use std::process::Command;
use std::time::{Duration, Instant};

use board_core::client::{BoardClient, UnixClient};

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

    let mut client = UnixClient::connect(fake.path()).expect("live socket accepts a new RPC");
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
