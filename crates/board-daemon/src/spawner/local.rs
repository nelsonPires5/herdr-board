use std::collections::HashMap;
#[cfg(any(windows, test))]
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};

#[cfg(windows)]
use super::job_object::ProcessTree;
use super::{HerdrLaunchPlan, RuntimeHandle, SpawnError, Spawner};

// ---------------------------------------------------------------------------
// LocalSpawner
// ---------------------------------------------------------------------------

/// Launches agents as ordinary child processes. Keeps each `Child` so liveness
/// checks can `try_wait` (reaping zombies) and kills are precise.
///
/// On Windows each child also owns a kill-on-close Job Object, so a kill (or
/// reaping the child) takes down everything it started — a `.cmd` shim's
/// `node`, say — instead of just the shim's `cmd.exe`. On unix a kill stays
/// the single-process `SIGKILL` it always was.
#[derive(Default)]
pub struct LocalSpawner {
    children: Arc<Mutex<HashMap<u32, Tracked>>>,
}

/// One launched child plus, on Windows, the job that owns its process tree.
struct Tracked {
    child: Child,
    #[cfg(windows)]
    tree: Option<ProcessTree>,
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
                .unwrap_or(argv.len());
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

impl LocalSpawner {
    /// The launch itself, in `anyhow` terms. Nothing here touches Herdr, so it
    /// always classifies as [`SpawnError::Other`].
    fn spawn_inner(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        let argv = materialize_local_argv(req)?;
        let (prog, args) = argv.split_first().ok_or_else(|| anyhow!("empty argv"))?;
        // Identity on unix; PATH + PATHEXT on Windows, so an npm-style
        // `pi.cmd` shim resolves the way a shell would find it.
        #[cfg(windows)]
        if let Some(program) = board_core::exec::resolve_program(prog) {
            if let Some(problem) = batch_argv_problem(&program, args) {
                bail!("cannot launch {prog} for {}: {problem}", req.name);
            }
        }
        let mut cmd = board_core::exec::command(prog);
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
        #[cfg(windows)]
        let tracked = {
            // Degrade to a single-process kill rather than failing a launch
            // that already started (e.g. when job nesting is refused).
            let tree = match ProcessTree::adopt(&child) {
                Ok(tree) => Some(tree),
                Err(error) => {
                    tracing::warn!(
                        pid,
                        error = %error,
                        "local child not placed in a job object; kill will not reach its descendants"
                    );
                    None
                }
            };
            Tracked { child, tree }
        };
        #[cfg(not(windows))]
        let tracked = Tracked { child };
        self.children
            .lock()
            .map_err(|_| anyhow!("local spawner child registry lock poisoned"))?
            .insert(pid, tracked);
        Ok(RuntimeHandle {
            pid: Some(pid),
            ..Default::default()
        })
    }
}

impl Spawner for LocalSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> Result<RuntimeHandle, SpawnError> {
        self.spawn_inner(req).map_err(SpawnError::from)
    }

    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()> {
        if let Some(pid) = h.pid {
            let child = self
                .children
                .lock()
                .map_err(|_| anyhow!("local spawner child registry lock poisoned"))?
                .remove(&pid);
            if let Some(mut tracked) = child {
                #[cfg(windows)]
                if let Some(tree) = &tracked.tree {
                    let _ = tree.terminate();
                }
                let _ = tracked.child.kill();
                let _ = tracked.child.wait();
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
            // Dropping the reaped entry also closes its Windows job, ending
            // any descendant the exited child left behind.
            Some(tracked) => match tracked.child.try_wait()? {
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

/// Why Windows cannot hand `args` to `program` when it is a batch file
/// (`.cmd`/`.bat`, the usual npm shim), or `None` when it can. `std` refuses
/// a CR or LF in any batch argument because `cmd.exe` would cut the command
/// line there — and the managed Pi/Claude materialization above always
/// carries the multiline system prompt and card task as argv. Launch the
/// real executable (e.g. `node …/cli.js`) instead of the shim for those.
#[cfg(any(windows, test))]
pub(crate) fn batch_argv_problem(program: &Path, args: &[String]) -> Option<String> {
    let is_batch = program
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return None;
    }
    let index = args.iter().position(|arg| arg.contains(['\r', '\n']))?;
    Some(format!(
        "{} is a batch file and argument {} contains a line break, which Windows cannot pass \
         to a .cmd/.bat program; point the harness at the real executable instead of its shim",
        program.display(),
        index + 1
    ))
}
