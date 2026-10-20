use super::*;

#[test]
fn configured_rename_pane_race_ignores_vanished_cleanup_and_retries_allocation() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let allocations = Arc::new(AtomicUsize::new(0));
    let allocations2 = Arc::clone(&allocations);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => {
            let pane_id = if allocations2.fetch_add(1, Ordering::SeqCst) == 0 {
                "w1:p-rename-race-first"
            } else {
                "w1:p-rename-race-second"
            };
            tab_created(req, pane_id)
        }
        "pane.rename" => {
            if req["params"]["pane_id"] == "w1:p-rename-race-first" {
                error(req, "pane_not_found", "owned pane vanished during rename")
            } else {
                pane_result(req, "w1:p-rename-race-second")
            }
        }
        "pane.close" => error(req, "pane_not_found", "owned pane already vanished"),
        method => panic!("unexpected configured rename race method {method}"),
    });
    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner = RecordingPaneRunner {
        calls,
        behavior: Box::new(move |_, argv| {
            assert_eq!(argv[2], "w1:p-rename-race-second");
            Ok(())
        }),
    };
    let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));
    let cwd = tempfile::tempdir().unwrap();

    let handle = spawner
        .spawn(&custom_req(
            fake.socket.clone(),
            cwd.path().to_path_buf(),
            vec!["configured-agent".into()],
        ))
        .unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-rename-race-second"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "tab.create")
            .count(),
        2,
        "rename disappearance must restart allocation rather than reuse stale ownership"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.close")
            .map(|request| request["params"]["pane_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["w1:p-rename-race-first"],
        "cleanup may target only the first board-owned pane"
    );
}

#[test]
fn configured_runner_pane_not_found_retries_but_generic_runner_error_is_terminal() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let allocations = Arc::new(AtomicUsize::new(0));
    let allocations2 = Arc::clone(&allocations);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => {
            let pane_id = if allocations2.fetch_add(1, Ordering::SeqCst) == 0 {
                "w1:p-runner-race-first"
            } else {
                "w1:p-runner-race-second"
            };
            tab_created(req, pane_id)
        }
        "pane.rename" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        "pane.close" => error(req, "pane_not_found", "runner observed vanished pane"),
        method => panic!("unexpected configured runner race method {method}"),
    });
    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner_calls = Arc::new(AtomicUsize::new(0));
    let runner_calls2 = Arc::clone(&runner_calls);
    let runner = RecordingPaneRunner {
        calls,
        behavior: Box::new(move |_, argv| {
            let call = runner_calls2.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                assert_eq!(argv[2], "w1:p-runner-race-first");
                Err(anyhow::Error::new(board_herdr::HerdrError::Protocol {
                    code: "pane_not_found".into(),
                    message: "CLI pane disappeared after scheduling".into(),
                }))
            } else {
                assert_eq!(argv[2], "w1:p-runner-race-second");
                Ok(())
            }
        }),
    };
    let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));
    let cwd = tempfile::tempdir().unwrap();
    let handle = spawner
        .spawn(&custom_req(
            fake.socket.clone(),
            cwd.path().to_path_buf(),
            vec!["configured-agent".into()],
        ))
        .unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-runner-race-second"));
    assert_eq!(runner_calls.load(Ordering::SeqCst), 2);

    let generic_fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p-generic-terminal"),
        "pane.rename" => pane_result(req, "w1:p-generic-terminal"),
        "pane.close" => pane_result(req, "w1:p-generic-terminal"),
        method => panic!("unexpected generic runner method {method}"),
    });
    let generic_calls = Arc::new(AtomicUsize::new(0));
    let generic_calls2 = Arc::clone(&generic_calls);
    let generic_runner = RecordingPaneRunner {
        calls: Arc::new(Mutex::new(Vec::new())),
        behavior: Box::new(move |_, _| {
            generic_calls2.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("runner crashed generically"))
        }),
    };
    let generic_spawner =
        HerdrSpawner::with_pane_runner(generic_fake.socket.clone(), Arc::new(generic_runner));
    let generic_err = generic_spawner
        .spawn(&custom_req(
            generic_fake.socket.clone(),
            cwd.path().to_path_buf(),
            vec!["configured-agent".into()],
        ))
        .unwrap_err();
    assert!(generic_err
        .to_string()
        .contains("runner crashed generically"));
    assert_eq!(generic_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        generic_fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "tab.create")
            .count(),
        1,
        "generic runner failures must remain terminal"
    );
}

/// The PowerShell host the Windows startup line names, chosen exactly as the
/// spawner chooses it.
#[cfg(windows)]
fn powershell_host() -> &'static str {
    if board_core::exec::resolve_program("pwsh").is_some() {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

/// An absolute `python` for the Windows recorders: a native executable whose
/// `sys.argv` shows exactly the argument boundaries it was handed.
#[cfg(windows)]
fn windows_python() -> String {
    board_core::exec::resolve_program("python")
        .expect("the Windows configured-harness tests need `python` on PATH")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn configured_pane_runner_resolves_herdr_bin_path_without_live_herdr() {
    // Run the real CLI runner in a child test process so HERDR_BIN_PATH is
    // configured for that process only. The empty PATH makes a hardcoded
    // `herdr` lookup fail rather than accidentally invoking a live Herdr.
    const CHILD_MARKER: &str = "HB_SPAWNER_BIN_PATH_TEST_CHILD";
    const CHILD_SOCKET: &str = "HB_SPAWNER_BIN_PATH_TEST_SOCKET";
    if std::env::var_os(CHILD_MARKER).is_some() {
        let socket = PathBuf::from(std::env::var_os(CHILD_SOCKET).unwrap());
        HerdrCliPaneRunner
            .run(&socket, &["pane".into(), "run".into(), "w1:p-bin".into()])
            .unwrap();
        return;
    }

    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let invocation = dir.path().join("invocation");
    let socket_capture = dir.path().join("socket");
    let socket = dir.path().join("selected.sock");
    let empty_path = dir.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    let recorder = write_herdr_recorder(
        dir.path(),
        "herdr-bin-recorder",
        &invocation,
        &socket_capture,
    );

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("configured_pane_runner_resolves_herdr_bin_path_without_live_herdr")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CHILD_SOCKET, &socket)
        .env("HERDR_BIN_PATH", &recorder)
        .env("PATH", &empty_path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the configured PaneRunner must execute HERDR_BIN_PATH, not literal `herdr`"
    );
    assert_eq!(read_recorded_args(&invocation), ["pane", "run", "w1:p-bin"]);
    assert_eq!(
        read_recorded_text(&socket_capture),
        socket.to_string_lossy()
    );
}

#[cfg(windows)]
#[test]
fn configured_pane_runner_resolves_a_herdr_cmd_shim_through_pathext() {
    // With HERDR_BIN_PATH unset the runner spawns the bare name `herdr`. On
    // Windows that must find a `herdr.cmd` shim on PATH (PATHEXT), not only a
    // `herdr.exe`, the way a shell would.
    const CHILD_MARKER: &str = "HB_SPAWNER_HERDR_SHIM_TEST_CHILD";
    const CHILD_SOCKET: &str = "HB_SPAWNER_HERDR_SHIM_TEST_SOCKET";
    if std::env::var_os(CHILD_MARKER).is_some() {
        let socket = PathBuf::from(std::env::var_os(CHILD_SOCKET).unwrap());
        HerdrCliPaneRunner
            .run(&socket, &["pane".into(), "run".into(), "w1:p-shim".into()])
            .unwrap();
        return;
    }

    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let invocation = dir.path().join("invocation");
    let socket_capture = dir.path().join("socket");
    let socket = dir.path().join("selected.sock");
    write_herdr_recorder(&bin, "herdr", &invocation, &socket_capture);

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("configured_pane_runner_resolves_a_herdr_cmd_shim_through_pathext")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CHILD_SOCKET, &socket)
        .env_remove("HERDR_BIN_PATH")
        .env("PATH", &bin)
        .status()
        .unwrap();
    assert!(status.success(), "a herdr.cmd shim on PATH must resolve");
    assert_eq!(
        read_recorded_args(&invocation),
        ["pane", "run", "w1:p-shim"]
    );
    assert_eq!(
        read_recorded_text(&socket_capture),
        socket.to_string_lossy()
    );
}

/// Write a fake `herdr` that records its argv to `invocation` and its
/// `HERDR_SOCKET_PATH` to `socket_capture`: a mode-0700 sh script on unix, a
/// `.cmd` batch file on Windows (`stem` plus the platform extension).
fn write_herdr_recorder(
    dir: &Path,
    stem: &str,
    invocation: &Path,
    socket_capture: &Path,
) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let recorder = dir.join(format!("{stem}.sh"));
        std::fs::write(
            &recorder,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf '%s' \"${{HERDR_SOCKET_PATH:-}}\" > {}\n",
                posix_quote(&invocation.to_string_lossy()),
                posix_quote(&socket_capture.to_string_lossy()),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
        recorder
    }
    #[cfg(windows)]
    {
        let recorder = dir.join(format!("{stem}.cmd"));
        std::fs::write(
            &recorder,
            format!(
                "@echo off\r\n>\"{}\" echo %*\r\n>\"{}\" echo %HERDR_SOCKET_PATH%\r\n",
                invocation.display(),
                socket_capture.display(),
            ),
        )
        .unwrap();
        recorder
    }
}

/// The argv a [`write_herdr_recorder`] fake recorded. The batch recorder
/// can only echo `%*`, so its (space-free) arguments are split on spaces.
fn read_recorded_args(path: &Path) -> Vec<String> {
    if cfg!(windows) {
        read_recorded_text(path)
            .split(' ')
            .map(str::to_string)
            .collect()
    } else {
        std::fs::read(path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect()
    }
}

/// A recorded text value, without the line break a batch `echo` appends.
fn read_recorded_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    if cfg!(windows) {
        text.trim_end_matches(['\r', '\n']).to_string()
    } else {
        text
    }
}

#[test]
fn configured_harness_uses_selected_socket_pane_run_with_exact_payload() {
    use std::process::Command;

    let command_dir = tempfile::tempdir().unwrap();
    let cwd = command_dir.path().join("custom cwd with spaces");
    std::fs::create_dir(&cwd).unwrap();
    let recorder = command_dir.path().join("custom command's recorder.py");
    let capture = command_dir.path().join("captured invocation.json");
    let recorder_body = format!(
        "import json, os, sys\nkeys = ['BOARD_PROMPT', 'BOARD_SYSTEM_PROMPT', 'HERDR_SOCKET_PATH']\njson.dump({{'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': {{k: os.environ[k] for k in keys}}}}, open({:?}, 'w', encoding='utf-8'))\n",
        capture
    );
    // unix runs the recorder itself (shebang + exec bit). Windows has
    // neither, so the configured argv names a bare `python` and the recorder
    // as its first argument: the bare name also exercises the PATH/PATHEXT
    // resolution the generated script relies on.
    #[cfg(unix)]
    let (exact_argv, recorded_from) = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &recorder,
            format!("#!/usr/bin/env python3\n{recorder_body}"),
        )
        .unwrap();
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
        (
            vec![
                recorder.to_string_lossy().into_owned(),
                "single'quote".into(),
                "literal argument with spaces".into(),
                "line one\nline two".into(),
            ],
            1,
        )
    };
    #[cfg(windows)]
    let (exact_argv, recorded_from) = {
        std::fs::write(&recorder, &recorder_body).unwrap();
        let mut argv: Vec<String> = vec![
            "python".into(),
            recorder.to_string_lossy().into_owned(),
            "single'quote".into(),
            "literal argument with spaces".into(),
            "line one\nline two".into(),
            "$env:USERNAME `n %PATH% ! & | ; ( )".into(),
            "typographic \u{2019}quote\u{2018} and unicode \u{2713} \u{fc}".into(),
            r"trailing backslash\".into(),
        ];
        if powershell_host() == "pwsh.exe" {
            // pwsh 7.3+ passes native arguments exactly; Windows PowerShell
            // 5.1's legacy passing would drop "" and mangle embedded quotes.
            argv.extend(["say \"hi\"".to_string(), String::new()]);
        }
        (argv, 2)
    };

    let selected = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p9"),
        "pane.rename" => pane_result(req, "w1:p9"),
        method => panic!("configured harness must not call managed/send-text method {method}"),
    });
    let default =
        serve_recording_herdr(|req, _| panic!("request incorrectly used default socket: {req}"));

    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner_path = Arc::new(Mutex::new(None::<PathBuf>));
    let runner_path2 = Arc::clone(&runner_path);
    let runner_socket = selected.socket.clone();
    let runner = RecordingPaneRunner {
        calls: Arc::clone(&calls),
        behavior: Box::new(move |socket, argv| {
            assert_eq!(socket, runner_socket.as_path());
            assert_eq!(&argv[..3], ["pane", "run", "w1:p9"]);
            assert_eq!(
                argv.len(),
                4,
                "the typed startup line must be one shell-free argv item"
            );
            let path = pane_run_script_path(argv).expect("startup script path");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(argv[3], path.to_string_lossy());
                assert_eq!(
                    std::fs::metadata(&path)?.permissions().mode() & 0o777,
                    0o700,
                    "startup script must be executable only by its owner",
                );
            }
            #[cfg(windows)]
            {
                assert_eq!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("ps1"),
                    "PowerShell -File only runs a .ps1 path",
                );
                assert_eq!(
                    argv[3],
                    windows_pane_command(powershell_host(), &path.to_string_lossy())?,
                );
            }
            *runner_path2.lock().unwrap() = Some(path.clone());

            // The configured runner only schedules the script. The pane
            // process opens it after the runner returns.
            Ok(())
        }),
    };
    let spawner = HerdrSpawner::with_pane_runner(default.socket.clone(), Arc::new(runner));

    let handle = spawner
        .spawn(&custom_req(
            selected.socket.clone(),
            cwd.clone(),
            exact_argv.clone(),
        ))
        .unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p9"));
    assert_eq!(
        handle.herdr_socket.as_deref(),
        Some(selected.socket.as_path())
    );

    let path = runner_path.lock().unwrap().clone().unwrap();
    let call = calls.lock().unwrap().clone();
    assert_eq!(call.len(), 1);
    assert_eq!(
        call[0].socket, selected.socket,
        "exactly one CLI call must target the selected session",
    );
    assert_eq!(pane_run_script_path(&call[0].argv), Some(path.clone()));
    assert!(
        path.exists(),
        "runner success must return before the pane opens the startup script"
    );

    // Simulate the selected pane opening the script after pane.run has
    // returned, including the pane's cwd and environment. unix executes the
    // script path; Windows types the recorded line into a cmd.exe shell (the
    // least capable pane shell it must parse in).
    #[cfg(unix)]
    let mut pane = Command::new(&path);
    #[cfg(windows)]
    let mut pane = {
        use std::os::windows::process::CommandExt;
        let mut pane = Command::new("cmd.exe");
        pane.raw_arg(format!("/d /s /c \"{}\"", call[0].argv[3]));
        pane
    };
    let status = pane
        .current_dir(&cwd)
        .env(
            "BOARD_PROMPT",
            "configured task line one\nconfigured task line two",
        )
        .env(
            "BOARD_SYSTEM_PROMPT",
            "configured system line one\nconfigured system line two",
        )
        .env("HERDR_SOCKET_PATH", selected.socket.as_path())
        .status()
        .unwrap();
    assert!(status.success(), "fake pane launch payload failed");
    assert!(
        !path.exists(),
        "startup script must self-remove when the pane opens it"
    );

    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(capture).unwrap()).unwrap();
    assert_eq!(
        recorded["argv"],
        serde_json::json!(exact_argv[recorded_from..])
    );
    let recorded_cwd = PathBuf::from(recorded["cwd"].as_str().unwrap());
    assert_eq!(
        dunce::canonicalize(recorded_cwd).unwrap(),
        dunce::canonicalize(&cwd).unwrap()
    );
    assert_eq!(
        recorded["env"],
        serde_json::json!({
            "BOARD_PROMPT": "configured task line one\nconfigured task line two",
            "BOARD_SYSTEM_PROMPT": "configured system line one\nconfigured system line two",
            "HERDR_SOCKET_PATH": selected.socket.to_string_lossy(),
        }),
        "configured payload must receive exact multiline prompt/system env and selected socket",
    );

    let requests = selected.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(methods[..3], ["ping", "tab.list", "tab.create"]);
    assert_eq!(
        requests[2]["params"],
        serde_json::json!({
            "workspace_id": "w1",
            "label": "kanban",
            "cwd": cwd.to_string_lossy(),
            "env": {
                "BOARD_PROMPT": "configured task line one\nconfigured task line two",
                "BOARD_SYSTEM_PROMPT": "configured system line one\nconfigured system line two",
                "HERDR_SOCKET_PATH": selected.socket.to_string_lossy(),
            },
            "focus": false,
        }),
        "tab placement establishes the configured child cwd and environment",
    );
    assert!(requests.iter().all(|r| {
        !matches!(
            r["method"].as_str(),
            Some("agent.start" | "pane.send_text" | "pane.send_keys")
        )
    }));
    assert!(default.requests.lock().unwrap().is_empty());
}

#[test]
fn recording_runner_drop_removes_only_recorded_startup_scripts() {
    let selected = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p-drop-cleanup"),
        "pane.rename" => pane_result(req, "w1:p-drop-cleanup"),
        method => panic!("unexpected configured-runner method {method}"),
    });
    let cwd = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner = RecordingPaneRunner {
        calls: Arc::clone(&calls),
        behavior: Box::new(|_, _| Ok(())),
    };
    let spawner = HerdrSpawner::with_pane_runner(selected.socket.clone(), Arc::new(runner));

    spawner
        .spawn(&custom_req(
            selected.socket.clone(),
            cwd.path().to_path_buf(),
            vec!["configured-agent".into()],
        ))
        .unwrap();
    let recorded_path = pane_run_script_path(&calls.lock().unwrap()[0].argv).unwrap();
    let decoy_path = recorded_path.with_file_name("herdr-board-run-decoy");
    std::fs::write(&decoy_path, "not a configured startup script\n").unwrap();
    assert!(recorded_path.exists());

    drop(spawner);

    assert!(
        !recorded_path.exists(),
        "dropping the recording runner must remove its unexecuted script"
    );
    assert!(
        decoy_path.exists(),
        "cleanup must not remove an unrecorded same-prefix file"
    );
    std::fs::remove_file(&decoy_path).unwrap();
    assert!(
        !decoy_path.exists(),
        "the test must leave no decoy artifact"
    );
}

#[cfg(unix)]
#[test]
fn configured_script_runs_child_then_reports_silent_exit_and_preserves_status() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("child with spaces.sh");
    let child_capture = dir.path().join("child-argv");
    std::fs::write(
        &child,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nexit 23\n",
            posix_quote(&child_capture.to_string_lossy())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
    let board_bin = dir.path().join("board bin recorder.sh");
    let board_capture = dir.path().join("board-argv");
    std::fs::write(
        &board_bin,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nexit 1\n",
            posix_quote(&board_capture.to_string_lossy())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&board_bin, std::fs::Permissions::from_mode(0o700)).unwrap();

    let script_path = dir.path().join("startup script");
    let exact_argv = vec![
        child.to_string_lossy().into_owned(),
        "argument with spaces".into(),
        "line one\nline two".into(),
        "single'quote".into(),
    ];
    std::fs::write(&script_path, configured_script(&script_path, &exact_argv)).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();

    let status = Command::new(&script_path)
        .env("BOARD_BIN", &board_bin)
        .env("BOARD_CARD_ID", "card-42")
        .env("BOARD_RUN_ID", "run-42")
        .env("CHILD_CAPTURE", &child_capture)
        .env("BOARD_CAPTURE", &board_capture)
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(23),
        "the child status must be preserved"
    );
    assert!(
        !script_path.exists(),
        "the startup script removes itself first"
    );

    let nul_args = |path: &std::path::Path| {
        std::fs::read(path)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(&nul_args(&child_capture), &exact_argv[1..]);
    assert_eq!(
        nul_args(&board_capture),
        vec!["__pane-exited", "--run-id", "run-42"]
    );
}

/// A Windows fixture for the generated `.ps1`: a python child that records
/// its argv as JSON and exits 23, and a `board` batch recorder that echoes
/// its arguments and fails (the callback's failure must be ignored).
#[cfg(windows)]
struct Ps1Fixture {
    dir: tempfile::TempDir,
    child: PathBuf,
    child_capture: PathBuf,
    board_bin: PathBuf,
    board_capture: PathBuf,
    script_path: PathBuf,
}

#[cfg(windows)]
impl Ps1Fixture {
    fn new() -> Ps1Fixture {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child with spaces.py");
        let child_capture = dir.path().join("child-argv.json");
        std::fs::write(
            &child,
            format!(
                "import json, sys\njson.dump(sys.argv[1:], open({:?}, 'w', encoding='utf-8'))\nsys.exit(23)\n",
                child_capture
            ),
        )
        .unwrap();
        let board_bin = dir.path().join("board bin recorder.cmd");
        let board_capture = dir.path().join("board-argv");
        std::fs::write(
            &board_bin,
            format!(
                "@echo off\r\n>\"{}\" echo %*\r\nexit /b 1\r\n",
                board_capture.display()
            ),
        )
        .unwrap();
        let script_path = dir.path().join("startup script.ps1");
        Ps1Fixture {
            dir,
            child,
            child_capture,
            board_bin,
            board_capture,
            script_path,
        }
    }

    /// Generate the script for `argv` and run it the way the typed pane line
    /// does (`<host> -NoProfile -ExecutionPolicy Bypass -File <path>`).
    fn run(&self, argv: &[String], board_bin: Option<&Path>) -> std::process::ExitStatus {
        std::fs::write(
            &self.script_path,
            configured_script_ps1(&self.script_path, argv),
        )
        .unwrap();
        let mut command = std::process::Command::new(powershell_host());
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&self.script_path)
            .current_dir(self.dir.path())
            .env("BOARD_CARD_ID", "card-42")
            .env("BOARD_RUN_ID", "run-42");
        match board_bin {
            Some(bin) => command.env("BOARD_BIN", bin),
            None => command.env_remove("BOARD_BIN"),
        };
        command.status().unwrap()
    }
}

#[cfg(windows)]
#[test]
fn configured_ps1_runs_child_then_reports_silent_exit_and_preserves_status() {
    let fixture = Ps1Fixture::new();
    let child_args: Vec<String> = vec![
        "argument with spaces".into(),
        "line one\nline two".into(),
        "single'quote".into(),
        "$env:BOARD_RUN_ID `t %BOARD_RUN_ID%".into(),
        "typographic \u{2019}quote\u{201b} \u{2713}".into(),
    ];
    let mut argv = vec![
        windows_python(),
        fixture.child.to_string_lossy().into_owned(),
    ];
    argv.extend(child_args.iter().cloned());

    let status = fixture.run(&argv, Some(&fixture.board_bin));
    assert_eq!(
        status.code(),
        Some(23),
        "the child status must be preserved"
    );
    assert!(
        !fixture.script_path.exists(),
        "the startup script removes itself first"
    );
    let recorded: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(&fixture.child_capture).unwrap()).unwrap();
    assert_eq!(recorded, child_args, "every argument arrives verbatim");
    assert_eq!(
        read_recorded_text(&fixture.board_capture),
        "__pane-exited --run-id run-42"
    );
}

#[cfg(windows)]
#[test]
fn configured_ps1_reports_a_missing_program_as_127_and_needs_board_bin_for_the_callback() {
    let fixture = Ps1Fixture::new();
    let missing = fixture.dir.path().join("no such program.exe");
    let argv = vec![missing.to_string_lossy().into_owned(), "arg".into()];

    // sh reports an unrunnable program as 127 and still calls back.
    let status = fixture.run(&argv, Some(&fixture.board_bin));
    assert_eq!(status.code(), Some(127));
    assert!(!fixture.script_path.exists());
    assert_eq!(
        read_recorded_text(&fixture.board_capture),
        "__pane-exited --run-id run-42"
    );

    // Without BOARD_BIN there is no callback, and the status still survives.
    std::fs::remove_file(&fixture.board_capture).unwrap();
    let argv = vec![
        windows_python(),
        fixture.child.to_string_lossy().into_owned(),
    ];
    let status = fixture.run(&argv, None);
    assert_eq!(status.code(), Some(23));
    assert!(!fixture.board_capture.exists());
}

// ---------------------------------------------------------------------------
// Pure Windows startup-script generation (checked on every platform)
// ---------------------------------------------------------------------------

#[test]
fn powershell_quote_doubles_every_single_quote_form_and_nothing_else() {
    assert_eq!(powershell_quote("plain"), "'plain'");
    assert_eq!(powershell_quote(""), "''");
    assert_eq!(powershell_quote("it's"), "'it''s'");
    assert_eq!(powershell_quote("''"), "''''''");
    // PowerShell reads U+2018..U+201B as single quotes as well.
    assert_eq!(
        powershell_quote("a\u{2018}b\u{2019}c\u{201a}d\u{201b}e"),
        "'a\u{2018}\u{2018}b\u{2019}\u{2019}c\u{201a}\u{201a}d\u{201b}\u{201b}e'"
    );
    // Nothing else is special inside a verbatim string.
    for literal in [
        "$HOME and $(Get-Date)",
        "back`tick `n",
        "double \"quotes\"",
        "line one\nline two\r\n",
        "spaces  and\ttabs",
        "unicode \u{2713} \u{fc} \u{1f600}",
        r"C:\path with\trailing\",
        "%PATH% ! & | ; < > ( ) { } @ #",
    ] {
        assert_eq!(powershell_quote(literal), format!("'{literal}'"));
    }
}

#[test]
fn configured_script_ps1_mirrors_the_sh_script_semantics() {
    let path = Path::new(r"C:\Temp\herdr-board-run-AbC123.ps1");
    let argv = vec![
        r"C:\Program Files\agent\agent.exe".to_string(),
        "it's".to_string(),
        "line one\nline two".to_string(),
        "$env:SECRET `n".to_string(),
    ];
    assert_eq!(
        configured_script_ps1(path, &argv),
        "\u{feff}# herdr-board configured-harness startup script (generated; removes itself).\n\
         Remove-Item -LiteralPath 'C:\\Temp\\herdr-board-run-AbC123.ps1' -Force -ErrorAction SilentlyContinue\n\
         $global:LASTEXITCODE = 0\n\
         $childStatus = 0\n\
         try {\n\
         \t& 'C:\\Program Files\\agent\\agent.exe' 'it''s' 'line one\nline two' '$env:SECRET `n'\n\
         \t$childStatus = $global:LASTEXITCODE\n\
         \tif ($null -eq $childStatus) { $childStatus = 0 }\n\
         } catch [System.Management.Automation.CommandNotFoundException] {\n\
         \t[Console]::Error.WriteLine($_.Exception.Message)\n\
         \t$childStatus = 127\n\
         } catch {\n\
         \t[Console]::Error.WriteLine($_.Exception.Message)\n\
         \t$childStatus = 1\n\
         }\n\
         if ($env:BOARD_BIN) {\n\
         \ttry { & $env:BOARD_BIN '__pane-exited' '--run-id' \"$env:BOARD_RUN_ID\" } catch { }\n\
         }\n\
         exit $childStatus\n"
    );
}

#[test]
fn windows_pane_command_double_quotes_the_script_for_every_pane_shell() {
    assert_eq!(
        windows_pane_command(
            "pwsh.exe",
            r"C:\Users\Jane Doe\AppData\Local\Temp\herdr-board-run-x1.ps1"
        )
        .unwrap(),
        r#"pwsh.exe -NoProfile -ExecutionPolicy Bypass -File "C:\Users\Jane Doe\AppData\Local\Temp\herdr-board-run-x1.ps1""#
    );
    assert_eq!(
        windows_pane_command("powershell.exe", r"C:\T\o'k (1) & ü.ps1").unwrap(),
        r#"powershell.exe -NoProfile -ExecutionPolicy Bypass -File "C:\T\o'k (1) & ü.ps1""#
    );
    // Characters some pane shell would expand or end the quotes on.
    for bad in ['"', '$', '`', '%', '!', '\n', '\r'] {
        let path = format!(r"C:\Temp\odd{bad}dir\herdr-board-run-x1.ps1");
        let err = windows_pane_command("pwsh.exe", &path).unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be quoted for every pane shell"),
            "{bad:?}: {err:#}"
        );
    }
}

#[test]
fn configured_script_sh_text_is_pinned() {
    // The unix generator is unchanged by Windows support; pin its exact text.
    let path = Path::new("/tmp/herdr-board-run-AbC123");
    let argv = vec![
        "/usr/bin/agent".to_string(),
        "it's".to_string(),
        "line one\nline two".to_string(),
    ];
    assert_eq!(
        configured_script(path, &argv),
        "#!/bin/sh\nrm -f -- '/tmp/herdr-board-run-AbC123'\n\
         '/usr/bin/agent' 'it'\\''s' 'line one\nline two' \n\
         child_status=$?\n\
         if [ -n \"${BOARD_BIN:-}\" ]; then\n\
         \t\"$BOARD_BIN\" __pane-exited --run-id \"$BOARD_RUN_ID\" || :\n\
         fi\nexit \"$child_status\"\n"
    );
}

#[test]
fn pwsh_batch_argv_problem_refuses_what_powershell_cannot_hand_a_batch_file() {
    // pwsh passes .cmd/.bat arguments with legacy quoting even in 7.3+, and
    // cmd.exe cuts an argument at a line break.
    let shim = Path::new(r"C:\npm\codex.cmd");
    let cases = [
        ("line one\nline two", "line break"),
        ("a\rb", "line break"),
        (r#"say "hi""#, "double quote"),
        ("", "empty"),
    ];
    for (arg, reason) in cases {
        let args = vec!["--flag".to_string(), arg.to_string()];
        let problem = pwsh_batch_argv_problem(shim, &args)
            .unwrap_or_else(|| panic!("{arg:?} cannot reach a .cmd"));
        assert!(problem.contains(reason), "{arg:?}: {problem}");
        assert!(problem.contains("argument 2"), "{arg:?}: {problem}");
        assert!(problem.contains("real executable"), "{arg:?}: {problem}");
    }
    assert!(pwsh_batch_argv_problem(Path::new(r"C:\x\RUN.Bat"), &[String::new()]).is_some());

    // Plain arguments to a batch file, and anything to a real executable.
    let plain = vec!["exec".to_string(), "it's & %VAR% (ok)".to_string()];
    assert!(pwsh_batch_argv_problem(shim, &plain).is_none());
    let odd = vec![String::new(), "a\"b\nc".to_string()];
    assert!(pwsh_batch_argv_problem(Path::new(r"C:\bin\codex.exe"), &odd).is_none());
    assert!(pwsh_batch_argv_problem(Path::new("/usr/bin/codex"), &odd).is_none());
}

#[cfg(windows)]
#[test]
fn configured_harness_refuses_a_batch_shim_argv_powershell_would_mangle() {
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p-batch"),
        "pane.rename" => pane_result(req, "w1:p-batch"),
        "pane.close" => error(req, "pane_not_found", "owned pane already closed"),
        method => panic!("unexpected configured batch-shim method {method}"),
    });
    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner = RecordingPaneRunner {
        calls: Arc::clone(&calls),
        behavior: Box::new(|_, _| panic!("a refused argv must never reach herdr pane run")),
    };
    let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));
    let cwd = tempfile::tempdir().unwrap();
    let shim = cwd.path().join("agent.cmd");
    std::fs::write(&shim, "@echo off\r\nexit /b 0\r\n").unwrap();

    let err = spawner
        .spawn(&custom_req(
            fake.socket.clone(),
            cwd.path().to_path_buf(),
            vec![
                shim.to_string_lossy().into_owned(),
                "task line one\ntask line two".into(),
            ],
        ))
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("batch file"), "{message}");
    assert!(message.contains("line break"), "{message}");
    assert!(calls.lock().unwrap().is_empty());
}
