//! `Spawner` implementations: `HerdrSpawner` (agent panes) and `LocalSpawner`
//! (plain child processes, used by tests with the fake harness).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context};
use board_core::spawn::{SpawnHandle, SpawnReq, Spawner};
use board_herdr::{
    AgentStartParams, AgentStarted, HerdrClient, HerdrError, LayoutPane, SplitDirection,
    TabCreateParams,
};

// ---------------------------------------------------------------------------
// LocalSpawner
// ---------------------------------------------------------------------------

/// Launches agents as ordinary child processes. Keeps each `Child` so liveness
/// checks can `try_wait` (reaping zombies) and kills are precise.
#[derive(Default)]
pub struct LocalSpawner {
    children: Arc<Mutex<HashMap<u32, Child>>>,
}

impl LocalSpawner {
    pub fn new() -> LocalSpawner {
        LocalSpawner::default()
    }
}

impl Spawner for LocalSpawner {
    fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle> {
        let (prog, args) = req
            .argv
            .split_first()
            .ok_or_else(|| anyhow!("empty argv"))?;
        let mut cmd = Command::new(prog);
        cmd.args(args);
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        // Inherit the daemon's environment (so e.g. BOARD_BIN flows through in
        // tests) and layer the per-run vars on top.
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .with_context(|| format!("spawning {prog} for {}", req.name))?;
        let pid = child.id();
        self.children.lock().unwrap().insert(pid, child);
        Ok(SpawnHandle {
            pid: Some(pid),
            ..Default::default()
        })
    }

    fn kill(&self, h: &SpawnHandle) -> anyhow::Result<()> {
        if let Some(pid) = h.pid {
            if let Some(mut child) = self.children.lock().unwrap().remove(&pid) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        Ok(())
    }

    fn is_alive(&self, h: &SpawnHandle) -> anyhow::Result<bool> {
        let Some(pid) = h.pid else { return Ok(false) };
        let mut guard = self.children.lock().unwrap();
        match guard.get_mut(&pid) {
            Some(child) => match child.try_wait()? {
                Some(_status) => {
                    guard.remove(&pid);
                    Ok(false)
                }
                None => Ok(true),
            },
            // Not tracked (e.g. after a daemon restart) → treat as gone.
            None => Ok(false),
        }
    }
}

// ---------------------------------------------------------------------------
// HerdrSpawner
// ---------------------------------------------------------------------------

/// Launches agents as herdr panes via `agent.start`; kills via `pane.close`;
/// liveness via `session.snapshot`.
///
/// Holds a *default* socket path and opens a fresh [`HerdrClient`] per call —
/// but each call targets the run's session socket when one is supplied
/// (`SpawnReq::herdr_socket` for spawn, `SpawnHandle::herdr_socket` for
/// kill/liveness), falling back to the default. A missing herdr surfaces as a
/// per-run spawn error (the daemon marks the run `fail`) rather than crashing.
#[derive(Clone)]
pub struct HerdrSpawner {
    socket: PathBuf,
}

impl HerdrSpawner {
    pub fn new(socket: PathBuf) -> HerdrSpawner {
        HerdrSpawner { socket }
    }

    /// Open a client on `socket` (the run's session), else the default socket.
    fn client_for(&self, socket: Option<&Path>) -> anyhow::Result<HerdrClient> {
        let target = socket.unwrap_or(&self.socket);
        HerdrClient::connect(target).map_err(|e| anyhow!("herdr unavailable: {e}"))
    }
}

impl Spawner for HerdrSpawner {
    fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle> {
        let mut client = self.client_for(req.herdr_socket.as_deref())?;
        let env: BTreeMap<String, String> = req.env.iter().cloned().collect();

        // Placement into a labeled tab (grid layout) only makes sense with a
        // workspace to host the tab. Otherwise fall back to a bare agent.start.
        let started = match (&req.tab_label, &req.workspace_ref) {
            (Some(label), Some(ws_id)) => place_in_tab(&mut client, req, &env, ws_id, label)
                .with_context(|| format!("herdr agent.start (tab '{label}') for {}", req.name))?,
            _ => {
                let params = AgentStartParams {
                    name: req.name.clone(),
                    argv: req.argv.clone(),
                    cwd: req.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    workspace_id: req.workspace_ref.clone(),
                    tab_id: None,
                    split: None,
                    env,
                    focus: false,
                };
                agent_start_retry_name(&mut client, &params, req.name_fallback.as_deref())
                    .with_context(|| format!("herdr agent.start for {}", req.name))?
            }
        };
        Ok(SpawnHandle {
            pane_id: Some(started.pane_id().to_string()),
            workspace_id: Some(started.workspace_id().to_string()),
            pid: None,
            herdr_socket: req.herdr_socket.clone(),
        })
    }

    fn kill(&self, h: &SpawnHandle) -> anyhow::Result<()> {
        if let Some(pane) = &h.pane_id {
            let mut client = self.client_for(h.herdr_socket.as_deref())?;
            client
                .pane_close(pane)
                .with_context(|| format!("herdr pane.close {pane}"))?;
        }
        Ok(())
    }

    fn is_alive(&self, h: &SpawnHandle) -> anyhow::Result<bool> {
        let Some(pane) = &h.pane_id else {
            return Ok(false);
        };
        let mut client = self.client_for(h.herdr_socket.as_deref())?;
        let snap = client
            .session_snapshot()
            .context("herdr session.snapshot")?;
        Ok(snap.pane_exists(pane))
    }
}

// ---------------------------------------------------------------------------
// Tab placement + grid layout
// ---------------------------------------------------------------------------

/// herdr protocol error code: the primary agent name is already in use by an
/// open pane. We retry once with the run-scoped fallback name.
const ERR_AGENT_NAME_TAKEN: &str = "agent_name_taken";
/// herdr protocol error code: `agent.start` targeted a tab that no longer
/// exists (raced away between find-or-create and start). We redo find-or-create.
const ERR_PLACEMENT_NOT_FOUND: &str = "agent_placement_not_found";

/// Place a run's agent pane in the `label` tab of `ws_id`: find-or-create the
/// tab, then either fill a freshly-created tab (no split, closing its leftover
/// shell pane) or split the largest existing pane per [`grid_slot`].
///
/// Retries find-or-create once if the tab vanishes between listing and
/// `agent.start` ([`ERR_PLACEMENT_NOT_FOUND`]).
fn place_in_tab(
    client: &mut HerdrClient,
    req: &SpawnReq,
    env: &BTreeMap<String, String>,
    ws_id: &str,
    label: &str,
) -> anyhow::Result<AgentStarted> {
    let mut last_err: Option<HerdrError> = None;
    for attempt in 0..2 {
        match try_place_once(client, req, env, ws_id, label) {
            Ok(started) => return Ok(started),
            Err(HerdrError::Protocol { code, message })
                if code == ERR_PLACEMENT_NOT_FOUND && attempt == 0 =>
            {
                // Tab raced away; loop redoes find-or-create.
                last_err = Some(HerdrError::Protocol { code, message });
            }
            Err(e) => return Err(anyhow!(e)),
        }
    }
    Err(anyhow!(
        last_err.expect("loop only exits early or records an error")
    ))
}

/// One placement attempt: find-or-create the tab and start the agent in it.
fn try_place_once(
    client: &mut HerdrClient,
    req: &SpawnReq,
    env: &BTreeMap<String, String>,
    ws_id: &str,
    label: &str,
) -> Result<AgentStarted, HerdrError> {
    let (tab_id, root_pane, freshly_created) = find_or_create_tab(client, ws_id, label)?;

    let mut params = AgentStartParams {
        name: req.name.clone(),
        argv: req.argv.clone(),
        cwd: req.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        workspace_id: Some(ws_id.to_string()),
        tab_id: Some(tab_id.clone()),
        split: None,
        env: env.clone(),
        focus: false,
    };

    if freshly_created {
        // First agent in the tab lands unsplit; then close the leftover shell
        // pane the tab was born with (best effort — a close race is harmless).
        let started = agent_start_retry_name(client, &params, req.name_fallback.as_deref())?;
        if let Some(root) = &root_pane {
            let _ = client.pane_close(root);
        }
        return Ok(started);
    }

    // Existing tab: split its largest pane to keep the mesh roughly square.
    // Use any pane in the tab as the layout anchor.
    let tab_panes: Vec<_> = client
        .pane_list(Some(ws_id))?
        .into_iter()
        .filter(|p| p.tab_id == tab_id)
        .collect();
    let anchor = tab_panes.first().map(|p| p.pane_id.clone());
    let layout = client.pane_layout(anchor.as_deref())?;

    if layout.panes.is_empty() {
        // No panes to split (unexpected for an existing tab): land unsplit.
        return agent_start_retry_name(client, &params, req.name_fallback.as_deref());
    }

    let (target, dir) = grid_slot(&layout.panes);
    // Non-atomic: `agent.start{split}` splits the tab's FOCUSED pane, so we
    // focus the chosen target immediately before starting. A human focusing a
    // pane concurrently could steal the split target; acceptable because
    // dispatch serializes spawns per space queue.
    client.pane_focus(&target)?;
    params.split = Some(dir);
    agent_start_retry_name(client, &params, req.name_fallback.as_deref())
}

/// Find the `label` tab in `ws_id`, or create it. Returns
/// `(tab_id, root_pane_id, freshly_created)`. Labels are not unique in herdr, so
/// the match is the first tab with that label, lowest `number` on ties.
fn find_or_create_tab(
    client: &mut HerdrClient,
    ws_id: &str,
    label: &str,
) -> Result<(String, Option<String>, bool), HerdrError> {
    let tabs = client.tab_list(Some(ws_id))?;
    if let Some(t) = tabs
        .iter()
        .filter(|t| t.label == label)
        .min_by_key(|t| t.number)
    {
        return Ok((t.tab_id.clone(), None, false));
    }
    let created = client.tab_create(&TabCreateParams {
        workspace_id: Some(ws_id.to_string()),
        label: Some(label.to_string()),
        focus: false,
        ..Default::default()
    })?;
    Ok((created.tab.tab_id, Some(created.root_pane.pane_id), true))
}

/// Run `agent.start`; on [`ERR_AGENT_NAME_TAKEN`] retry once with `fallback`.
fn agent_start_retry_name(
    client: &mut HerdrClient,
    params: &AgentStartParams,
    fallback: Option<&str>,
) -> Result<AgentStarted, HerdrError> {
    match client.agent_start(params) {
        Err(HerdrError::Protocol { code, message }) if code == ERR_AGENT_NAME_TAKEN => {
            match fallback {
                Some(name) => {
                    let mut retry = params.clone();
                    retry.name = name.to_string();
                    client.agent_start(&retry)
                }
                None => Err(HerdrError::Protocol { code, message }),
            }
        }
        other => other,
    }
}

/// Choose which existing pane to split and in which direction so the tab's pane
/// mesh stays visually roughly square (4 panes ≈ 2x2). Splits the pane with the
/// largest rect area; direction is `Right` when that pane is at least twice as
/// wide as tall (terminal cells are ~1:2), else `Down`.
///
/// Precondition: `panes` is non-empty.
pub fn grid_slot(panes: &[LayoutPane]) -> (String, SplitDirection) {
    let target = panes
        .iter()
        .max_by_key(|p| p.rect.width * p.rect.height)
        .expect("grid_slot requires at least one pane");
    let dir = if target.rect.width >= 2 * target.rect.height {
        SplitDirection::Right
    } else {
        SplitDirection::Down
    };
    (target.pane_id.clone(), dir)
}

#[cfg(test)]
mod tests {
    use super::grid_slot;
    use board_herdr::{LayoutPane, Rect, SplitDirection};

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

    #[test]
    fn single_pane_is_the_split_target() {
        let panes = [pane("p1", 200, 40)];
        let (target, _) = grid_slot(&panes);
        assert_eq!(target, "p1");
    }

    #[test]
    fn wide_pane_splits_right() {
        // width (200) >= 2 * height (40) → Right.
        let panes = [pane("p1", 200, 40)];
        let (_, dir) = grid_slot(&panes);
        assert_eq!(dir, SplitDirection::Right);
    }

    #[test]
    fn tall_narrowish_pane_splits_down() {
        // width (60) < 2 * height (50) → Down.
        let panes = [pane("p1", 60, 50)];
        let (target, dir) = grid_slot(&panes);
        assert_eq!(target, "p1");
        assert_eq!(dir, SplitDirection::Down);
    }

    #[test]
    fn largest_area_pane_wins() {
        let panes = [
            pane("small", 50, 10),
            pane("biggest", 200, 40),
            pane("medium", 30, 30),
        ];
        let (target, dir) = grid_slot(&panes);
        assert_eq!(target, "biggest");
        assert_eq!(dir, SplitDirection::Right);
    }
}
