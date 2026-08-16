use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::testkit::{
    self, agent_info, agent_started, error, pane_info, reply, tab_created, FakeHerdr,
};

use super::herdr::{
    configured_script, posix_quote, remove_file_if_exists, HerdrCliPaneRunner, PaneRunner,
};
use super::local::materialize_local_argv;
use super::placement::grid_slot;
use super::{HerdrLaunchPlan, HerdrSpawner, Spawner, WorkspaceBootstrapHint};

use board_herdr::{LayoutPane, Rect, SplitDirection};
use serde_json::Value;

fn pane(id: &str, width: u64, height: u64) -> LayoutPane {
    LayoutPane {
        pane_id: id.to_string(),
        focused: false,
        rect: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
    }
}

// -----------------------------------------------------------------------
// Pane-first managed launch contracts for the supported Herdr API
// -----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneRunCall {
    socket: PathBuf,
    argv: Vec<String>,
}

type PaneRunBehavior = dyn Fn(&Path, &[String]) -> anyhow::Result<()> + Send + Sync;

struct RecordingPaneRunner {
    calls: Arc<Mutex<Vec<PaneRunCall>>>,
    behavior: Box<PaneRunBehavior>,
}

impl PaneRunner for RecordingPaneRunner {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(PaneRunCall {
            socket: socket.to_path_buf(),
            argv: argv.to_vec(),
        });
        (self.behavior)(socket, argv)
    }
}

impl Drop for RecordingPaneRunner {
    fn drop(&mut self) {
        let paths = self
            .calls
            .lock()
            .ok()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|call| {
                        call.argv.len() == 4 && call.argv[0] == "pane" && call.argv[1] == "run"
                    })
                    .map(|call| PathBuf::from(&call.argv[3]))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for path in paths {
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_name.starts_with("herdr-board-run-") {
                continue;
            }
            let Ok(script) = std::fs::read_to_string(&path) else {
                // A successfully opened startup script removes itself
                // before running the child, so absence is expected.
                continue;
            };
            let expected_header = format!(
                "#!/bin/sh\nrm -f -- {}\n",
                posix_quote(&path.to_string_lossy())
            );
            if script.starts_with(&expected_header) {
                let _ = remove_file_if_exists(&path);
            }
        }
    }
}

fn serve_recording_herdr<F>(handler: F) -> FakeHerdr
where
    F: Fn(&Value, usize) -> Value + Send + Sync + 'static,
{
    testkit::herdr_server().handler(handler).serve()
}

fn serve_recording_herdr_with_ping<F>(handler: F, version: &str, protocol: u32) -> FakeHerdr
where
    F: Fn(&Value, usize) -> Value + Send + Sync + 'static,
{
    testkit::herdr_server()
        .version(version)
        .protocol(protocol)
        .handler(handler)
        .serve()
}

fn empty_tab_list(req: &Value) -> Value {
    reply(req, serde_json::json!({"type": "tab_list", "tabs": []}))
}

fn existing_tab_list(req: &Value) -> Value {
    reply(
        req,
        serde_json::json!({"type": "tab_list", "tabs": [{
            "tab_id": "w1:t1", "workspace_id": "w1", "number": 1,
            "label": "kanban", "focused": true, "pane_count": 1,
            "agent_status": "idle"
        }]}),
    )
}

fn pane_result(req: &Value, pane_id: &str) -> Value {
    reply(
        req,
        serde_json::json!({"type": "pane_info", "pane": pane_info(pane_id)}),
    )
}

fn agent_get_result(req: &Value, pane_id: &str, name: &str, pending: bool, ready: bool) -> Value {
    reply(
        req,
        serde_json::json!({
            "type": "agent_info",
            "agent": agent_info(pane_id, name, pending, ready)
        }),
    )
}

fn agent_prompted(req: &Value, pane_id: &str, name: &str) -> Value {
    reply(
        req,
        serde_json::json!({
            "type": "agent_prompted",
            "agent": agent_info(pane_id, name, false, true)
        }),
    )
}

fn pi_req(initial_prompt: Option<&str>) -> HerdrLaunchPlan {
    HerdrLaunchPlan {
        name: "card-42-execute".into(),
        name_fallback: Some("card-42-execute-r7".into()),
        agent_kind: Some("pi".into()),
        initial_prompt: initial_prompt.map(str::to_string),
        system_prompt: Some("system instructions\nwith an exact second line".into()),
        tab_label: Some("kanban".into()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
        bootstrap: None,
        env: vec![("BOARD_CARD_ID".into(), "42".into())],
        argv: vec![
            "pi".into(),
            "--model".into(),
            "provider/model with space".into(),
            "--session-id".into(),
            "session-42".into(),
        ],
    }
}

fn claude_req() -> HerdrLaunchPlan {
    HerdrLaunchPlan {
        name: "card-42-execute".into(),
        name_fallback: Some("card-42-execute-r7".into()),
        agent_kind: Some("claude".into()),
        initial_prompt: None,
        system_prompt: Some("claude system instructions".into()),
        tab_label: Some("kanban".into()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
        bootstrap: None,
        env: vec![("BOARD_CARD_ID".into(), "42".into())],
        argv: vec![
            "claude".into(),
            "--model".into(),
            "provider/model with space".into(),
            "--effort".into(),
            "low".into(),
            "--permission-mode".into(),
            "acceptEdits".into(),
            "--allowedTools".into(),
            "Bash(board:*)".into(),
            "--resume".into(),
            "source-session".into(),
            "--fork-session".into(),
        ],
    }
}

/// A board-built codex launch plan: startup-only argv (Mint) or with the
/// `resume <id>` / `fork <id>` subcommand pair appended last (the exact shape
/// `board_core::harness::codex::managed_codex_invocation` persists).
fn codex_req(session_tail: &[&str], initial_prompt: Option<&str>) -> HerdrLaunchPlan {
    let mut argv = vec![
        "codex".into(),
        "--model".into(),
        "gpt-5.6".into(),
        "-c".into(),
        "model_reasoning_effort=low".into(),
    ];
    argv.extend(session_tail.iter().map(|s| s.to_string()));
    HerdrLaunchPlan {
        name: "card-42-execute".into(),
        name_fallback: Some("card-42-execute-r7".into()),
        agent_kind: Some("codex".into()),
        initial_prompt: initial_prompt.map(str::to_string),
        system_prompt: Some("codex system instructions".into()),
        tab_label: Some("kanban".into()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
        bootstrap: None,
        env: vec![("BOARD_CARD_ID".into(), "42".into())],
        argv,
    }
}

/// A board-built opencode launch plan: startup-only argv (Mint) or with the
/// `-s <id>` / `-s <id> --fork` session flags appended last (the exact shape
/// `board_core::harness::opencode::managed_opencode_invocation` persists).
///
/// The board effort rides a process-local config env — the TUI has no
/// `--variant` — so the plan carries the exact `OPENCODE_CONFIG_CONTENT` JSON
/// alongside the `--agent herdr-board` startup flags; `-m` never appears.
fn opencode_req(session_tail: &[&str], initial_prompt: Option<&str>) -> HerdrLaunchPlan {
    let mut argv = vec![
        "opencode".into(),
        "--agent".into(),
        board_core::harness::opencode::AGENT_NAME.into(),
        "--auto".into(),
    ];
    argv.extend(session_tail.iter().map(|s| s.to_string()));
    HerdrLaunchPlan {
        name: "card-42-execute".into(),
        name_fallback: Some("card-42-execute-r7".into()),
        agent_kind: Some("opencode".into()),
        initial_prompt: initial_prompt.map(str::to_string),
        system_prompt: Some("opencode system instructions".into()),
        tab_label: Some("kanban".into()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
        bootstrap: None,
        env: vec![
            ("BOARD_CARD_ID".into(), "42".into()),
            (
                board_core::harness::opencode::CONFIG_ENV.into(),
                board_core::harness::opencode::effort_agent_config(
                    "opencode/deepseek-v4-flash-free",
                    board_core::protocol::Effort::Low,
                ),
            ),
        ],
        argv,
    }
}

fn assert_startup_prompt_file(
    req: &Value,
    expected_base_args: &[&str],
    expected_flag: &str,
    expected_contents: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let args = req["params"]["args"].as_array().unwrap();
    let actual_base: Vec<_> = args[..expected_base_args.len()]
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(actual_base, expected_base_args, "base argv tail changed");
    assert_eq!(args.len(), expected_base_args.len() + 2);
    assert_eq!(args[expected_base_args.len()], expected_flag);
    let path = PathBuf::from(args.last().unwrap().as_str().unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected_contents);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "authoritative system prompt must never be group/world-readable",
    );
    path
}

fn custom_req(socket: PathBuf, cwd: PathBuf, argv: Vec<String>) -> HerdrLaunchPlan {
    HerdrLaunchPlan {
        name: "card-9-custom".into(),
        name_fallback: Some("card-9-custom-r1".into()),
        agent_kind: None,
        initial_prompt: None,
        system_prompt: None,
        tab_label: Some("kanban".into()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: Some(cwd),
        workspace_ref: Some("w1".into()),
        herdr_socket: Some(socket.clone()),
        bootstrap: None,
        env: vec![
            (
                "BOARD_PROMPT".into(),
                "configured task line one\nconfigured task line two".into(),
            ),
            (
                "BOARD_SYSTEM_PROMPT".into(),
                "configured system line one\nconfigured system line two".into(),
            ),
            (
                "HERDR_SOCKET_PATH".into(),
                socket.to_string_lossy().into_owned(),
            ),
        ],
        argv,
    }
}

fn managed_req(kind: &str) -> HerdrLaunchPlan {
    HerdrLaunchPlan {
        name: "card-7-execute".into(),
        agent_kind: Some(kind.into()),
        initial_prompt: Some("exact task".into()),
        system_prompt: Some("old system\nsecond line".into()),
        name_fallback: None,
        tab_label: None,
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        reuse_pane_id: None,
        cwd: None,
        workspace_ref: None,
        herdr_socket: None,
        bootstrap: None,
        env: vec![],
        argv: if kind == "pi" {
            vec![
                "pi".into(),
                "--model".into(),
                "m".into(),
                "--session-id".into(),
                "s".into(),
            ]
        } else {
            vec![
                "claude".into(),
                "--model".into(),
                "m".into(),
                "--allowedTools".into(),
                "Bash(*)".into(),
            ]
        },
    }
}

// ---------------------------------------------------------------------------
// Composed managed-retry budget assertion
// ---------------------------------------------------------------------------

fn assert_composed_busy_name_sequence(sequence: &[&str], expected_names: &[&str]) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let starts = Arc::new(AtomicUsize::new(0));
    let starts2 = Arc::clone(&starts);
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
    let sequence = sequence
        .iter()
        .map(|outcome| (*outcome).to_string())
        .collect::<Vec<_>>();
    let sequence2 = sequence.clone();
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
        "pane.split" => pane_result(req, "w1:p3"),
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
            let call = starts2.fetch_add(1, Ordering::SeqCst);
            match sequence2[call].as_str() {
                "busy" => error(req, "agent_pane_busy", "pane is still busy"),
                "name_taken" => error(req, "agent_name_taken", "agent name is taken"),
                "success" => agent_started(req, "w1:p3", false, true),
                outcome => panic!("unexpected test outcome {outcome}"),
            }
        }
        "pane.close" => pane_result(req, "w1:p3"),
        method => panic!("unexpected composed retry method {method}"),
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

    let err = spawner.spawn(&pi_req(None)).unwrap_err();
    assert!(err.to_string().contains("pane is still busy"));
    assert_eq!(starts.load(Ordering::SeqCst), sequence.len());
    assert_eq!(expected_names.len(), sequence.len());
    let mut expected_delays = Vec::new();
    let mut delay = super::AGENT_START_BUSY_BACKOFF;
    for _ in 0..super::AGENT_START_BUSY_RETRIES {
        expected_delays.push(delay);
        delay = delay.saturating_mul(2);
    }
    assert_eq!(
        delays.lock().unwrap().as_slice(),
        expected_delays.as_slice(),
        "busy delays must be globally bounded across the name fallback",
    );

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), sequence.len());
    for (request, expected_name) in starts.iter().zip(expected_names) {
        assert_eq!(request["params"]["name"], *expected_name);
        assert_eq!(request["params"]["pane_id"], "w1:p3");
        assert_eq!(request["params"]["kind"], "pi");
        assert_eq!(request["params"]["timeout_ms"], 30_000);
        assert_eq!(request["params"]["args"], starts[0]["params"]["args"]);
    }
    let prompt_paths = prompt_paths.lock().unwrap();
    assert!(prompt_paths.windows(2).all(|paths| paths[0] == paths[1]));
    assert!(!prompt_paths[0].exists());
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1,
        "the name fallback must reuse the one owned pane",
    );
    let closes: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "pane.close")
        .map(|request| request["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(closes, ["w1:p3"]);
    assert!(!closes.contains(&"w1:p1"));
}

mod card_tabs;
mod configured;
mod failures;
mod local;
mod managed;
mod pane_reuse;
mod placement;
mod races;
