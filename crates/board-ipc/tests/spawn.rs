//! Detached spawn must hand the child only the handles it names.
#![cfg(windows)]

use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};

/// A plain `CreateProcess(bInheritHandles = TRUE)` would copy every inheritable
/// handle — like a caller's stdout pipe — into the long-lived child, and the
/// caller's reader would never see EOF.
#[test]
fn detached_child_does_not_inherit_unlisted_handles() {
    let (mut reader, writer) = std::io::pipe().unwrap();
    // SAFETY: valid handle owned by `writer`.
    let ok = unsafe {
        SetHandleInformation(
            writer.as_raw_handle() as HANDLE,
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    };
    assert_ne!(ok, 0);

    let dir = tempfile::tempdir().unwrap();
    let log = std::fs::File::create(dir.path().join("stderr.log")).unwrap();
    let cmd = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    let pid =
        board_ipc::spawn_detached(&cmd, &["/c", "ping", "-n", "4", "127.0.0.1"], log).unwrap();
    assert_ne!(pid, 0);

    drop(writer);
    let started = Instant::now();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the detached child inherited the pipe's write end"
    );
}

/// Without a console of its own, every console program the daemon later runs
/// (herdr, git, catalog probes) would pop a visible console window. The child
/// is this test binary re-run as `console_probe`, which reports on stderr.
#[test]
fn detached_child_gets_a_hidden_console() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("probe.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let exe = std::env::current_exe().unwrap();
    board_ipc::spawn_detached(
        &exe,
        &["console_probe", "--exact", "--ignored", "--nocapture"],
        log,
    )
    .unwrap();

    let started = Instant::now();
    let report = loop {
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        if let Some(line) = text.lines().find(|l| l.starts_with("console=")) {
            break line.to_string();
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "probe never reported"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(report, "console=true visible=false");
}

/// Child side of `detached_child_gets_a_hidden_console`.
#[test]
#[ignore = "child probe spawned by detached_child_gets_a_hidden_console"]
fn console_probe() {
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    // SAFETY: plain queries into a live local buffer; a null window is
    // handled before IsWindowVisible. A process with no console gets 0 from
    // GetConsoleProcessList; CREATE_NO_WINDOW's console has no window (null).
    let mut pids = [0u32; 16];
    let console = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } != 0;
    let window = unsafe { GetConsoleWindow() };
    let visible = !window.is_null() && unsafe { IsWindowVisible(window) } != 0;
    eprintln!("console={console} visible={visible}");
}
