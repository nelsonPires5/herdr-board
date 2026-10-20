//! Program resolution + bounded subprocess runs (`board_core::exec`).
//!
//! Resolution is the identity on unix (`Command` keeps its own `PATH`
//! search) and a `PATH`/`PATHEXT` lookup on Windows, where npm-style installs
//! ship `<name>.cmd` shims that `Command::new("<name>")` alone never finds.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use board_core::exec::{resolve_program, run_bounded};

#[test]
fn empty_program_names_never_resolve() {
    assert_eq!(resolve_program(""), None);
    assert_eq!(resolve_program("   "), None);
}

#[cfg(unix)]
#[test]
fn unix_resolution_is_the_identity() {
    // Even a name that exists nowhere resolves to itself: the spawn reports
    // not-found exactly as it did before the resolver existed.
    for name in [
        "pi",
        "opencode",
        "/nonexistent/agy",
        "hb-definitely-missing",
    ] {
        assert_eq!(resolve_program(name), Some(PathBuf::from(name)));
    }
}

#[cfg(windows)]
#[test]
fn windows_resolution_applies_pathext_to_cmd_shims() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir.path().join("hb-shim.cmd");
    std::fs::write(&shim, "@echo off\r\necho shim\r\n").unwrap();

    // A path without the extension resolves to the `.cmd` shim.
    let resolved = resolve_program(dir.path().join("hb-shim").to_str().unwrap())
        .expect("the .cmd shim must resolve through PATHEXT");
    assert!(
        resolved
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("hb-shim.cmd"),
        "resolved {resolved:?}"
    );
    // System binaries on PATH resolve too.
    assert!(resolve_program("cmd").is_some());
    // Nothing matching → None.
    assert_eq!(resolve_program("hb-definitely-missing-program"), None);
}

#[test]
fn run_bounded_rejects_empty_argv_and_missing_programs() {
    assert!(run_bounded(&[], Duration::from_secs(5), 1024).is_none());
    let missing = vec!["hb-definitely-missing-program".to_string()];
    assert!(run_bounded(&missing, Duration::from_secs(5), 1024).is_none());
}

/// A fixture that prints `hello` and exits with `code`.
#[cfg(unix)]
fn echo_fixture(dir: &tempfile::TempDir, code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.path().join(format!("hb-echo-{code}"));
    std::fs::write(&bin, format!("#!/bin/sh\necho hello\nexit {code}\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[cfg(windows)]
fn echo_fixture(dir: &tempfile::TempDir, code: i32) -> PathBuf {
    let bin = dir.path().join(format!("hb-echo-{code}.cmd"));
    std::fs::write(
        &bin,
        format!("@echo off\r\necho hello\r\nexit /b {code}\r\n"),
    )
    .unwrap();
    // Hand the resolver the extension-less path, as callers do with `pi`.
    dir.path().join(format!("hb-echo-{code}"))
}

#[test]
fn run_bounded_captures_stdout_and_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let ok = echo_fixture(&dir, 0);
    let out = run_bounded(
        &[ok.to_string_lossy().into_owned()],
        Duration::from_secs(20),
        1024,
    )
    .expect("a quick fixture completes within the budget");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");

    // A non-zero exit is still output; the caller inspects the status.
    let failing = echo_fixture(&dir, 3);
    let out = run_bounded(
        &[failing.to_string_lossy().into_owned()],
        Duration::from_secs(20),
        1024,
    )
    .expect("a failing fixture still completes");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn run_bounded_rejects_output_past_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let bin = echo_fixture(&dir, 0);
    // `hello` plus a newline is more than 3 bytes.
    assert!(run_bounded(
        &[bin.to_string_lossy().into_owned()],
        Duration::from_secs(20),
        3
    )
    .is_none());
}

/// A fixture whose shell is killed on timeout while a grandchild keeps the
/// stdout pipe open — the shape of a Windows `.cmd` shim whose `node` child
/// survives `cmd.exe` being killed. Joining the reader would block until the
/// grandchild exits; the bounded run must return near its own deadline.
#[cfg(unix)]
fn pipe_holding_fixture(dir: &tempfile::TempDir) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.path().join("hb-pipe-holder");
    // No `exec`: `sh` is the killed child and the backgrounded `sleep`
    // inherits stdout.
    std::fs::write(&bin, "#!/bin/sh\nsleep 20 &\nwait\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[cfg(windows)]
fn pipe_holding_fixture(dir: &tempfile::TempDir) -> PathBuf {
    let bin = dir.path().join("hb-pipe-holder.cmd");
    // `ping` writes to the inherited stdout pipe and outlives `cmd.exe`.
    std::fs::write(&bin, "@ping -n 20 127.0.0.1\r\n").unwrap();
    bin
}

#[test]
fn run_bounded_timeout_does_not_wait_for_a_grandchild_holding_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let bin = pipe_holding_fixture(&dir);
    let started = Instant::now();
    let out = run_bounded(
        &[bin.to_string_lossy().into_owned()],
        Duration::from_secs(2),
        1024 * 1024,
    );
    let elapsed = started.elapsed();
    assert!(out.is_none(), "a hung CLI must yield None");
    assert!(
        elapsed < Duration::from_secs(12),
        "the timeout path must not block on the pipe-holding grandchild (took {elapsed:?})"
    );
}
