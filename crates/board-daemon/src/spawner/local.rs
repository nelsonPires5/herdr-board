use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};

use super::{HerdrLaunchPlan, RuntimeHandle, Spawner};

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

/// Reconstruct the legacy direct-process argv for an explicitly managed
/// invocation. Configured (unmanaged) commands remain byte-for-byte unchanged.
pub(crate) fn materialize_local_argv(req: &HerdrLaunchPlan) -> anyhow::Result<Vec<String>> {
    let Some(kind) = req.agent_kind.as_deref() else {
        return Ok(req.argv.clone());
    };
    let system_prompt = req
        .system_prompt
        .as_ref()
        .ok_or_else(|| anyhow!("managed {kind} invocation is missing system_prompt metadata"))?;
    let initial_prompt = req
        .initial_prompt
        .as_ref()
        .ok_or_else(|| anyhow!("managed {kind} invocation is missing initial_prompt metadata"))?;
    if req.argv.is_empty() {
        bail!("managed {kind} invocation has empty startup argv");
    }

    let mut argv = req.argv.clone();
    match kind {
        "pi" => {
            // Historic Pi order put the system prompt after model/thinking and
            // before all session/fork flags, with the card task last.
            let insert_at = argv
                .iter()
                .position(|arg| arg == "--session-id" || arg == "--fork")
                .map_or(argv.len(), |index| index);
            argv.splice(
                insert_at..insert_at,
                ["--append-system-prompt".to_string(), system_prompt.clone()],
            );
            argv.push(format!("Card task:\n{initial_prompt}"));
        }
        "claude" => {
            // Historic Claude order put this pair immediately before the
            // board-tool allowlist, then used `--` for the final card task.
            let insert_at = argv
                .iter()
                .position(|arg| arg == "--allowedTools")
                .ok_or_else(|| {
                    anyhow!("managed claude invocation is missing --allowedTools startup metadata")
                })?;
            argv.splice(
                insert_at..insert_at,
                ["--append-system-prompt".to_string(), system_prompt.clone()],
            );
            argv.extend(["--".to_string(), initial_prompt.clone()]);
        }
        other => bail!("unsupported managed harness kind: {other}"),
    }
    Ok(argv)
}

impl Spawner for LocalSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        let argv = materialize_local_argv(req)?;
        let (prog, args) = argv.split_first().ok_or_else(|| anyhow!("empty argv"))?;
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
        self.children
            .lock()
            .map_err(|_| anyhow!("local spawner child registry lock poisoned"))?
            .insert(pid, child);
        Ok(RuntimeHandle {
            pid: Some(pid),
            ..Default::default()
        })
    }

    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()> {
        if let Some(pid) = h.pid {
            let child = self
                .children
                .lock()
                .map_err(|_| anyhow!("local spawner child registry lock poisoned"))?
                .remove(&pid);
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        Ok(())
    }

    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool> {
        let Some(pid) = h.pid else { return Ok(false) };
        let mut guard = self
            .children
            .lock()
            .map_err(|_| anyhow!("local spawner child registry lock poisoned"))?;
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
