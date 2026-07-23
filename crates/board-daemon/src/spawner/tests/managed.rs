use super::*;

#[test]
fn herdr_protocol_gate_rejects_mismatches_before_any_spawn_or_placement_call() {
    for (version, protocol) in [("0.7.4", 17), ("0.7.5", 16)] {
        let fake = serve_recording_herdr_with_ping(
            |req, _| error(req, "unexpected_call", "protocol gate was bypassed"),
            version,
            protocol,
        );
        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner = RecordingPaneRunner {
            calls: Arc::clone(&calls),
            behavior: Box::new(|_, _| anyhow::bail!("runner must not be called")),
        };
        let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));

        let err = spawner
            .spawn(&custom_req(
                fake.socket.clone(),
                PathBuf::from("/tmp/card cwd"),
                vec!["custom-agent".into()],
            ))
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("Herdr 0.7.5 with protocol 17 is required"),
            "mismatch must explain the required Herdr version/protocol: {text}"
        );
        assert_eq!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ping"],
            "protocol mismatch must stop before tab.list/tab.create/pane.split"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "protocol mismatch must stop before pane runner"
        );
    }
}

#[test]
fn managed_pi_uses_startup_only_system_file_then_polls_ready_before_card_prompt() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let gets = Arc::new(AtomicUsize::new(0));
    let gets2 = Arc::clone(&gets);
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
            agent_started(req, "w1:p2", true, false)
        }
        "agent.get" => {
            let call = gets2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req["params"], serde_json::json!({"target": "w1:p2"}));
            if call == 0 {
                agent_get_result(req, "w1:p2", "card-42-execute", true, false)
            } else {
                agent_get_result(req, "w1:p2", "card-42-execute", false, true)
            }
        }
        "agent.prompt" => {
            assert_eq!(
                gets2.load(Ordering::SeqCst),
                2,
                "agent.prompt must not be sent while agent.get is still pending",
            );
            assert_eq!(
                req["params"],
                serde_json::json!({
                    "target": "w1:p2",
                    "text": "first task line\nsecond task line with spaces"
                }),
                "only the initial/card prompt belongs in agent.prompt",
            );
            agent_prompted(req, "w1:p2", "card-42-execute")
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let prompt = "first task line\nsecond task line with spaces";

    let handle = spawner.spawn(&pi_req(Some(prompt))).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p2"));
    let path = prompt_path.lock().unwrap().clone().unwrap();
    assert!(
        !path.exists(),
        "the 0600 system-prompt file must be removed before spawn returns"
    );

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
            "tab.create",
            "agent.start",
            "agent.get",
            "agent.get",
            "agent.prompt"
        ],
        "schema-valid readiness polling must precede prompt submission",
    );
    assert_eq!(
        requests[2]["params"],
        serde_json::json!({
            "workspace_id": "w1", "label": "kanban", "cwd": "/tmp/card cwd",
            "env": {"BOARD_CARD_ID": "42"}, "focus": false
        })
    );
    assert_eq!(requests[3]["params"]["name"], "card-42-execute");
    assert_eq!(requests[3]["params"]["kind"], "pi");
    assert_eq!(requests[3]["params"]["pane_id"], "w1:p2");
    assert_eq!(requests[3]["params"]["timeout_ms"], 30000);
}

#[test]
fn managed_claude_uses_file_specific_flag_after_unchanged_startup_tail() {
    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p8"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--effort",
                    "low",
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    "Bash(board:*)",
                    "--resume",
                    "source-session",
                    "--fork-session",
                ],
                "--append-system-prompt-file",
                "claude system instructions",
            );
            *prompt_path2.lock().unwrap() = Some(path);
            agent_started(req, "w1:p8", false, true)
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&claude_req()).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p8"));
    assert!(!prompt_path.lock().unwrap().as_ref().unwrap().exists());
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests[3]["params"]["kind"], "claude");
    assert!(requests.iter().all(|r| r["method"] != "agent.prompt"));
}

#[test]
fn managed_existing_tab_splits_selected_pane_before_exact_agent_start() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
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
        "pane.split" => pane_result(req, "w1:p3"),
        "agent.start" => {
            assert_startup_prompt_file(
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
            agent_started(req, "w1:p3", false, true)
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p3"));

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
            "agent.start"
        ]
    );
    assert_eq!(requests[4]["params"]["target_pane_id"], "w1:p1");
    assert_eq!(requests[4]["params"]["direction"], "right");
    assert_eq!(requests[4]["params"]["cwd"], "/tmp/card cwd");
    assert_eq!(
        requests[4]["params"]["env"],
        serde_json::json!({"BOARD_CARD_ID": "42"}),
        "split placement must establish the requested child environment",
    );
    assert_eq!(requests[5]["params"]["pane_id"], "w1:p3");
    assert!(!methods.contains(&"pane.focus"));
}

#[test]
fn pane_split_race_rediscovers_tab_and_splits_a_live_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let split_calls = Arc::new(AtomicUsize::new(0));
    let split_calls2 = Arc::clone(&split_calls);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => {
            let pane = if split_calls2.load(Ordering::SeqCst) == 0 {
                "w1:p1"
            } else {
                "w1:p4"
            };
            reply(
                req,
                serde_json::json!({"type": "pane_list", "panes": [pane_info(pane)]}),
            )
        }
        "pane.layout" => {
            let pane = if split_calls2.load(Ordering::SeqCst) == 0 {
                "w1:p1"
            } else {
                "w1:p4"
            };
            reply(
                req,
                serde_json::json!({"type": "pane_layout", "layout": {
                    "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                    "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                    "focused_pane_id": pane,
                    "panes": [{"pane_id": pane, "focused": true,
                        "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                    "splits": []
                }}),
            )
        }
        "pane.split" => {
            let call = split_calls2.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                error(req, "pane_not_found", "selected pane raced away")
            } else {
                assert_eq!(req["params"]["target_pane_id"], "w1:p4");
                pane_result(req, "w1:p5")
            }
        }
        "agent.start" => agent_started(req, "w1:p5", false, true),
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p5"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.list")
            .count(),
        2,
        "a pane.split race must restart tab discovery",
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.split")
            .count(),
        2
    );
}

#[test]
fn listed_tab_disappearing_during_pane_discovery_creates_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pane_lists = Arc::new(AtomicUsize::new(0));
    let pane_lists2 = Arc::clone(&pane_lists);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            if pane_lists2.load(Ordering::SeqCst) == 0 {
                existing_tab_list(req)
            } else {
                empty_tab_list(req)
            }
        }
        "pane.list" => {
            pane_lists2.fetch_add(1, Ordering::SeqCst);
            error(req, "pane_not_found", "listed tab disappeared")
        }
        "tab.create" => tab_created(req, "w1:p6"),
        "agent.start" => {
            assert_eq!(req["params"]["pane_id"], "w1:p6");
            agent_started(req, "w1:p6", false, true)
        }
        method => panic!("unexpected tab-discovery race method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p6"));

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
            "tab.list",
            "tab.create",
            "agent.start"
        ],
        "a vanished listed tab must trigger bounded full rediscovery"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        1
    );
}

#[test]
fn name_collision_retries_on_the_same_owned_pane_and_same_prompt_file() {
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
    let fake = serve_recording_herdr(move |req, index| match req["method"].as_str().unwrap() {
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
            prompt_paths2.lock().unwrap().push(path);
            if index == 2 {
                error(req, "agent_name_taken", "primary name is already used")
            } else {
                agent_started(req, "w1:p2", false, true)
            }
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    spawner.spawn(&pi_req(None)).unwrap();

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["params"]["name"], "card-42-execute");
    assert_eq!(starts[1]["params"]["name"], "card-42-execute-r7");
    assert_eq!(starts[0]["params"]["pane_id"], "w1:p2");
    assert_eq!(starts[1]["params"]["pane_id"], "w1:p2");
    let paths = prompt_paths.lock().unwrap();
    assert_eq!(paths[0], paths[1]);
    assert!(!paths[0].exists());
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        1,
        "fallback owns and reuses the pane already created by the board",
    );
}

#[test]
fn empty_existing_tab_rediscovers_and_launches_in_replacement_tab() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pane_lists = Arc::new(AtomicUsize::new(0));
    let pane_lists2 = Arc::clone(&pane_lists);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            if pane_lists2.load(Ordering::SeqCst) == 0 {
                existing_tab_list(req)
            } else {
                empty_tab_list(req)
            }
        }
        "pane.list" => {
            pane_lists2.fetch_add(1, Ordering::SeqCst);
            reply(req, serde_json::json!({"type": "pane_list", "panes": []}))
        }
        "tab.create" => tab_created(req, "w1:p-race-replacement"),
        "agent.start" => agent_started(req, "w1:p-race-replacement", false, true),
        method => panic!("unexpected empty-tab race method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-race-replacement"));
    let methods: Vec<_> = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["method"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "pane.list",
            "tab.list",
            "tab.create",
            "agent.start"
        ],
        "an existing tab that empties during discovery must trigger bounded rediscovery"
    );
}
