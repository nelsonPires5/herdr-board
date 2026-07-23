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

    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let dir = tempfile::tempdir().unwrap();
    let recorder = dir.path().join("herdr-bin-recorder.sh");
    let invocation = dir.path().join("invocation");
    let socket = dir.path().join("selected.sock");
    let empty_path = dir.path().join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    std::fs::write(
            &recorder,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf '%s' \"${{HERDR_SOCKET_PATH:-}}\" > {}\n",
                posix_quote(&invocation.to_string_lossy()),
                posix_quote(&dir.path().join("socket").to_string_lossy()),
            ),
        )
        .unwrap();
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();

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

    let args = std::fs::read(&invocation)
        .unwrap()
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(args, ["pane", "run", "w1:p-bin"]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("socket")).unwrap(),
        socket.to_string_lossy()
    );
}

#[test]
fn configured_harness_uses_selected_socket_pane_run_with_exact_payload() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let command_dir = tempfile::tempdir().unwrap();
    let cwd = command_dir.path().join("custom cwd with spaces");
    std::fs::create_dir(&cwd).unwrap();
    let recorder = command_dir.path().join("custom command's recorder.py");
    let capture = command_dir.path().join("captured invocation.json");
    std::fs::write(
            &recorder,
            format!(
                "#!/usr/bin/env python3\nimport json, os, sys\nkeys = ['BOARD_PROMPT', 'BOARD_SYSTEM_PROMPT', 'HERDR_SOCKET_PATH']\njson.dump({{'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': {{k: os.environ[k] for k in keys}}}}, open({:?}, 'w'))\n",
                capture
            ),
        )
        .unwrap();
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let exact_argv = vec![
        recorder.to_string_lossy().into_owned(),
        "single'quote".into(),
        "literal argument with spaces".into(),
        "line one\nline two".into(),
    ];

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
                "script path must be one shell-free argv item"
            );
            let path = PathBuf::from(&argv[3]);
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o700,
                "startup script must be executable only by its owner",
            );
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
    assert_eq!(
        call,
        [PaneRunCall {
            socket: selected.socket.clone(),
            argv: vec![
                "pane".into(),
                "run".into(),
                "w1:p9".into(),
                path.to_string_lossy().into_owned(),
            ],
        }],
        "exactly one CLI call must target the selected session and transport one script path",
    );
    assert!(
        path.exists(),
        "runner success must return before the pane opens the startup script"
    );

    // Simulate the selected pane opening the script after pane.run has
    // returned, including the pane's cwd and environment.
    let status = Command::new(&path)
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
    assert_eq!(recorded["argv"], serde_json::json!(exact_argv[1..]));
    let cwd_canonical = std::fs::canonicalize(&cwd).unwrap();
    assert_eq!(recorded["cwd"], cwd_canonical.to_string_lossy().as_ref());
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
    let recorded_path = PathBuf::from(&calls.lock().unwrap()[0].argv[3]);
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
