use super::*;

#[test]
fn local_materializer_preserves_pi_historic_prompt_flag_and_card_argument() {
    let argv = materialize_local_argv(&managed_req("pi")).unwrap();
    assert_eq!(
        argv,
        vec![
            "pi",
            "--model",
            "m",
            "--append-system-prompt",
            "old system\nsecond line",
            "--session-id",
            "s",
            "Card task:\nexact task",
        ]
    );
}

#[test]
fn local_materializer_preserves_claude_flag_order_and_final_prompt() {
    let argv = materialize_local_argv(&managed_req("claude")).unwrap();
    assert_eq!(
        argv,
        vec![
            "claude",
            "--model",
            "m",
            "--append-system-prompt",
            "old system\nsecond line",
            "--allowedTools",
            "Bash(*)",
            "--",
            "exact task",
        ]
    );
}

#[test]
fn local_materializer_leaves_configured_argv_untouched() {
    let mut req = managed_req("custom");
    req.agent_kind = None;
    req.initial_prompt = None;
    req.system_prompt = None;
    req.argv = vec!["configured".into(), "literal\nargument".into()];
    assert_eq!(materialize_local_argv(&req).unwrap(), req.argv);
}

#[test]
fn local_materializer_rejects_incomplete_or_unknown_managed_metadata() {
    let mut missing = managed_req("pi");
    missing.system_prompt = None;
    let err = materialize_local_argv(&missing).unwrap_err();
    assert!(err.to_string().contains("system_prompt"));

    let err = materialize_local_argv(&managed_req("other")).unwrap_err();
    assert!(err.to_string().contains("managed") || err.to_string().contains("harness"));
}

use super::super::local::batch_argv_problem;
#[cfg(windows)]
use super::super::LocalSpawner;

#[test]
fn batch_argv_problem_flags_only_line_breaks_handed_to_a_batch_file() {
    let shim = Path::new(r"C:\npm\pi.cmd");
    let multiline = vec!["--append-system-prompt".to_string(), "a\nb".to_string()];
    let problem = batch_argv_problem(shim, &multiline).expect("LF cannot reach a .cmd");
    assert!(problem.contains("argument 2"), "{problem}");
    assert!(problem.contains("real executable"), "{problem}");
    assert!(batch_argv_problem(Path::new(r"C:\x\run.BAT"), &["a\rb".to_string()]).is_some());

    // Single-line arguments, and any argument to a real executable, are fine.
    assert!(batch_argv_problem(shim, &["one line with \"quotes\" & %VAR%".to_string()]).is_none());
    assert!(batch_argv_problem(Path::new(r"C:\bin\pi.exe"), &multiline).is_none());
    assert!(batch_argv_problem(Path::new("/usr/bin/pi"), &multiline).is_none());
}

#[cfg(windows)]
#[test]
fn local_spawner_refuses_multiline_argv_for_a_cmd_shim_with_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir.path().join("pi.cmd");
    std::fs::write(&shim, "@echo off\r\nexit /b 0\r\n").unwrap();
    let mut req = managed_req("pi");
    req.argv[0] = shim.to_string_lossy().into_owned();

    let err = LocalSpawner::new().spawn(&req).unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("batch file"), "{message}");
    assert!(message.contains("line break"), "{message}");
}

#[cfg(windows)]
#[test]
fn local_kill_takes_down_everything_a_cmd_shim_started() {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    // A `.cmd` shim whose real work runs in a grandchild, the npm shape:
    // killing only the shim's cmd.exe would leave the grandchild running.
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("grandchild.pid");
    let shim = dir.path().join("agent shim.cmd");
    std::fs::write(
        &shim,
        format!(
            "@echo off\r\npowershell.exe -NoProfile -Command \"Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep -Seconds 120\"\r\n",
            pid_file.display()
        ),
    )
    .unwrap();
    let mut req = managed_req("custom");
    req.agent_kind = None;
    req.argv = vec![shim.to_string_lossy().into_owned()];

    let spawner = LocalSpawner::new();
    let handle = spawner.spawn(&req).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let grandchild = loop {
        let pid = std::fs::read_to_string(&pid_file).ok().and_then(|text| {
            text.trim_matches(['\u{feff}', '\r', '\n', ' '])
                .parse::<u32>()
                .ok()
        });
        if let Some(pid) = pid {
            break pid;
        }
        assert!(Instant::now() < deadline, "the grandchild never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(spawner.is_alive(&handle).unwrap());

    // SAFETY: plain Win32 calls on a handle this test owns and closes.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, grandchild) };
    assert!(
        !process.is_null(),
        "the grandchild must be running before kill"
    );
    spawner.kill(&handle).unwrap();
    let waited = unsafe { WaitForSingleObject(process, 10_000) };
    unsafe { CloseHandle(process) };
    assert_eq!(
        waited, WAIT_OBJECT_0,
        "killing the shim must also end the process it started"
    );
    assert!(!spawner.is_alive(&handle).unwrap());
}
