use super::*;

#[test]
fn failed_managed_start_removes_prompt_file_and_closes_only_owned_pane() {
    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            *prompt_path2.lock().unwrap() = Some(path);
            error(req, "unsupported_agent_kind", "unsupported kind")
        }
        "pane.close" => pane_result(req, "w1:p2"),
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let err = spawner.spawn(&pi_req(None)).unwrap_err();
    assert!(err.to_string().contains("unsupported"));
    assert!(!prompt_path.lock().unwrap().as_ref().unwrap().exists());

    let requests = fake.requests.lock().unwrap();
    let closes: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "pane.close")
        .collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["params"], serde_json::json!({"pane_id": "w1:p2"}));
}

#[test]
fn vanished_owned_pane_after_agent_start_is_rediscovered_without_closing_target() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let split_calls = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let split_calls2 = Arc::clone(&split_calls);
    let starts2 = Arc::clone(&starts);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => reply(
            req,
            serde_json::json!({"type": "pane_list", "panes": [pane_info("w1:p1")]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                "focused_pane_id": "w1:p1",
                "panes": [{"pane_id": "w1:p1", "focused": true,
                    "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                "splits": []
            }}),
        ),
        "pane.split" => {
            let call = split_calls2.fetch_add(1, Ordering::SeqCst);
            pane_result(req, if call == 0 { "w1:p3" } else { "w1:p4" })
        }
        "agent.start" => {
            let call = starts2.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                assert_eq!(req["params"]["pane_id"], "w1:p3");
                error(req, "pane_not_found", "owned pane vanished before start")
            } else {
                assert_eq!(req["params"]["pane_id"], "w1:p4");
                agent_started(req, "w1:p4", false, true)
            }
        }
        "pane.close" => {
            assert_eq!(req["params"]["pane_id"], "w1:p3");
            error(req, "pane_not_found", "owned pane already vanished")
        }
        method => panic!("unexpected vanished-owned-pane method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p4"));
    assert_eq!(starts.load(Ordering::SeqCst), 2);

    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "pane.list",
            "pane.layout",
            "pane.split",
            "agent.start",
            "pane.close",
            "tab.list",
            "pane.list",
            "pane.layout",
            "pane.split",
            "agent.start"
        ],
        "cleanup and replacement allocation must preserve request ordering"
    );
    let closes: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "pane.close")
        .map(|r| r["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(closes, ["w1:p3"]);
    assert!(!closes.contains(&"w1:p1"));
}

#[test]
fn failed_managed_start_in_existing_tab_closes_only_new_split_pane() {
    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => reply(
            req,
            serde_json::json!({"type": "pane_list", "panes": [pane_info("w1:p1")]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                "focused_pane_id": "w1:p1",
                "panes": [{"pane_id": "w1:p1", "focused": true,
                    "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                "splits": []
            }}),
        ),
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "w1:p1");
            pane_result(req, "w1:p3")
        }
        "agent.start" => {
            assert_eq!(req["params"]["pane_id"], "w1:p3");
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            *prompt_path2.lock().unwrap() = Some(path);
            error(req, "unsupported_agent_kind", "start failed after split")
        }
        "pane.close" => {
            assert_eq!(
                req["params"],
                serde_json::json!({"pane_id": "w1:p3"}),
                "cleanup must never close the pre-existing user pane w1:p1",
            );
            pane_result(req, "w1:p3")
        }
        method => panic!("unexpected existing-tab cleanup method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let err = spawner.spawn(&pi_req(None)).unwrap_err();
    assert!(err.to_string().contains("start failed after split"));
    let path = prompt_path.lock().unwrap().clone().unwrap();
    assert!(
        !path.exists(),
        "failed start must remove its authoritative system-prompt file",
    );

    let requests = fake.requests.lock().unwrap();
    let closed: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "pane.close")
        .map(|r| r["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(closed, ["w1:p3"]);
    assert!(!closed.contains(&"w1:p1"));
}

#[test]
fn configured_pane_run_failure_removes_script_and_closes_owned_pane() {
    use std::os::unix::fs::PermissionsExt;

    let selected = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p9"),
        "pane.rename" => pane_result(req, "w1:p9"),
        "pane.close" => pane_result(req, "w1:p9"),
        method => panic!("unexpected configured-runner method {method}"),
    });
    let default =
        serve_recording_herdr(|req, _| panic!("request incorrectly used default socket: {req}"));
    let cwd = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
    let runner_path = Arc::new(Mutex::new(None::<PathBuf>));
    let runner_path2 = Arc::clone(&runner_path);
    let selected_socket = selected.socket.clone();
    let runner = RecordingPaneRunner {
        calls: Arc::clone(&calls),
        behavior: Box::new(move |socket, argv| {
            assert_eq!(socket, selected_socket.as_path());
            assert_eq!(&argv[..3], ["pane", "run", "w1:p9"]);
            assert_eq!(argv.len(), 4);
            let path = PathBuf::from(&argv[3]);
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o700,
            );
            *runner_path2.lock().unwrap() = Some(path);
            anyhow::bail!("herdr pane run failed on selected session")
        }),
    };
    let spawner = HerdrSpawner::with_pane_runner(default.socket.clone(), Arc::new(runner));
    let argv = vec![
        "/bin/printf".into(),
        "single'quote".into(),
        "space value".into(),
        "line one\nline two".into(),
    ];

    let err = spawner
        .spawn(&custom_req(
            selected.socket.clone(),
            cwd.path().to_path_buf(),
            argv,
        ))
        .unwrap_err();
    assert!(err.to_string().contains("pane run failed"));
    let path = runner_path.lock().unwrap().clone().unwrap();
    assert!(
        !path.exists(),
        "daemon must remove an unexecuted script after CLI failure",
    );
    let call = calls.lock().unwrap().clone();
    assert_eq!(call.len(), 1);
    assert_eq!(call[0].socket, selected.socket);
    assert_eq!(&call[0].argv[..3], ["pane", "run", "w1:p9"]);
    assert_eq!(call[0].argv[3], path.to_string_lossy());

    let requests = selected.requests.lock().unwrap();
    let closes: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "pane.close")
        .collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["params"], serde_json::json!({"pane_id": "w1:p9"}));
    assert!(default.requests.lock().unwrap().is_empty());
}
