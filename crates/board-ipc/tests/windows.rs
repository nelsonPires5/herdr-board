//! Named-pipe specifics: naming, busy vs absent, server identity.
#![cfg(windows)]

mod support;

use std::io;
use std::path::Path;
use std::time::Duration;

use board_ipc::{endpoint, owner_only, LocalStream};
use support::listener;

/// ERROR_PIPE_BUSY.
const PIPE_BUSY: i32 = 231;

#[test]
fn pipe_name_is_the_prefix_plus_the_path_verbatim() {
    let name =
        endpoint::pipe_name(Path::new(r"C:\Users\jon\AppData\Roaming\herdr\herdr.sock")).unwrap();
    assert_eq!(
        name,
        r"\\.\pipe\C:\Users\jon\AppData\Roaming\herdr\herdr.sock"
    );
    // Forward slashes are not normalized: Herdr uses the string verbatim.
    let name = endpoint::pipe_name(Path::new("C:/x/herdr.sock")).unwrap();
    assert_eq!(name, r"\\.\pipe\C:/x/herdr.sock");
}

#[test]
fn pipe_name_strips_a_verbatim_prefix() {
    let name = endpoint::pipe_name(Path::new(r"\\?\C:\x\boardd.sock")).unwrap();
    assert_eq!(name, r"\\.\pipe\C:\x\boardd.sock");
    let name = endpoint::pipe_name(Path::new(r"\\?\UNC\srv\share\b.sock")).unwrap();
    assert_eq!(name, r"\\.\pipe\\\srv\share\b.sock");
}

#[test]
fn pipe_name_passes_an_existing_pipe_name_through() {
    let name = endpoint::pipe_name(Path::new(r"\\.\pipe\custom")).unwrap();
    assert_eq!(name, r"\\.\pipe\custom");
}

#[test]
fn pipe_name_rejects_names_over_256_units() {
    let fits = format!(r"C:\{}", "x".repeat(256 - 9 - 3));
    assert!(endpoint::pipe_name(Path::new(&fits)).is_ok());
    let too_long = format!("{fits}x");
    let error = endpoint::pipe_name(Path::new(&too_long)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("too long"), "{error}");
    let error = LocalStream::connect(Path::new(&too_long)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!endpoint::is_absent(&error));
}

#[test]
fn a_busy_pipe_is_not_absent() {
    assert!(!endpoint::is_absent(&io::Error::from_raw_os_error(
        PIPE_BUSY
    )));

    // The one listening instance is taken by the first client; with no
    // accept to create the next, the second client sees a busy pipe and
    // times out instead of reporting "nothing is listening".
    let (_dir, path, _listener) = listener();
    let _first = LocalStream::connect_with_deadline(&path, Duration::from_secs(5)).unwrap();
    let error = LocalStream::connect_with_deadline(&path, Duration::from_millis(200)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error:?}");
    assert!(!endpoint::is_absent(&error));
}

#[test]
fn server_process_id_names_the_listening_process() {
    let (_dir, path, _listener) = listener();
    let client = LocalStream::connect(&path).unwrap();
    assert_eq!(client.server_process_id().unwrap(), std::process::id());
}

#[test]
fn nothing_is_created_on_disk() {
    let (_dir, path, _listener) = listener();
    assert!(!path.exists());
}

#[test]
fn a_client_accepts_a_pipe_this_user_created() {
    // The server-ownership check passes for our own listener (blocking and
    // nothing else is needed to reach it).
    let (_dir, path, _listener) = listener();
    LocalStream::connect_with_deadline(&path, Duration::from_secs(5)).unwrap();
}

#[test]
fn restrict_dir_leaves_only_the_current_user() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    let existing = logs.join("before.log");
    std::fs::write(&existing, "x").unwrap();

    owner_only::restrict_dir(&logs).unwrap();
    let me = owner_only::current_user_sid().unwrap();
    let sddl = owner_only::dacl_sddl(&logs).unwrap();
    assert!(
        sddl.starts_with("D:P"),
        "protected, nothing inherited: {sddl}"
    );
    assert_eq!(sddl.matches('(').count(), 1, "one entry: {sddl}");
    assert!(sddl.contains(&format!("(A;OICI;FA;;;{me})")), "{sddl}");

    // Files already inside and files created later carry only that entry.
    let created = logs.join("after.log");
    std::fs::write(&created, "y").unwrap();
    for child in [existing, created] {
        let sddl = owner_only::dacl_sddl(&child).unwrap();
        assert_eq!(sddl.matches('(').count(), 1, "{}: {sddl}", child.display());
        assert!(sddl.contains(&format!("(A;ID;FA;;;{me})")), "{sddl}");
    }
}
