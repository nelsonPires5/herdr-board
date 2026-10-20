use super::{
    check_listener_after_connect_failure, classify, connect_or_start_with, daemon_command,
    platform, prepare_log_dir, ListenerCheck, Probe,
};
use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use board_ipc::LocalListener;

fn io_error(kind: io::ErrorKind) -> anyhow::Error {
    anyhow::Error::new(io::Error::new(kind, "probe"))
}

// -- endpoint classification ---------------------------------------------------

#[test]
fn only_an_absent_endpoint_classifies_as_absent() {
    assert!(matches!(
        classify::<()>(Err(io_error(io::ErrorKind::NotFound))),
        Probe::Absent
    ));
    // A deadline, a permission problem, or a non-io failure says something
    // is there: never "not running".
    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::PermissionDenied] {
        assert!(
            matches!(classify::<()>(Err(io_error(kind))), Probe::Unreachable(_)),
            "{kind:?} must not read as absent"
        );
    }
    assert!(matches!(
        classify::<()>(Err(anyhow::anyhow!("not an io error"))),
        Probe::Unreachable(_)
    ));
    assert!(matches!(classify(Ok(7)), Probe::Live(7)));
}

#[cfg(unix)]
#[test]
fn unix_refused_socket_classifies_as_absent() {
    assert!(matches!(
        classify::<()>(Err(io_error(io::ErrorKind::ConnectionRefused))),
        Probe::Absent
    ));
}

#[cfg(windows)]
#[test]
fn windows_busy_pipe_classifies_as_live_not_absent() {
    const ERROR_PIPE_BUSY: i32 = 231;
    let busy = anyhow::Error::new(io::Error::from_raw_os_error(ERROR_PIPE_BUSY));
    assert!(matches!(classify::<()>(Err(busy)), Probe::Unreachable(_)));
}

#[test]
fn a_missing_endpoint_is_absent_through_the_real_client() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = board_core::client::LocalClient::connect(&dir.path().join("none.sock"));
    assert!(matches!(classify(result), Probe::Absent));
}

// -- connect_or_start ----------------------------------------------------------

#[test]
fn a_live_daemon_is_used_without_starting_another() {
    let spawned = Cell::new(false);
    let client = connect_or_start_with(
        Path::new("boardd.sock"),
        |_| Ok("client"),
        || {
            spawned.set(true);
            Ok(())
        },
        Duration::from_millis(200),
    )
    .expect("live daemon");
    assert_eq!(client, "client");
    assert!(!spawned.get());
}

#[test]
fn an_absent_daemon_is_started_once_then_connected() {
    let spawns = Cell::new(0);
    let attempts = Cell::new(0);
    let client = connect_or_start_with(
        Path::new("boardd.sock"),
        |_| {
            attempts.set(attempts.get() + 1);
            if spawns.get() == 0 || attempts.get() < 3 {
                Err(io_error(io::ErrorKind::NotFound))
            } else {
                Ok("client")
            }
        },
        || {
            spawns.set(spawns.get() + 1);
            Ok(())
        },
        Duration::from_secs(3),
    )
    .expect("started daemon");
    assert_eq!(client, "client");
    assert_eq!(spawns.get(), 1);
}

#[test]
fn a_busy_daemon_is_waited_on_and_never_given_a_second_start() {
    let attempts = Cell::new(0);
    let client = connect_or_start_with(
        Path::new("boardd.sock"),
        |_| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(io_error(io::ErrorKind::TimedOut))
            } else {
                Ok("client")
            }
        },
        || panic!("an unreachable daemon must not be answered with a second start"),
        Duration::from_secs(3),
    )
    .expect("busy daemon eventually answers");
    assert_eq!(client, "client");
}

#[test]
fn a_daemon_that_stays_busy_fails_without_a_start_and_names_the_cause() {
    let error = connect_or_start_with::<()>(
        Path::new("boardd.sock"),
        |_| Err(io_error(io::ErrorKind::TimedOut)),
        || panic!("an unreachable daemon must not be answered with a second start"),
        Duration::from_millis(100),
    )
    .expect_err("never reachable");
    // The boardd message is the headline; the OS cause follows it.
    let headline = error.to_string();
    assert!(
        headline.starts_with("could not connect to boardd at boardd.sock"),
        "{headline}"
    );
    let message = format!("{error:#}");
    assert!(message.contains("probe"), "{message}");
}

// -- stale-endpoint checks -----------------------------------------------------

#[test]
fn a_live_listener_is_never_cleaned_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("live.sock");
    let _listener = LocalListener::bind(&path).expect("bind");
    #[cfg(windows)]
    fs::write(&path, format!("{}:1", std::process::id())).expect("marker");
    let original = platform::identity(&path);

    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Live
    );
    assert!(path.exists(), "a live endpoint keeps its on-disk trace");
}

#[test]
fn a_missing_endpoint_is_already_gone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("none.sock");
    let original = platform::identity(&path);
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Gone
    );
}

// -- bootstrap log -------------------------------------------------------------

/// A daemon stand-in that writes `stdout-secret` to stdout and
/// `$BOOTSTRAP_MESSAGE` to stderr.
fn bootstrap_probe(dir: &Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("bootstrap-probe.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'stdout-secret\\n'\nprintf '%s\\n' \"$BOOTSTRAP_MESSAGE\" >&2\n",
        )
        .expect("write probe");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("chmod probe");
        script
    }
    #[cfg(windows)]
    {
        let script = dir.join("bootstrap-probe.cmd");
        fs::write(
            &script,
            "@echo off\r\necho stdout-secret\r\n>&2 echo %BOOTSTRAP_MESSAGE%\r\n",
        )
        .expect("write probe");
        script
    }
}

#[test]
fn detached_bootstrap_log_is_truncated_per_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = bootstrap_probe(dir.path());
    let log = dir.path().join("bootstrap.log");
    fs::write(&log, "a much longer leftover from an earlier start\n").expect("seed log");

    for message in ["first", "second"] {
        let status = daemon_command(&script, &log)
            .expect("build command")
            .env("BOOTSTRAP_MESSAGE", message)
            .status()
            .expect("run probe");
        assert!(status.success());
    }

    // Only the latest start's stderr; stdout never reaches the file.
    assert_eq!(
        fs::read_to_string(&log).expect("read log").trim_end(),
        "second"
    );
}

#[cfg(unix)]
#[test]
fn detached_bootstrap_log_is_private_and_refuses_a_symlink() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = tempfile::tempdir().expect("tempdir");
    let script = bootstrap_probe(dir.path());
    let log = dir.path().join("bootstrap.log");
    daemon_command(&script, &log).expect("build command");
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
fn log_directory_is_private_and_a_symlink_is_refused() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = tempfile::tempdir().expect("tempdir");
    let logs = dir.path().join("logs");
    prepare_log_dir(&logs).expect("create log dir");
    assert_eq!(
        fs::metadata(&logs).expect("metadata").permissions().mode() & 0o777,
        0o700
    );

    let real = dir.path().join("real");
    fs::create_dir(&real).expect("real dir");
    let link = dir.path().join("linked-logs");
    symlink(&real, &link).expect("symlink");
    assert!(prepare_log_dir(&link).is_err());
}

#[cfg(unix)]
#[test]
fn daemon_child_owns_a_distinct_process_group() {
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

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

#[cfg(unix)]
#[test]
fn unix_stale_socket_is_unlinked_only_while_it_is_the_original_inode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stale.sock");
    drop(LocalListener::bind(&path).expect("bind"));
    let original = platform::identity(&path);
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Gone
    );
    assert!(!path.exists());

    // A regular file is never a stale socket, even when it is the original.
    fs::write(&path, "not a socket").expect("write file");
    let original = platform::identity(&path);
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Replaced
    );
    assert!(path.exists());
}

// -- Windows -------------------------------------------------------------------

/// A directory junction: unlike a symlink it needs no privilege, so the
/// reparse-point refusals are exercised on every Windows host.
#[cfg(windows)]
fn junction(target: &Path, link: &Path) {
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run mklink");
    assert!(status.success(), "mklink /J failed");
}

#[cfg(windows)]
#[test]
fn windows_bootstrap_log_refuses_reparse_points() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = bootstrap_probe(dir.path());

    // A junction where the log file should be: refused, nothing written.
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("dir");
    let log = dir.path().join("bootstrap.log");
    junction(&elsewhere, &log);
    assert!(daemon_command(&script, &log).is_err());
    assert!(fs::read_dir(&elsewhere).expect("read").next().is_none());

    // A file symlink, where this account may create one: refused before the
    // target is truncated.
    let target = dir.path().join("target");
    fs::write(&target, "do-not-truncate").expect("write target");
    let linked = dir.path().join("linked.log");
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
    match std::os::windows::fs::symlink_file(&target, &linked) {
        Ok(()) => {
            assert!(daemon_command(&script, &linked).is_err());
            assert_eq!(
                fs::read_to_string(&target).expect("read target"),
                "do-not-truncate"
            );
        }
        Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
            eprintln!("symlink creation not permitted here; link assertions skipped");
        }
        Err(error) => panic!("symlink_file: {error}"),
    }
}

#[cfg(windows)]
#[test]
fn windows_log_directory_junction_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    fs::create_dir(&real).expect("real dir");
    let link = dir.path().join("logs");
    junction(&real, &link);
    assert!(prepare_log_dir(&link).is_err());
    let fresh = dir.path().join("fresh").join("logs");
    prepare_log_dir(&fresh).expect("creates a missing dir");

    // Owner-only, like unix 0700, wherever BOARD_LOG_DIR points.
    let me = board_ipc::owner_only::current_user_sid().expect("sid");
    let sddl = board_ipc::owner_only::dacl_sddl(&fresh).expect("dacl");
    assert!(sddl.starts_with("D:P"), "{sddl}");
    assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
    assert!(sddl.contains(&format!("(A;OICI;FA;;;{me})")), "{sddl}");
}

#[cfg(windows)]
#[test]
fn windows_daemon_is_detached_from_the_cli_console_and_job() {
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
    };

    let flags = platform::creation_flags(true);
    assert_ne!(flags & CREATE_NEW_PROCESS_GROUP, 0, "own Ctrl+C group");
    assert_ne!(flags & CREATE_NO_WINDOW, 0, "hidden console of its own");
    assert_ne!(flags & CREATE_BREAKAWAY_FROM_JOB, 0, "leaves the CLI's job");
    assert_eq!(
        flags & DETACHED_PROCESS,
        0,
        "a console-less daemon would pop a window for every console child"
    );
    assert_eq!(
        platform::creation_flags(false),
        CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
    );
}

#[cfg(windows)]
#[test]
fn windows_marker_pid_requires_the_exact_marker_shape() {
    assert_eq!(platform::marker_pid("4242:1700000000000000000"), Some(4242));
    for foreign in [
        "", "4242", "4242:", ":1", "x:1", "1:x", "1:2\n", " 1:2", "1:2:3",
    ] {
        assert_eq!(platform::marker_pid(foreign), None, "{foreign:?}");
    }
}

#[cfg(windows)]
#[test]
fn windows_stale_marker_is_removed_only_while_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("boardd.sock");

    // The pipe is gone and the marker is the one first observed: stale.
    fs::write(&path, "4242:1").expect("marker");
    let original = platform::identity(&path);
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Gone
    );
    assert!(!path.exists());

    // A different start's marker is preserved.
    fs::write(&path, "4343:2").expect("marker");
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Replaced
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "4343:2");

    // A file that is not a boardd marker is never removed.
    fs::write(&path, "user data").expect("file");
    let original = platform::identity(&path);
    assert_eq!(
        check_listener_after_connect_failure(&path, &original),
        ListenerCheck::Replaced
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "user data");

    // Nor is a directory in its place.
    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("dir");
    assert_eq!(
        check_listener_after_connect_failure(&path, &None),
        ListenerCheck::Replaced
    );
    assert!(path.is_dir());
}

#[cfg(windows)]
#[test]
fn windows_stale_removal_tolerates_the_exiting_daemon_removing_its_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("boardd.sock");
    fs::write(&path, "4242:1").expect("marker");
    let original = platform::identity(&path);

    // The daemon deletes its own marker first: still a clean stop.
    assert_eq!(
        platform::remove_stale_with(&path, &original, |path| {
            fs::remove_file(path).expect("daemon removes");
            fs::remove_file(path)
        }),
        ListenerCheck::Gone
    );

    // A marker mid-deletion is looked at again, never a replacement.
    fs::write(&path, "4242:1").expect("marker");
    assert_eq!(
        platform::remove_stale_with(&path, &original, |_| {
            Err(io::ErrorKind::PermissionDenied.into())
        }),
        ListenerCheck::Live
    );
    assert_eq!(fs::read_to_string(&path).expect("kept"), "4242:1");

    // Any other removal failure still fails closed.
    assert_eq!(
        platform::remove_stale_with(&path, &original, |_| Err(io::Error::other("locked"))),
        ListenerCheck::Replaced
    );
}

#[cfg(windows)]
#[test]
fn windows_live_pipe_identity_is_its_server_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("boardd.sock");
    let _listener = LocalListener::bind(&path).expect("bind");
    let stream = platform::probe(&path).expect("connect");

    let ours = Some(format!("{}:1", std::process::id()));
    assert!(platform::serves_identity(&stream, &ours));
    let other = Some(format!("{}:1", std::process::id().wrapping_add(1)));
    assert!(!platform::serves_identity(&stream, &other));
    assert!(
        platform::serves_identity(&stream, &None),
        "no marker: nothing to contradict"
    );
}
