//! Path resolution tests. Pure parsing stays in-process; env-reading
//! resolvers are exercised through a child process so the parent test process
//! never mutates global environment state.

use std::process::Command;

use board_core::paths::session_name_from_socket;

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
fn windows_socket_paths_use_backslash_separators() {
    // `herdr session list --json` reports Windows socket paths with
    // backslashes; the named pipe is `\\.\pipe\` + that path verbatim.
    assert_eq!(
        session_name_from_socket(Some(
            r"C:\Users\x\AppData\Roaming\herdr\sessions\work\herdr.sock"
        )),
        Some("work".to_string())
    );
    assert_eq!(
        session_name_from_socket(Some(r"C:\Users\x\AppData\Roaming\herdr\herdr.sock")),
        None
    );
    // Mixed separators (e.g. a value assembled by a forward-slash tool).
    assert_eq!(
        session_name_from_socket(Some(
            "C:/Users/x/AppData/Roaming/herdr/sessions/work/herdr.sock"
        )),
        Some("work".to_string())
    );
    assert_eq!(
        session_name_from_socket(Some(r"C:\Users\x\herdr\sessions/work\herdr.sock")),
        Some("work".to_string())
    );
    for path in [
        r"C:\Users\x\herdr\sessions\herdr.sock",
        r"C:\Users\x\herdr\sessions\\herdr.sock",
        r"C:\Users\x\herdr\session\work\herdr.sock",
        r"C:\Users\x\herdr\sessions\work\herdr.socket",
        r"C:\Users\x\herdr\sessions\work\xherdr.sock",
        r"herdr.sock",
    ] {
        assert_eq!(
            session_name_from_socket(Some(path)),
            None,
            "expected {path:?} to read as the default session"
        );
    }
}

#[test]
fn home_dir_is_absolute_when_known() {
    // On unix this is `$HOME` verbatim; on Windows the profile folder. Either
    // way a resolved home is an absolute path the catalogs can join onto.
    if let Some(home) = board_core::paths::home_dir() {
        assert!(home.is_absolute(), "home dir must be absolute: {home:?}");
    }
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
