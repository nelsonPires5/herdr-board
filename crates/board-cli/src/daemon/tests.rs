#[cfg(unix)]
use super::daemon_command;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
#[test]
fn detached_bootstrap_log_is_private_and_truncated_per_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("bootstrap-probe.sh");
    fs::write(
        &script,
        "#!/bin/sh\nprintf 'stdout-secret\\n'\nprintf '%s\\n' \"$BOOTSTRAP_MESSAGE\" >&2\n",
    )
    .expect("write probe");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("chmod probe");
    let log = dir.path().join("bootstrap.log");

    for message in ["first", "second"] {
        let status = daemon_command(&script, &log)
            .expect("build command")
            .env("BOOTSTRAP_MESSAGE", message)
            .status()
            .expect("run probe");
        assert!(status.success());
    }

    assert_eq!(fs::read_to_string(&log).expect("read log"), "second\n");
    assert_eq!(
        fs::metadata(&log)
            .expect("log metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let target = dir.path().join("target");
    fs::write(&target, "do-not-truncate").expect("write target");
    fs::remove_file(&log).expect("remove bootstrap log");
    symlink(&target, &log).expect("symlink bootstrap log");
    assert!(daemon_command(&script, &log).is_err());
    assert_eq!(
        fs::read_to_string(target).expect("read target"),
        "do-not-truncate"
    );
}

#[cfg(unix)]
#[test]
fn daemon_child_owns_a_distinct_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("probe-daemon.sh");
    let evidence = dir.path().join("process-group");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s %s %s\\n' \"$$\" \"$(ps -o pgid= -p $$)\" \"$(ps -o pgid= -p $PPID)\" > \"$BOARD_TEST_PROCESS_GROUP\"\nsleep 30\n",
    )
    .expect("write probe");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("chmod probe");

    let log = dir.path().join("daemon.log");
    let mut command = daemon_command(Path::new(&script), &log).expect("build command");
    command.env("BOARD_TEST_PROCESS_GROUP", &evidence);
    let mut child = command.spawn().expect("spawn probe");

    let values = {
        let mut complete = None;
        for _ in 0..100 {
            if let Ok(values) = fs::read_to_string(&evidence) {
                if values.split_whitespace().count() == 3 {
                    complete = Some(values);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        complete.expect("process-group evidence did not contain exactly three fields within 1s")
    };
    let mut fields = values.split_whitespace();
    let pid: i32 = fields.next().expect("pid").parse().expect("numeric pid");
    let pgid: i32 = fields.next().expect("pgid").parse().expect("numeric pgid");
    let parent_pgid: i32 = fields
        .next()
        .expect("parent pgid")
        .parse()
        .expect("numeric parent pgid");
    assert_eq!(pid, pgid, "daemon child must lead its owned process group");
    assert_ne!(
        pgid, parent_pgid,
        "daemon group must be distinct from its parent"
    );

    child.kill().expect("kill probe");
    child.wait().expect("reap probe");
}

#[cfg(windows)]
#[test]
fn stop_reports_not_running_when_no_pipe_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boardd.sock");
    assert!(matches!(
        super::check_listener_after_connect_failure(&path, super::file_identity(&path)),
        super::ListenerCheck::Gone
    ));
}

/// A pipe that still exists but refuses this connect (every instance busy,
/// or another user's server) must not be reported as stopped.
#[cfg(windows)]
#[test]
fn stop_fails_closed_when_the_pipe_exists_but_connect_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boardd.sock");
    // Never accepts, so the one held client leaves every instance busy.
    let _listener = board_ipc::Listener::bind(&path).unwrap();
    let _held = board_ipc::Stream::connect(&path).unwrap();
    assert!(matches!(
        super::check_listener_after_connect_failure(&path, super::file_identity(&path)),
        super::ListenerCheck::Live
    ));
}

#[cfg(windows)]
#[test]
fn bootstrap_log_is_truncated_per_start() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("bootstrap.log");
    std::fs::write(&log, "stale").unwrap();
    drop(super::open_bootstrap_log(&log).unwrap());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
}
