//! Supported-contract managed launch: the version/protocol gate, the pane-first
//! `pane.split` → `agent.start` order, the authoritative startup system-prompt
//! file, readiness polling before the card prompt, and the bounded
//! `agent_pane_busy` / name-collision retry budget.

use std::time::Duration;

use super::*;
use serde_json::json;

#[test]
fn herdr_protocol_gate_rejects_mismatches_before_any_spawn_or_placement_call() {
    for (version, protocol) in [
        ("0.8.1", board_herdr::SUPPORTED_HERDR_PROTOCOL),
        (
            board_herdr::SUPPORTED_HERDR_VERSION,
            board_herdr::SUPPORTED_HERDR_PROTOCOL - 1,
        ),
    ] {
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
            text.contains(&format!(
                "Herdr {} with protocol {} is required",
                board_herdr::SUPPORTED_HERDR_VERSION,
                board_herdr::SUPPORTED_HERDR_PROTOCOL
            )),
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
        method => panic!("unexpected supported-contract method {method}"),
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
        method => panic!("unexpected supported-contract method {method}"),
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
fn managed_fresh_launch_closes_the_anchor_leaving_only_the_harness_pane() {
    // A fresh managed launch in a card tab ends anchorless: after a successful
    // `agent.start` (and prompt), the anchor pane is closed so the tab holds
    // exactly the harness pane. The handle therefore persists anchor_pane_id
    // as None.
    let closed = Arc::new(Mutex::new(Vec::<String>::new()));
    let closed_for_server = Arc::clone(&closed);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":2
            }]}),
        ),
        "pane.list" => reply(
            req,
            json!({"type":"pane_list","panes":[
                pane_info("w1:p-anchor"),
                pane_info("w1:p-prior")
            ]}),
        ),
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},
                "focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => pane_result(req, "w1:p-fresh"),
        "agent.start" => agent_started(req, "w1:p-fresh", false, true),
        "pane.close" => {
            closed_for_server
                .lock()
                .unwrap()
                .push(req["params"]["pane_id"].as_str().unwrap().to_string());
            pane_result(req, req["params"]["pane_id"].as_str().unwrap())
        }
        method => panic!("unexpected managed-close method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = None;

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-fresh"));
    assert_eq!(
        handle.anchor_pane_id.as_deref(),
        None,
        "a successful fresh managed launch must not persist a closed anchor"
    );
    assert_eq!(
        *closed.lock().unwrap(),
        vec!["w1:p-prior", "w1:p-anchor"],
        "the ended child is reclaimed and the anchor is closed after launch"
    );
    assert!(
        !closed.lock().unwrap().contains(&"w1:p-fresh".to_string()),
        "the harness pane itself must survive"
    );
    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    let last = methods.last().copied().unwrap();
    assert_eq!(last, "pane.close");
    assert_eq!(
        requests[requests.len() - 1]["params"]["pane_id"],
        "w1:p-anchor"
    );
}

#[test]
fn managed_fresh_recovery_closes_the_temporary_anchor_leaving_one_harness_pane() {
    // Later fresh managed run in an anchorless tab: the temporary anchor is
    // recreated from the exact durable prior child, the new child is split and
    // launched, then the temporary anchor is closed and the prior ended child
    // is reclaimed — one harness pane remains.
    let splits = Arc::new(Mutex::new(Vec::<String>::new()));
    let splits_for_server = Arc::clone(&splits);
    let closed = Arc::new(Mutex::new(Vec::<String>::new()));
    let closed_for_server = Arc::clone(&closed);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":1
            }]}),
        ),
        "pane.list" => reply(
            req,
            json!({"type":"pane_list","panes":[{
                "pane_id":"w1:p-prior","terminal_id":"term-prior","workspace_id":"w1",
                "tab_id":"w1:t1","label":"card-42-execute","agent":null,
                "agent_status":"idle","focused":false,"revision":2
            }]}),
        ),
        "pane.layout" => {
            let target = req["params"]["pane_id"].as_str().unwrap().to_string();
            let (width, height) = if target == "w1:p-prior" {
                (240, 40)
            } else {
                (100, 40)
            };
            reply(
                req,
                json!({"type":"pane_layout","layout":{
                    "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                    "area":{"x":0,"y":0,"width":width,"height":height},
                    "focused_pane_id":target,
                    "panes":[{"pane_id":target,"focused":true,
                        "rect":{"x":0,"y":0,"width":width,"height":height}}],"splits":[]
                }}),
            )
        }
        "pane.split" => {
            let target = req["params"]["target_pane_id"]
                .as_str()
                .unwrap()
                .to_string();
            let mut splits = splits_for_server.lock().unwrap();
            splits.push(target.clone());
            let child = if target == "w1:p-prior" {
                "w1:p-temp-anchor"
            } else {
                "w1:p-new-child"
            };
            pane_result(req, child)
        }
        "pane.rename" => pane_result(req, "w1:p-temp-anchor"),
        "agent.start" => agent_started(req, "w1:p-new-child", false, true),
        "pane.close" => {
            let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
            closed_for_server.lock().unwrap().push(pane_id.clone());
            pane_result(req, &pane_id)
        }
        method => panic!("unexpected managed-recovery method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = None;

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-new-child"));
    assert_eq!(handle.anchor_pane_id.as_deref(), None);
    assert_eq!(
        *splits.lock().unwrap(),
        vec!["w1:p-prior".to_string(), "w1:p-temp-anchor".to_string()],
        "recovery splits the temporary anchor from the durable child, then the child"
    );
    assert_eq!(
        *closed.lock().unwrap(),
        vec!["w1:p-prior".to_string(), "w1:p-temp-anchor".to_string()],
        "the prior ended child is reclaimed and the temporary anchor is closed"
    );
    assert!(
        !closed
            .lock()
            .unwrap()
            .contains(&"w1:p-new-child".to_string()),
        "the new harness pane must survive"
    );
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
        method => panic!("unexpected supported-contract method {method}"),
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
fn managed_busy_retry_preserves_exact_start_on_one_new_split_pane() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let starts = Arc::new(AtomicUsize::new(0));
    let starts2 = Arc::clone(&starts);
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
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
            if starts2.fetch_add(1, Ordering::SeqCst) == 0 {
                error(req, "agent_pane_busy", "pane is still busy")
            } else {
                agent_started(req, "w1:p3", false, true)
            }
        }
        method => panic!("unexpected busy-retry method {method}"),
    });
    let delays = Arc::new(Mutex::new(Vec::new()));
    let delays2 = Arc::clone(&delays);
    let spawner = HerdrSpawner::with_pane_runner_and_delay(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| unreachable!("managed launch must not use pane runner")),
        }),
        Arc::new(move |delay| delays2.lock().unwrap().push(delay)),
    );

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p3"));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(
        delays.lock().unwrap().as_slice(),
        &[super::super::AGENT_START_BUSY_BACKOFF]
    );

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["params"]["pane_id"], "w1:p3");
    assert_eq!(starts[1]["params"]["pane_id"], "w1:p3");
    assert_eq!(starts[0]["params"]["name"], "card-42-execute");
    assert_eq!(starts[1]["params"]["name"], "card-42-execute");
    assert_eq!(starts[0]["params"], starts[1]["params"]);
    let prompt_paths = prompt_paths.lock().unwrap();
    assert_eq!(prompt_paths[0], prompt_paths[1]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1,
        "busy must retry on the owned pane instead of splitting again",
    );
}

#[test]
fn managed_composed_busy_then_name_taken_has_one_global_busy_budget() {
    assert_composed_busy_name_sequence(
        &["busy", "name_taken", "busy", "busy"],
        &[
            "card-42-execute",
            "card-42-execute",
            "card-42-execute-r7",
            "card-42-execute-r7",
        ],
    );
}

#[test]
fn managed_composed_name_taken_then_busy_has_one_global_busy_budget() {
    assert_composed_busy_name_sequence(
        &["name_taken", "busy", "busy", "busy"],
        &[
            "card-42-execute",
            "card-42-execute-r7",
            "card-42-execute-r7",
            "card-42-execute-r7",
        ],
    );
}

// ---------------------------------------------------------------------------
// Codex (C5 capture + C7 no-file prompt semantics): the self-minting harness
// has no system-prompt file and no prompt in startup argv; after readiness the
// daemon bounded-polls `agent.get.agent_session` on the gated connection,
// validates `{agent: codex, kind: id, value non-empty}`, and delivers the
// prompt — Mint gets one delimited system+task block, resume/fork the task
// alone, reuse the task alone, rescue nothing. The captured thread id rides
// the handle for atomic promotion.
// ---------------------------------------------------------------------------

/// An `agent.get` reply carrying a protocol-19 `AgentSessionInfo`.
/// A reused codex pane that is already interactive and quiescent (mirror of
/// `pane_reuse::reuse_agent_ready`, kept local so this module owns its fixture).
fn codex_reuse_agent_ready(req: &Value, pane_id: &str, status: &str) -> Value {
    reply(
        req,
        json!({"type":"agent_info","agent":{
            "pane_id": pane_id, "agent": "codex", "agent_status": status,
            "interactive_ready": true, "launch_pending": false,
            "focused": false, "revision": 2
        }}),
    )
}

/// An `agent.get` reply carrying a protocol-19 `AgentSessionInfo`.
fn agent_get_with_session(req: &Value, pane_id: &str, session: Value) -> Value {
    reply(
        req,
        json!({
            "type": "agent_info",
            "agent": {
                "pane_id": pane_id,
                "agent": "codex",
                "agent_status": "idle",
                "interactive_ready": true,
                "launch_pending": false,
                "focused": false,
                "revision": 2,
                "agent_session": session
            }
        }),
    )
}

fn codex_session(thread_id: &str) -> Value {
    json!({"agent": "codex", "kind": "id", "source": "session", "value": thread_id})
}

/// The exact startup tail `board_core::harness::codex` produces for
/// `codex_req`: no prompt file flag, no `--`, no task text.
const CODEX_STARTUP_TAIL: &[&str] = &["--model", "gpt-5.6", "-c", "model_reasoning_effort=low"];

#[test]
fn managed_codex_mint_has_no_prompt_file_captures_session_then_prompts_delimited_block() {
    use board_core::harness::codex::{mint_prompt, MINT_SYSTEM_DELIMITER, MINT_TASK_DELIMITER};

    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let prompts_for_server = Arc::clone(&prompts);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => {
            // C7: no system-prompt file and no argv prompt for codex.
            let args: Vec<&str> = req["params"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(
                args, CODEX_STARTUP_TAIL,
                "codex startup argv must be exactly the startup tail"
            );
            assert_eq!(req["params"]["kind"], "codex");
            agent_started(req, "w1:p2", false, true)
        }
        "agent.get" => {
            assert_eq!(req["params"], json!({"target": "w1:p2"}));
            agent_get_with_session(req, "w1:p2", codex_session("thread-1"))
        }
        "agent.prompt" => {
            prompts_for_server
                .lock()
                .unwrap()
                .push(req["params"]["text"].as_str().unwrap().to_string());
            agent_prompted(req, "w1:p2", "card-42-execute")
        }
        method => panic!("unexpected codex-mint method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner
        .spawn(&codex_req(&[], Some("build the widget")))
        .unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p2"));
    assert_eq!(
        handle.captured_session_id.as_deref(),
        Some("thread-1"),
        "the captured thread id must ride the handle for atomic promotion"
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
            "agent.prompt"
        ],
        "capture polls agent.get once and only then delivers the prompt"
    );
    let prompts = prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(
        prompts[0],
        mint_prompt("codex system instructions", "build the widget"),
        "Mint receives ONE clearly delimited block: system instructions first, then the task"
    );
    let block = &prompts[0];
    assert!(block.starts_with(MINT_SYSTEM_DELIMITER));
    let task_pos = block.find(MINT_TASK_DELIMITER).unwrap();
    assert!(block[..task_pos].contains("codex system instructions"));
    assert!(block[task_pos..].contains("build the widget"));
}

#[test]
fn managed_codex_resume_and_fork_prompt_task_only_with_the_real_thread_id() {
    // Resume: the prompt is the task alone (the conversation already has the
    // system instructions), and the reported session id is the real thread.
    for (tail, thread) in [
        (&["resume", "thread-7"][..], "thread-7"),
        (&["fork", "thread-7"][..], "thread-7"),
    ] {
        let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
        let prompts_for_server = Arc::clone(&prompts);
        let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => tab_created(req, "w1:p2"),
            "agent.start" => {
                let args: Vec<&str> = req["params"]["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                let mut expected = CODEX_STARTUP_TAIL.to_vec();
                expected.extend_from_slice(tail);
                assert_eq!(args, expected, "the session subcommand closes the argv");
                agent_started(req, "w1:p2", false, true)
            }
            "agent.get" => agent_get_with_session(req, "w1:p2", codex_session(thread)),
            "agent.prompt" => {
                prompts_for_server
                    .lock()
                    .unwrap()
                    .push(req["params"]["text"].as_str().unwrap().to_string());
                agent_prompted(req, "w1:p2", "card-42-execute")
            }
            method => panic!("unexpected codex-{tail:?} method {method}"),
        });
        let spawner = HerdrSpawner::new(fake.socket.clone());
        let handle = spawner
            .spawn(&codex_req(tail, Some("next stage task")))
            .unwrap();
        assert_eq!(handle.captured_session_id.as_deref(), Some(thread));
        let prompts = prompts.lock().unwrap();
        assert_eq!(
            prompts.as_slice(),
            &["next stage task".to_string()],
            "resume/fork receive the task alone — never a system block"
        );
    }
}

#[test]
fn managed_codex_reuse_prompts_task_only_and_does_not_capture() {
    let gets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gets_for_server = Arc::clone(&gets);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":1
            }]}),
        ),
        "pane.list" => {
            let mut prior = pane_info("w1:p-prior");
            prior["label"] = json!("card-42-setup");
            prior["agent"] = json!("codex");
            prior["agent_status"] = json!("done");
            reply(req, json!({"type":"pane_list","panes":[prior]}))
        }
        "agent.get" => {
            gets_for_server.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            codex_reuse_agent_ready(req, "w1:p-prior", "done")
        }
        "agent.prompt" => {
            assert_eq!(req["params"]["text"], "next stage task");
            agent_prompted(req, "w1:p-prior", "card-42-execute")
        }
        method => panic!("unexpected codex-reuse method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = codex_req(&["resume", "thread-7"], Some("next stage task"));
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = Some("w1:p-prior".into());

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-prior"));
    assert_eq!(
        handle.captured_session_id, None,
        "same-pane reuse re-prompts the live conversation; there is nothing to capture"
    );
    assert_eq!(
        gets.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "reuse does exactly one agent.get (quiescence) and no capture poll"
    );
}

#[test]
fn managed_codex_absent_session_degrades_within_bounds_and_launch_succeeds() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let gets = Arc::new(AtomicUsize::new(0));
    let gets_for_server = Arc::clone(&gets);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => {
            assert_eq!(
                req["params"]["args"].as_array().unwrap().len(),
                CODEX_STARTUP_TAIL.len(),
                "no prompt file flag may be appended when no session is reported"
            );
            agent_started(req, "w1:p2", false, true)
        }
        "agent.get" => {
            gets_for_server.fetch_add(1, Ordering::SeqCst);
            agent_get_result(req, "w1:p2", "card-42-execute", false, true)
        }
        "agent.prompt" => agent_prompted(req, "w1:p2", "card-42-execute"),
        method => panic!("unexpected codex-absent method {method}"),
    });
    // Zero-delay clock: the bounded capture backoff never hits the wall.
    let spawner = HerdrSpawner::with_pane_runner_and_delay(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| unreachable!("managed launch must not use pane runner")),
        }),
        Arc::new(|_: Duration| {}),
    );

    let handle = spawner
        .spawn(&codex_req(&[], Some("build the widget")))
        .unwrap();
    assert_eq!(
        handle.captured_session_id, None,
        "an absent thread report degrades to None"
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        super::super::SESSION_CAPTURE_PROBES,
        "the capture is bounded: exactly the probe budget, never an unbounded poll"
    );
    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods.last().copied(),
        Some("agent.prompt"),
        "basic launch stays successful without a reported session"
    );
}

#[test]
fn managed_codex_mismatched_session_report_degrades_immediately() {
    // Wrong owner agent: the pane's session belongs to another agent.
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => agent_started(req, "w1:p2", false, true),
        "agent.get" => agent_get_with_session(
            req,
            "w1:p2",
            json!({"agent": "pi", "kind": "id", "source": "session", "value": "thread-x"}),
        ),
        "agent.prompt" => agent_prompted(req, "w1:p2", "card-42-execute"),
        method => panic!("unexpected codex-wrong-agent method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let handle = spawner.spawn(&codex_req(&[], Some("task"))).unwrap();
    assert_eq!(
        handle.captured_session_id, None,
        "a session owned by a different agent must not be captured as the codex thread id"
    );

    // `path` kind: a filesystem reference is not a resumable conversation id.
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => agent_started(req, "w1:p2", false, true),
        "agent.get" => agent_get_with_session(
            req,
            "w1:p2",
            json!({"agent": "codex", "kind": "path", "source": "pane", "value": "/tmp/s.json"}),
        ),
        "agent.prompt" => agent_prompted(req, "w1:p2", "card-42-execute"),
        method => panic!("unexpected codex-path-kind method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let handle = spawner.spawn(&codex_req(&[], Some("task"))).unwrap();
    assert_eq!(
        handle.captured_session_id, None,
        "a path-kind reference is not a conversation id"
    );

    // Blank value: nothing to resume against.
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => agent_started(req, "w1:p2", false, true),
        "agent.get" => agent_get_with_session(
            req,
            "w1:p2",
            json!({"agent": "codex", "kind": "id", "source": "session", "value": "  "}),
        ),
        "agent.prompt" => agent_prompted(req, "w1:p2", "card-42-execute"),
        method => panic!("unexpected codex-blank-value method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let handle = spawner.spawn(&codex_req(&[], Some("task"))).unwrap();
    assert_eq!(handle.captured_session_id, None, "a blank id must degrade");
}

#[test]
fn managed_codex_rescue_shaped_launch_captures_but_never_prompts() {
    // Rescue shape: `resume <id>` argv with initial_prompt cleared by
    // `resume_invocation`. The capture still runs (the id is re-confirmed),
    // but NO agent.prompt is sent — re-sending the task would re-run it.
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => agent_started(req, "w1:p2", false, true),
        "agent.get" => agent_get_with_session(req, "w1:p2", codex_session("thread-9")),
        method => panic!("unexpected codex-rescue method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner
        .spawn(&codex_req(&["resume", "thread-9"], None))
        .unwrap();
    assert_eq!(handle.captured_session_id.as_deref(), Some("thread-9"));
    let requests = fake.requests.lock().unwrap();
    assert!(
        requests.iter().all(|r| r["method"] != "agent.prompt"),
        "a rescue-shaped launch must never re-send the card task"
    );
}
