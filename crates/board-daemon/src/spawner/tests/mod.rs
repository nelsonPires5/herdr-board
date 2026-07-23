use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use super::herdr::{
    configured_script, posix_quote, remove_file_if_exists, HerdrCliPaneRunner, PaneRunner,
};
use super::local::materialize_local_argv;
use super::placement::grid_slot;
use super::{HerdrLaunchPlan, HerdrSpawner, Spawner};

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
// Protocol-17 pane-first launch contracts
// -----------------------------------------------------------------------

struct RecordingHerdr {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
}

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

fn serve_recording_herdr<F>(handler: F) -> RecordingHerdr
where
    F: Fn(&Value, usize) -> Value + Send + Sync + 'static,
{
    serve_recording_herdr_with_ping(handler, "0.7.5", 17)
}

fn serve_recording_herdr_with_ping<F>(handler: F, version: &str, protocol: u32) -> RecordingHerdr
where
    F: Fn(&Value, usize) -> Value + Send + Sync + 'static,
{
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests2 = Arc::clone(&requests);
    let handler = Arc::new(handler);
    let version = version.to_string();
    thread::spawn(move || {
        let mut handler_index = 0;
        for conn in listener.incoming() {
            let Ok(stream) = conn else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader
                .read_line(&mut line)
                .ok()
                .filter(|n| *n > 0)
                .is_none()
            {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            {
                let mut seen = requests2.lock().unwrap();
                seen.push(request.clone());
            }
            let response = if request["method"] == "ping" {
                serde_json::json!({
                    "id": request["id"].clone(),
                    "result": {"type": "pong", "version": version.clone(), "protocol": protocol, "capabilities": {}}
                })
            } else {
                let response = handler(&request, handler_index);
                handler_index += 1;
                response
            };
            writeln!(writer, "{}", response).unwrap();
            writer.flush().unwrap();
        }
    });
    RecordingHerdr {
        _dir: dir,
        socket,
        requests,
    }
}

fn reply(req: &Value, result: Value) -> Value {
    serde_json::json!({"id": req["id"].clone(), "result": result})
}

fn error(req: &Value, code: &str, message: &str) -> Value {
    serde_json::json!({
        "id": req["id"].clone(),
        "error": {"code": code, "message": message}
    })
}

/// Minimal schema-valid protocol-17 `PaneInfo` fixture. In particular,
/// `focused` and `revision` are required by the authoritative schema.
fn pane_info(id: &str) -> Value {
    serde_json::json!({
        "pane_id": id,
        "terminal_id": format!("term-{id}"),
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "focused": false,
        "agent_status": "unknown",
        "revision": 1
    })
}

fn agent_info(pane_id: &str, name: &str, pending: bool, ready: bool) -> Value {
    let mut agent = pane_info(pane_id);
    agent["name"] = Value::String(name.into());
    agent["launch_pending"] = Value::Bool(pending);
    agent["interactive_ready"] = Value::Bool(ready);
    agent
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

fn tab_created(req: &Value, root_pane: &str) -> Value {
    reply(
        req,
        serde_json::json!({
            "type": "tab_created",
            "tab": {
                "tab_id": "w1:t1", "workspace_id": "w1", "number": 1,
                "label": "kanban", "focused": false, "pane_count": 1,
                "agent_status": "unknown"
            },
            "root_pane": pane_info(root_pane)
        }),
    )
}

fn pane_result(req: &Value, pane_id: &str) -> Value {
    reply(
        req,
        serde_json::json!({"type": "pane_info", "pane": pane_info(pane_id)}),
    )
}

fn agent_started(req: &Value, pane_id: &str, pending: bool, ready: bool) -> Value {
    let name = req["params"]["name"].as_str().unwrap();
    let mut argv = vec![Value::String(
        req["params"]["kind"].as_str().unwrap().into(),
    )];
    argv.extend(req["params"]["args"].as_array().unwrap().iter().cloned());
    reply(
        req,
        serde_json::json!({
            "type": "agent_started",
            "agent": agent_info(pane_id, name, pending, ready),
            "argv": argv
        }),
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
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
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
        cwd: Some(PathBuf::from("/tmp/card cwd")),
        workspace_ref: Some("w1".into()),
        herdr_socket: None,
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
        cwd: Some(cwd),
        workspace_ref: Some("w1".into()),
        herdr_socket: Some(socket.clone()),
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
        cwd: None,
        workspace_ref: None,
        herdr_socket: None,
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

mod configured;
mod failures;
mod local;
mod managed;
mod placement;
