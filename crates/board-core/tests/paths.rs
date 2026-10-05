//! Path resolution tests. Pure parsing stays in-process; env-reading
//! resolvers are exercised through a child process so the parent test process
//! never mutates global environment state.

use std::process::Command;

use board_core::paths::{open_private_file, session_name_from_socket};

#[test]
fn session_name_is_read_only_from_a_named_session_socket() {
    assert_eq!(
        session_name_from_socket(Some("/run/user/1000/herdr/sessions/work/herdr.sock")),
        Some("work".to_string())
    );
    // The default session's socket has no `sessions/<name>` segment.
    assert_eq!(
        session_name_from_socket(Some("/run/user/1000/herdr/herdr.sock")),
        None
    );
    // Unset means the default session too.
    assert_eq!(session_name_from_socket(None), None);
}

#[test]
fn malformed_socket_paths_fall_back_to_the_default_session() {
    for path in [
        "",
        "herdr.sock",
        "/run/user/1000/herdr/sessions/herdr.sock",
        "/run/user/1000/herdr/sessions//herdr.sock",
        "/run/user/1000/herdr/session/work/herdr.sock",
        "/run/user/1000/herdr/sessions/work/herdr.socket",
        "/run/user/1000/herdr/sessions/work/",
    ] {
        assert_eq!(
            session_name_from_socket(Some(path)),
            None,
            "expected {path:?} to read as the default session"
        );
    }
}

#[test]
fn log_dir_honors_the_board_log_dir_override() {
    const CHILD_ENV: &str = "BOARD_CORE_PATHS_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        println!("{}", board_core::paths::log_dir().display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "log_dir_honors_the_board_log_dir_override",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("BOARD_LOG_DIR", dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line == dir.path().to_str().unwrap()),
        "log_dir() must resolve to the BOARD_LOG_DIR override verbatim; got: {stdout:?}"
    );
}

#[test]
fn session_name_parses_windows_separators() {
    assert_eq!(
        session_name_from_socket(Some(
            r"C:\Users\a\AppData\Roaming\herdr\sessions\work\herdr.sock"
        )),
        Some("work".to_string())
    );
    assert_eq!(
        session_name_from_socket(Some(r"C:\Users\a\AppData\Roaming\herdr\herdr.sock")),
        None
    );
}

#[test]
fn open_private_file_refuses_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.log");
    std::fs::write(&target, b"").unwrap();
    let link = dir.path().join("link.log");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(windows)]
    if std::os::windows::fs::symlink_file(&target, &link).is_err() {
        return; // creating symlinks needs Developer Mode or admin; nothing to test
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    assert!(open_private_file(&mut options, &link).is_err());
}

#[test]
fn open_private_file_creates_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.log");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    open_private_file(&mut options, &path).unwrap();
    assert!(path.is_file());
}
