//! `Spawner` implementations: `HerdrSpawner` (agent panes) and `LocalSpawner`
//! (plain child processes, used by tests with the fake harness).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context};
use board_core::spawn::{SpawnHandle, SpawnReq, Spawner};
use board_herdr::{AgentStartParams, HerdrClient};

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
/// Holds only the socket path and opens a fresh [`HerdrClient`] per call, so a
/// missing herdr surfaces as a per-run spawn error (the daemon marks the run
/// `fail`) rather than crashing at startup.
#[derive(Clone)]
pub struct HerdrSpawner {
    socket: PathBuf,
}

impl HerdrSpawner {
    pub fn new(socket: PathBuf) -> HerdrSpawner {
        HerdrSpawner { socket }
    }

    fn client(&self) -> anyhow::Result<HerdrClient> {
        HerdrClient::connect(&self.socket).map_err(|e| anyhow!("herdr unavailable: {e}"))
    }
}

impl Spawner for HerdrSpawner {
    fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle> {
        let mut client = self.client()?;
        let env: BTreeMap<String, String> = req.env.iter().cloned().collect();
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
        let started = client
            .agent_start(&params)
            .with_context(|| format!("herdr agent.start for {}", req.name))?;
        Ok(SpawnHandle {
            pane_id: Some(started.pane_id().to_string()),
            workspace_id: Some(started.workspace_id().to_string()),
            pid: None,
        })
    }

    fn kill(&self, h: &SpawnHandle) -> anyhow::Result<()> {
        if let Some(pane) = &h.pane_id {
            let mut client = self.client()?;
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
        let mut client = self.client()?;
        let snap = client
            .session_snapshot()
            .context("herdr session.snapshot")?;
        Ok(snap.pane_exists(pane))
    }
}
