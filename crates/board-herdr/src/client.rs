//! Blocking herdr socket client (`std::os::unix::net::UnixStream`, no async).
//!
//! One [`HerdrClient`] owns a single request/response connection. Calls are
//! synchronous: the daemon is expected to wrap them in `spawn_blocking` or a
//! dedicated thread. Event streaming lives on a separate connection — see
//! [`crate::events`].

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::envelope::{Request, Response};
use crate::error::{HerdrError, Result};
use crate::types::{
    AgentInfo, AgentStarted, AgentStatus, Layout, NotificationShown, NotificationSound, PaneInfo,
    PaneReadResult, Pong, ReadSource, SessionSnapshot, SplitDirection, TabCreated, TabInfo,
    WorkspaceCreated, WorkspaceInfo, WorktreeCreated, WorktreeRemoved,
};

/// Default socket path: `$HERDR_SOCKET_PATH` (herdr's canonical variable,
/// injected into panes/plugins so named sessions resolve to their own socket),
/// else `$HERDR_SOCKET` (this crate's override), else the default session's
/// `~/.config/herdr/herdr.sock`.
pub fn default_socket_path() -> PathBuf {
    for var in ["HERDR_SOCKET_PATH", "HERDR_SOCKET"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".config/herdr/herdr.sock")
}

// -- request parameter structs ----------------------------------------------

/// Params for `workspace.create`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkspaceCreateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

/// Params for `tab.create`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TabCreateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

/// Protocol-17 params for `agent.start`. Placement, cwd, and environment are
/// established on the target pane before starting the managed agent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentStartParams {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Optional wait behavior for `agent.prompt`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPromptWaitOptions {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Params for `agent.prompt`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPromptParams {
    pub target: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<AgentPromptWaitOptions>,
}

/// Params for `agent.wait`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentWaitParams {
    pub target: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Params for protocol-17 `pane.split`.
#[derive(Debug, Clone, Serialize)]
pub struct PaneSplitParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub target_pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub direction: SplitDirection,
    pub focus: bool,
}

/// Params for `pane.rename`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PaneRenameParams {
    pub pane_id: String,
    pub label: String,
}

/// Params for `worktree.create`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorktreeCreateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub focus: bool,
}

// -- client ------------------------------------------------------------------

/// A blocking client for the herdr socket.
///
/// herdr serves **one request per connection** — it closes the socket after
/// each response (like the `herdr` CLI). So every [`call`](HerdrClient::call)
/// opens a fresh connection. The client is therefore cheap to clone-by-path and
/// safe to keep around; it holds no live socket between calls.
#[derive(Debug, Clone)]
pub struct HerdrClient {
    path: PathBuf,
    next_id: u64,
}

impl HerdrClient {
    /// Bind a client to the socket at `path`, verifying it is reachable.
    pub fn connect(path: &Path) -> Result<HerdrClient> {
        // Fail fast if the socket is missing/unreachable; the probe connection
        // is dropped immediately (herdr tolerates a no-request connection).
        let _probe = UnixStream::connect(path)?;
        Ok(HerdrClient {
            path: path.to_path_buf(),
            next_id: 0,
        })
    }

    /// Connect using [`default_socket_path`].
    pub fn connect_default() -> Result<HerdrClient> {
        HerdrClient::connect(&default_socket_path())
    }

    /// The socket path this client is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Send one request on a fresh connection and return its `result` payload,
    /// mapping an `error` envelope to [`HerdrError::Protocol`] and EOF to
    /// [`HerdrError::Disconnected`].
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let req = Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        };

        let stream = UnixStream::connect(&self.path)?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        writer.write_all(req.to_line()?.as_bytes())?;
        writer.flush()?;

        loop {
            let mut buf = String::new();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                return Err(HerdrError::Disconnected);
            }
            if buf.trim().is_empty() {
                continue;
            }
            let resp = Response::from_line(&buf)?;
            // Ignore anything that is not this request's reply.
            if !resp.id.is_empty() && resp.id != id {
                continue;
            }
            if let Some(err) = resp.error {
                return Err(HerdrError::Protocol {
                    code: err.code,
                    message: err.message,
                });
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    fn call_into<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let v = self.call(method, params)?;
        Ok(serde_json::from_value(v)?)
    }

    fn call_field<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
        field: &str,
    ) -> Result<T> {
        let v = self.call(method, params)?;
        let inner = v.get(field).cloned().unwrap_or(Value::Null);
        Ok(serde_json::from_value(inner)?)
    }

    // -- liveness ------------------------------------------------------------

    /// `ping` round-trip. Cheap; use for liveness checks.
    pub fn ping(&mut self) -> Result<Pong> {
        self.call_into("ping", json!({}))
    }

    /// Require the exact Herdr release and socket protocol supported by this
    /// client. The explicit protocol argument keeps callers' expected contract
    /// visible at the gate.
    pub fn require_protocol(&mut self, expected: u32) -> Result<Pong> {
        let pong = self.ping()?;
        if pong.version != "0.7.5" || pong.protocol != expected || expected != 17 {
            return Err(HerdrError::Protocol {
                code: "incompatible_protocol".to_string(),
                message: format!(
                    "Herdr 0.7.5 with protocol 17 is required (found Herdr {} with protocol {})",
                    pong.version, pong.protocol
                ),
            });
        }
        Ok(pong)
    }

    /// True if a `ping` currently succeeds. The daemon uses this to set its
    /// `herdr_connected` flag.
    pub fn is_live(&mut self) -> bool {
        self.ping().is_ok()
    }

    // -- workspace -----------------------------------------------------------

    pub fn workspace_create(&mut self, p: &WorkspaceCreateParams) -> Result<WorkspaceCreated> {
        self.call_into("workspace.create", serde_json::to_value(p)?)
    }

    pub fn workspace_list(&mut self) -> Result<Vec<WorkspaceInfo>> {
        self.call_field("workspace.list", json!({}), "workspaces")
    }

    pub fn workspace_close(&mut self, workspace_id: &str) -> Result<()> {
        self.call("workspace.close", json!({ "workspace_id": workspace_id }))?;
        Ok(())
    }

    // -- tab -----------------------------------------------------------------

    pub fn tab_create(&mut self, p: &TabCreateParams) -> Result<TabCreated> {
        self.call_into("tab.create", serde_json::to_value(p)?)
    }

    /// List tabs, optionally scoped to one workspace (`None` = all workspaces).
    pub fn tab_list(&mut self, workspace_id: Option<&str>) -> Result<Vec<TabInfo>> {
        self.call_field("tab.list", json!({ "workspace_id": workspace_id }), "tabs")
    }

    // -- agent ---------------------------------------------------------------

    pub fn agent_start(&mut self, p: &AgentStartParams) -> Result<AgentStarted> {
        self.call_into("agent.start", serde_json::to_value(p)?)
    }

    pub fn agent_get(&mut self, target: &str) -> Result<AgentInfo> {
        self.call_field("agent.get", json!({ "target": target }), "agent")
    }

    pub fn agent_prompt(&mut self, p: &AgentPromptParams) -> Result<AgentInfo> {
        self.call_field("agent.prompt", serde_json::to_value(p)?, "agent")
    }

    pub fn agent_wait(&mut self, p: &AgentWaitParams) -> Result<AgentInfo> {
        self.call_field("agent.wait", serde_json::to_value(p)?, "agent")
    }

    // -- pane ----------------------------------------------------------------

    pub fn pane_list(&mut self, workspace_id: Option<&str>) -> Result<Vec<PaneInfo>> {
        let params = match workspace_id {
            Some(w) => json!({ "workspace_id": w }),
            None => json!({}),
        };
        self.call_field("pane.list", params, "panes")
    }

    pub fn pane_split(&mut self, p: &PaneSplitParams) -> Result<PaneInfo> {
        self.call_field("pane.split", serde_json::to_value(p)?, "pane")
    }

    pub fn pane_read(
        &mut self,
        pane_id: &str,
        source: ReadSource,
        lines: Option<u32>,
    ) -> Result<PaneReadResult> {
        let params = json!({
            "pane_id": pane_id,
            "source": source,
            "lines": lines,
        });
        self.call_field("pane.read", params, "read")
    }

    pub fn pane_send_text(&mut self, pane_id: &str, text: &str) -> Result<()> {
        self.call(
            "pane.send_text",
            json!({ "pane_id": pane_id, "text": text }),
        )?;
        Ok(())
    }

    pub fn pane_send_keys(&mut self, pane_id: &str, keys: &[String]) -> Result<()> {
        self.call(
            "pane.send_keys",
            json!({ "pane_id": pane_id, "keys": keys }),
        )?;
        Ok(())
    }

    pub fn pane_close(&mut self, pane_id: &str) -> Result<()> {
        self.call("pane.close", json!({ "pane_id": pane_id }))?;
        Ok(())
    }

    /// Focus a pane; returns the pane's updated [`PaneInfo`].
    pub fn pane_focus(&mut self, pane_id: &str) -> Result<PaneInfo> {
        self.call_field("pane.focus", json!({ "pane_id": pane_id }), "pane")
    }

    /// Rename a pane; returns the pane's updated [`PaneInfo`].
    pub fn pane_rename(&mut self, p: &PaneRenameParams) -> Result<PaneInfo> {
        self.call_field("pane.rename", serde_json::to_value(p)?, "pane")
    }

    /// Fetch the pane [`Layout`] for the tab containing `pane_id` (`None` = the
    /// focused tab).
    pub fn pane_layout(&mut self, pane_id: Option<&str>) -> Result<Layout> {
        self.call_field("pane.layout", json!({ "pane_id": pane_id }), "layout")
    }

    // -- worktree ------------------------------------------------------------

    pub fn worktree_create(&mut self, p: &WorktreeCreateParams) -> Result<WorktreeCreated> {
        self.call_into("worktree.create", serde_json::to_value(p)?)
    }

    pub fn worktree_remove(&mut self, workspace_id: &str, force: bool) -> Result<WorktreeRemoved> {
        self.call_into(
            "worktree.remove",
            json!({ "workspace_id": workspace_id, "force": force }),
        )
    }

    // -- notification --------------------------------------------------------

    pub fn notification_show(
        &mut self,
        title: &str,
        body: Option<&str>,
        sound: NotificationSound,
    ) -> Result<NotificationShown> {
        self.call_into(
            "notification.show",
            json!({ "title": title, "body": body, "sound": sound }),
        )
    }

    // -- session -------------------------------------------------------------

    pub fn session_snapshot(&mut self) -> Result<SessionSnapshot> {
        self.call_field("session.snapshot", json!({}), "snapshot")
    }
}
