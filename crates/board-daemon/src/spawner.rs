//! `Spawner` implementations: `HerdrSpawner` (agent panes) and `LocalSpawner`
//! (plain child processes, used by tests with the fake harness).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use board_herdr::{
    AgentInfo, AgentPromptParams, AgentStartParams, AgentStarted, HerdrClient, HerdrError,
    LayoutPane, PaneRenameParams, PaneSplitParams, SplitDirection, TabCreateParams,
};

/// A request to launch one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrLaunchPlan {
    /// herdr agent/pane label, e.g. `card-42-execute`.
    pub name: String,
    /// Explicit Herdr managed-agent kind (`pi` or `claude`). `None` means a
    /// configured, unmanaged command; callers must never infer this from argv.
    pub agent_kind: Option<String>,
    /// Card task to submit after a managed agent becomes interactive. `None`
    /// for configured harnesses, which receive `BOARD_PROMPT` in `env`.
    pub initial_prompt: Option<String>,
    /// Authoritative system instructions for a managed agent, transported
    /// separately from startup argv. `None` for configured harnesses, which
    /// receive `BOARD_SYSTEM_PROMPT` in `env`.
    pub system_prompt: Option<String>,
    /// Fallback agent name to retry with when `name` is already taken (herdr
    /// agent names are exclusive while a pane using one is open). `None` skips
    /// the retry.
    pub name_fallback: Option<String>,
    /// herdr tab label to place the agent pane in (find-or-create + grid
    /// layout). `None` = no tab placement (cwd spaces / `LocalSpawner`, which
    /// ignore it). Only honored when `workspace_ref` is also set.
    pub tab_label: Option<String>,
    /// Working directory (resolved workspace cwd, or `LocalSpawner`).
    pub cwd: Option<PathBuf>,
    /// herdr workspace id (for `workspace` / `new_workspace` spaces).
    pub workspace_ref: Option<String>,
    /// herdr socket to spawn on, resolved from the card's session. `None` =
    /// the spawner's default socket (`LocalSpawner` ignores it).
    pub herdr_socket: Option<PathBuf>,
    /// Environment pairs to set on the child.
    pub env: Vec<(String, String)>,
    /// The command line.
    pub argv: Vec<String>,
}

/// A handle to a launched process: herdr ids and/or a local pid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeHandle {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pid: Option<u32>,
    /// herdr socket this pane lives on (its session), so kill/liveness target
    /// the right session after a daemon restart. `None` = default socket.
    pub herdr_socket: Option<PathBuf>,
}

/// Launch, kill, and liveness-check agent processes.
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle>;
    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()>;
    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool>;
}

const HERDR_PROTOCOL: u32 = 17;
const AGENT_START_TIMEOUT_MS: u64 = 30_000;
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_BACKOFF: Duration = Duration::from_millis(100);
const IMMEDIATE_READINESS_PROBES: usize = 3;

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
fn materialize_local_argv(req: &HerdrLaunchPlan) -> anyhow::Result<Vec<String>> {
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

// ---------------------------------------------------------------------------
// HerdrSpawner
// ---------------------------------------------------------------------------

/// Injectable bridge for configured harnesses. Keeping the CLI boundary here
/// lets tests verify the exact shell-free invocation.
trait PaneRunner: Send + Sync {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()>;
}

#[derive(Debug, Default)]
struct HerdrCliPaneRunner;

impl PaneRunner for HerdrCliPaneRunner {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()> {
        let herdr_bin = std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "herdr".to_string());
        let status = Command::new(herdr_bin)
            .args(argv)
            .env("HERDR_SOCKET_PATH", socket)
            .status()
            .context("invoking herdr pane run")?;
        if !status.success() {
            bail!("herdr pane run exited with status {status}");
        }
        Ok(())
    }
}

/// Launches managed agents through protocol-17 `agent.start`, and configured
/// harnesses through a board-owned pane plus `herdr pane run`.
///
/// Every operation opens a client bound to the run's selected socket. Handles
/// retain the run's explicit socket override so kill/liveness stay in-session.
#[derive(Clone)]
pub struct HerdrSpawner {
    socket: PathBuf,
    pane_runner: Arc<dyn PaneRunner>,
}

impl HerdrSpawner {
    pub fn new(socket: PathBuf) -> HerdrSpawner {
        HerdrSpawner {
            socket,
            pane_runner: Arc::new(HerdrCliPaneRunner),
        }
    }

    #[cfg(test)]
    fn with_pane_runner(socket: PathBuf, pane_runner: Arc<dyn PaneRunner>) -> HerdrSpawner {
        HerdrSpawner {
            socket,
            pane_runner,
        }
    }

    /// Open a client on `socket` (the run's session), else the default socket.
    fn client_for(&self, socket: Option<&Path>) -> anyhow::Result<HerdrClient> {
        let target = socket.unwrap_or(&self.socket);
        HerdrClient::connect(target).map_err(|error| {
            let message = error.to_string();
            anyhow::Error::new(error).context(format!("herdr unavailable: {message}"))
        })
    }

    fn selected_socket<'a>(&'a self, req: &'a HerdrLaunchPlan) -> &'a Path {
        req.herdr_socket.as_deref().unwrap_or(&self.socket)
    }
}

impl Spawner for HerdrSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        let selected_socket = self.selected_socket(req).to_path_buf();
        let mut client = self.client_for(Some(&selected_socket))?;

        // This must be the first protocol call: no placement or external
        // runner action is allowed against an incompatible socket.
        client.require_protocol(HERDR_PROTOCOL).map_err(|error| {
            let message = error.to_string();
            anyhow::Error::new(error).context(format!("checking Herdr protocol: {message}"))
        })?;

        let workspace_id = req
            .workspace_ref
            .as_deref()
            .ok_or_else(|| anyhow!("Herdr spawn requires workspace_ref for pane placement"))?;
        let tab_label = req
            .tab_label
            .as_deref()
            .ok_or_else(|| anyhow!("Herdr spawn requires tab_label for pane placement"))?;
        let env: BTreeMap<String, String> = req.env.iter().cloned().collect();

        let mut last_placement_race = None;
        for attempt in 0..2 {
            let owned = match allocate_owned_pane(
                &mut client,
                workspace_id,
                tab_label,
                req.cwd.as_deref(),
                &env,
            )
            .with_context(|| format!("placing pane in tab '{tab_label}' for {}", req.name))
            {
                Ok(owned) => owned,
                Err(error) if attempt == 0 && is_retryable_placement_race(&error) => {
                    last_placement_race = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };

            let launch_result = match req.agent_kind.as_deref() {
                Some(kind) => launch_managed(&mut client, req, kind, &owned.pane_id),
                None => launch_configured(
                    &mut client,
                    self.pane_runner.as_ref(),
                    &selected_socket,
                    req,
                    &owned.pane_id,
                ),
            };

            match launch_result {
                Ok(()) => {
                    return Ok(RuntimeHandle {
                        pane_id: Some(owned.pane_id),
                        workspace_id: Some(owned.workspace_id),
                        pid: None,
                        herdr_socket: req.herdr_socket.clone(),
                    });
                }
                Err(error) if attempt == 0 && is_retryable_placement_race(&error) => {
                    if let Err(cleanup_error) = close_owned_for_retry(&mut client, &owned.pane_id) {
                        return Err(error.context(format!(
                            "additionally failed to clean up board-owned pane {} before placement retry: {cleanup_error:#}",
                            owned.pane_id
                        )));
                    }
                    last_placement_race = Some(error);
                }
                Err(error) => {
                    return Err(close_owned_after_error(&mut client, &owned.pane_id, error));
                }
            }
        }

        Err(last_placement_race
            .unwrap_or_else(|| anyhow!("pane placement retry exhausted without a terminal result")))
    }

    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()> {
        if let Some(pane) = &h.pane_id {
            let mut client = self.client_for(h.herdr_socket.as_deref())?;
            client
                .pane_close(pane)
                .with_context(|| format!("herdr pane.close {pane}"))?;
        }
        Ok(())
    }

    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool> {
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
// Pane-first placement
// ---------------------------------------------------------------------------

const ERR_PANE_NOT_FOUND: &str = "pane_not_found";
const ERR_EMPTY_TAB: &str = "empty_tab";
const ERR_EMPTY_LAYOUT: &str = "empty_layout";

#[derive(Debug)]
struct OwnedPane {
    pane_id: String,
    workspace_id: String,
}

/// Find/create the board tab, then consume its root pane or split an explicitly
/// selected existing pane. The caller owns the single bounded full-placement
/// retry, so a race at any discovery step restarts from `tab.list`.
fn allocate_owned_pane(
    client: &mut HerdrClient,
    workspace_id: &str,
    label: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<OwnedPane> {
    let cwd = cwd.map(|path| path.to_string_lossy().into_owned());
    let tabs = client
        .tab_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?;
    let existing = tabs
        .iter()
        .filter(|tab| tab.label == label)
        .min_by_key(|tab| tab.number);

    let Some(tab) = existing else {
        let created = client
            .tab_create(&TabCreateParams {
                workspace_id: Some(workspace_id.to_string()),
                cwd,
                label: Some(label.to_string()),
                env: env.clone(),
                focus: false,
            })
            .map_err(anyhow::Error::new)?;
        return Ok(OwnedPane {
            pane_id: created.root_pane.pane_id,
            workspace_id: created.tab.workspace_id,
        });
    };

    let panes: Vec<_> = client
        .pane_list(Some(workspace_id))
        .map_err(mark_retryable_placement_race)?
        .into_iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .collect();
    let anchor = panes.first().ok_or_else(|| {
        mark_retryable_placement_race(HerdrError::Protocol {
            code: ERR_EMPTY_TAB.to_string(),
            message: format!("existing tab {} has no pane available to split", tab.tab_id),
        })
    })?;
    let layout = client
        .pane_layout(Some(&anchor.pane_id))
        .map_err(mark_retryable_placement_race)?;
    let (target_pane_id, direction) =
        grid_slot_result(&layout.panes).map_err(mark_retryable_placement_race)?;
    let pane = client
        .pane_split(&PaneSplitParams {
            workspace_id: Some(workspace_id.to_string()),
            target_pane_id,
            cwd,
            env: env.clone(),
            direction,
            focus: false,
        })
        .map_err(mark_retryable_placement_race)?;
    Ok(OwnedPane {
        pane_id: pane.pane_id,
        workspace_id: if pane.workspace_id.is_empty() {
            workspace_id.to_string()
        } else {
            pane.workspace_id
        },
    })
}

fn grid_slot_result(panes: &[LayoutPane]) -> Result<(String, SplitDirection), HerdrError> {
    if panes.is_empty() {
        return Err(HerdrError::Protocol {
            code: ERR_EMPTY_LAYOUT.to_string(),
            message: "existing tab layout has no pane available to split".to_string(),
        });
    }
    Ok(grid_slot(panes))
}

/// Choose the largest pane and a roughly-square split direction.
pub fn grid_slot(panes: &[LayoutPane]) -> (String, SplitDirection) {
    let Some(target) = panes
        .iter()
        .max_by_key(|pane| pane.rect.width.saturating_mul(pane.rect.height))
    else {
        // The public helper predates fallible placement. Production checks the
        // precondition in `grid_slot_result`; retain a non-panicking fallback.
        return (String::new(), SplitDirection::Down);
    };
    let direction = if target.rect.width >= 2_u64.saturating_mul(target.rect.height) {
        SplitDirection::Right
    } else {
        SplitDirection::Down
    };
    (target.pane_id.clone(), direction)
}

// ---------------------------------------------------------------------------
// Managed protocol-17 launch
// ---------------------------------------------------------------------------

const ERR_AGENT_NAME_TAKEN: &str = "agent_name_taken";

/// Marks placement disappearance only at operations where restarting the
/// complete placement is safe. Keeping `HerdrError` as the source preserves
/// its typed protocol code in the anyhow chain.
#[derive(Debug)]
struct RetryablePlacementRace(HerdrError);

impl std::fmt::Display for RetryablePlacementRace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RetryablePlacementRace {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn launch_managed(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    kind: &str,
    pane_id: &str,
) -> anyhow::Result<()> {
    let flag = match kind {
        "pi" => "--append-system-prompt",
        "claude" => "--append-system-prompt-file",
        other => bail!("unsupported managed harness kind: {other}"),
    };
    let system_prompt = req
        .system_prompt
        .as_deref()
        .ok_or_else(|| anyhow!("managed {kind} invocation is missing system_prompt metadata"))?;
    let (_, startup_tail) = req
        .argv
        .split_first()
        .ok_or_else(|| anyhow!("managed {kind} invocation has empty startup argv"))?;

    let mut prompt_file = tempfile::Builder::new()
        .prefix("herdr-board-system-")
        .tempfile()
        .context("creating managed system-prompt file")?;
    fs::set_permissions(prompt_file.path(), fs::Permissions::from_mode(0o600))
        .context("setting managed system-prompt file mode to 0600")?;
    prompt_file
        .write_all(system_prompt.as_bytes())
        .context("writing managed system-prompt file")?;
    prompt_file
        .flush()
        .context("flushing managed system-prompt file")?;
    let prompt_path = prompt_file
        .path()
        .to_str()
        .ok_or_else(|| anyhow!("managed system-prompt path is not valid UTF-8"))?
        .to_string();

    let mut args = startup_tail.to_vec();
    args.extend([flag.to_string(), prompt_path]);
    let params = AgentStartParams {
        name: req.name.clone(),
        kind: kind.to_string(),
        pane_id: pane_id.to_string(),
        args,
        timeout_ms: Some(AGENT_START_TIMEOUT_MS),
    };

    let operation = (|| -> anyhow::Result<()> {
        let started = agent_start_retry_name(client, &params, req.name_fallback.as_deref())
            .map_err(|error| {
                let message = error.to_string();
                let typed = if matches!(
                    &error,
                    HerdrError::Protocol { code, .. } if code == ERR_PANE_NOT_FOUND
                ) {
                    anyhow::Error::new(RetryablePlacementRace(error))
                } else {
                    anyhow::Error::new(error)
                };
                typed.context(format!("herdr agent.start for {}: {message}", req.name))
            })?;
        await_interactive_ready(client, &started)?;
        if let Some(text) = &req.initial_prompt {
            client
                .agent_prompt(&AgentPromptParams {
                    target: pane_id.to_string(),
                    text: text.clone(),
                    wait: None,
                })
                .with_context(|| format!("herdr agent.prompt for {}", req.name))?;
        }
        Ok(())
    })();

    let remove_result = prompt_file
        .close()
        .context("removing managed system-prompt file");
    match (operation, remove_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(remove_error)) => Err(remove_error),
        (Err(error), Err(remove_error)) => Err(error.context(format!(
            "additionally failed to remove system-prompt file: {remove_error:#}"
        ))),
    }
}

fn agent_start_retry_name(
    client: &mut HerdrClient,
    params: &AgentStartParams,
    fallback: Option<&str>,
) -> Result<AgentStarted, HerdrError> {
    match client.agent_start(params) {
        Err(HerdrError::Protocol { code, message }) if code == ERR_AGENT_NAME_TAKEN => {
            if let Some(name) = fallback {
                let mut retry = params.clone();
                retry.name = name.to_string();
                client.agent_start(&retry)
            } else {
                Err(HerdrError::Protocol { code, message })
            }
        }
        result => result,
    }
}

fn is_interactive(agent: &AgentInfo) -> bool {
    agent.interactive_ready && !agent.launch_pending
}

fn await_interactive_ready(client: &mut HerdrClient, started: &AgentStarted) -> anyhow::Result<()> {
    if is_interactive(&started.agent) {
        return Ok(());
    }

    let pane_id = started.pane_id();
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut probes = 0_usize;
    loop {
        // Probe immediately several times. Protocol/socket fixtures generally
        // transition synchronously, and this avoids wall sleeps in unit tests.
        let agent = client
            .agent_get(pane_id)
            .with_context(|| format!("herdr agent.get while waiting for {pane_id}"))?;
        if is_interactive(&agent) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for managed agent in pane {pane_id} to become interactive");
        }
        probes += 1;
        if probes >= IMMEDIATE_READINESS_PROBES {
            thread::sleep(
                READINESS_BACKOFF.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Configured harness runner bridge
// ---------------------------------------------------------------------------

fn launch_configured(
    client: &mut HerdrClient,
    runner: &dyn PaneRunner,
    socket: &Path,
    req: &HerdrLaunchPlan,
    pane_id: &str,
) -> anyhow::Result<()> {
    if req.argv.is_empty() {
        bail!("configured harness has empty argv");
    }
    client
        .pane_rename(&PaneRenameParams {
            pane_id: pane_id.to_string(),
            label: req.name.clone(),
        })
        .map_err(mark_retryable_placement_race)
        .with_context(|| format!("herdr pane.rename {pane_id}"))?;

    let mut script = tempfile::Builder::new()
        .prefix("herdr-board-run-")
        .tempfile()
        .context("creating configured-harness startup script")?;
    let script_path = script.path().to_path_buf();
    let script_text = configured_script(&script_path, &req.argv);
    script
        .write_all(script_text.as_bytes())
        .context("writing configured-harness startup script")?;
    script
        .flush()
        .context("flushing configured-harness startup script")?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
        .context("setting configured-harness startup script mode to 0700")?;
    // Close the writer before the pane executes this file (Linux rejects an
    // open-for-write executable with ETXTBSY). `keep` transfers cleanup to the
    // script after runner success, or back to the daemon after runner failure.
    let (script_file, script_path) = script
        .keep()
        .context("persisting configured-harness startup script")?;
    drop(script_file);

    let runner_argv = vec![
        "pane".to_string(),
        "run".to_string(),
        pane_id.to_string(),
        script_path.to_string_lossy().into_owned(),
    ];
    let run_result = runner
        .run(socket, &runner_argv)
        .map_err(mark_retryable_runner_race)
        .map_err(|error| {
            let message = format!("{error:#}");
            error.context(format!("herdr pane run {pane_id}: {message}"))
        });

    match run_result {
        Ok(()) => {
            // `pane run` only schedules the command; the pane may not have
            // opened the script when the runner returns. Its first command is
            // therefore the sole owner of successful-launch cleanup. No fixed
            // daemon deadline can safely unlink a scheduled-but-not-yet-opened
            // script, and an unbounded sleeping reaper thread is unacceptable.
            // If the pane never opens it, an orphan is the unavoidable side of
            // this scheduling boundary.
            Ok(())
        }
        Err(error) => {
            let remove_result = remove_file_if_exists(&script_path)
                .context("removing configured-harness startup script after runner failure");
            match remove_result {
                Ok(()) => Err(error),
                Err(remove_error) => Err(error.context(format!(
                    "additionally failed to remove startup script: {remove_error:#}"
                ))),
            }
        }
    }
}

fn configured_script(path: &Path, argv: &[String]) -> String {
    let mut script = String::from("#!/bin/sh\nrm -f -- ");
    script.push_str(&posix_quote(&path.to_string_lossy()));
    script.push('\n');
    for arg in argv {
        script.push_str(&posix_quote(arg));
        script.push(' ');
    }
    script.push_str("\nchild_status=$?\n");
    script.push_str("if [ -n \"${BOARD_BIN:-}\" ]; then\n");
    script.push_str("\t\"$BOARD_BIN\" __pane-exited --run-id \"$BOARD_RUN_ID\" || :\n");
    script.push_str("fi\nexit \"$child_status\"\n");
    script
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn mark_retryable_placement_race(error: HerdrError) -> anyhow::Error {
    if is_placement_disappearance(&error) {
        anyhow::Error::new(RetryablePlacementRace(error))
    } else {
        anyhow::Error::new(error)
    }
}

fn is_retryable_placement_race(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<RetryablePlacementRace>().is_some()
            || cause
                .downcast_ref::<RetryableRunnerPlacementRace>()
                .is_some()
    })
}

fn mark_retryable_runner_race(error: anyhow::Error) -> anyhow::Error {
    let pane_disappeared = error.chain().any(|cause| {
        cause
            .downcast_ref::<HerdrError>()
            .is_some_and(is_pane_not_found)
    });
    if pane_disappeared {
        anyhow::Error::new(RetryableRunnerPlacementRace(error))
    } else {
        error
    }
}

#[derive(Debug)]
struct RetryableRunnerPlacementRace(anyhow::Error);

impl std::fmt::Display for RetryableRunnerPlacementRace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RetryableRunnerPlacementRace {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

fn is_placement_disappearance(error: &HerdrError) -> bool {
    matches!(
        error,
        HerdrError::Protocol { code, .. }
            if matches!(code.as_str(), ERR_PANE_NOT_FOUND | ERR_EMPTY_TAB | ERR_EMPTY_LAYOUT)
    )
}

fn is_pane_not_found(error: &HerdrError) -> bool {
    matches!(
        error,
        HerdrError::Protocol { code, .. } if code == ERR_PANE_NOT_FOUND
    )
}

fn close_owned_for_retry(client: &mut HerdrClient, pane_id: &str) -> anyhow::Result<()> {
    match client.pane_close(pane_id) {
        Ok(()) => Ok(()),
        Err(error) if is_pane_not_found(&error) => Ok(()),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("herdr pane.close board-owned pane {pane_id}"))),
    }
}

fn close_owned_after_error(
    client: &mut HerdrClient,
    pane_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match client.pane_close(pane_id) {
        Ok(()) => error,
        Err(cleanup_error) if is_pane_not_found(&cleanup_error) => error,
        Err(cleanup_error) => error.context(format!(
            "additionally failed to close board-owned pane {pane_id}: {cleanup_error}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::{grid_slot, materialize_local_argv, HerdrCliPaneRunner, HerdrSpawner, PaneRunner};
    use crate::spawner::{HerdrLaunchPlan, Spawner};
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
                    super::posix_quote(&path.to_string_lossy())
                );
                if script.starts_with(&expected_header) {
                    let _ = super::remove_file_if_exists(&path);
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

    fn serve_recording_herdr_with_ping<F>(
        handler: F,
        version: &str,
        protocol: u32,
    ) -> RecordingHerdr
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

    fn agent_get_result(
        req: &Value,
        pane_id: &str,
        name: &str,
        pending: bool,
        ready: bool,
    ) -> Value {
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

    #[test]
    fn configured_rename_pane_race_ignores_vanished_cleanup_and_retries_allocation() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let allocations = Arc::new(AtomicUsize::new(0));
        let allocations2 = Arc::clone(&allocations);
        let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => {
                let pane_id = if allocations2.fetch_add(1, Ordering::SeqCst) == 0 {
                    "w1:p-rename-race-first"
                } else {
                    "w1:p-rename-race-second"
                };
                tab_created(req, pane_id)
            }
            "pane.rename" => {
                if req["params"]["pane_id"] == "w1:p-rename-race-first" {
                    error(req, "pane_not_found", "owned pane vanished during rename")
                } else {
                    pane_result(req, "w1:p-rename-race-second")
                }
            }
            "pane.close" => error(req, "pane_not_found", "owned pane already vanished"),
            method => panic!("unexpected configured rename race method {method}"),
        });
        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner = RecordingPaneRunner {
            calls,
            behavior: Box::new(move |_, argv| {
                assert_eq!(argv[2], "w1:p-rename-race-second");
                Ok(())
            }),
        };
        let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));
        let cwd = tempfile::tempdir().unwrap();

        let handle = spawner
            .spawn(&custom_req(
                fake.socket.clone(),
                cwd.path().to_path_buf(),
                vec!["configured-agent".into()],
            ))
            .unwrap();
        assert_eq!(handle.pane_id.as_deref(), Some("w1:p-rename-race-second"));
        let requests = fake.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "tab.create")
                .count(),
            2,
            "rename disappearance must restart allocation rather than reuse stale ownership"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["method"] == "pane.close")
                .map(|request| request["params"]["pane_id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["w1:p-rename-race-first"],
            "cleanup may target only the first board-owned pane"
        );
    }

    #[test]
    fn configured_runner_pane_not_found_retries_but_generic_runner_error_is_terminal() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let allocations = Arc::new(AtomicUsize::new(0));
        let allocations2 = Arc::clone(&allocations);
        let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => {
                let pane_id = if allocations2.fetch_add(1, Ordering::SeqCst) == 0 {
                    "w1:p-runner-race-first"
                } else {
                    "w1:p-runner-race-second"
                };
                tab_created(req, pane_id)
            }
            "pane.rename" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
            "pane.close" => error(req, "pane_not_found", "runner observed vanished pane"),
            method => panic!("unexpected configured runner race method {method}"),
        });
        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner_calls = Arc::new(AtomicUsize::new(0));
        let runner_calls2 = Arc::clone(&runner_calls);
        let runner = RecordingPaneRunner {
            calls,
            behavior: Box::new(move |_, argv| {
                let call = runner_calls2.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    assert_eq!(argv[2], "w1:p-runner-race-first");
                    Err(anyhow::Error::new(board_herdr::HerdrError::Protocol {
                        code: "pane_not_found".into(),
                        message: "CLI pane disappeared after scheduling".into(),
                    }))
                } else {
                    assert_eq!(argv[2], "w1:p-runner-race-second");
                    Ok(())
                }
            }),
        };
        let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));
        let cwd = tempfile::tempdir().unwrap();
        let handle = spawner
            .spawn(&custom_req(
                fake.socket.clone(),
                cwd.path().to_path_buf(),
                vec!["configured-agent".into()],
            ))
            .unwrap();
        assert_eq!(handle.pane_id.as_deref(), Some("w1:p-runner-race-second"));
        assert_eq!(runner_calls.load(Ordering::SeqCst), 2);

        let generic_fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => tab_created(req, "w1:p-generic-terminal"),
            "pane.rename" => pane_result(req, "w1:p-generic-terminal"),
            "pane.close" => pane_result(req, "w1:p-generic-terminal"),
            method => panic!("unexpected generic runner method {method}"),
        });
        let generic_calls = Arc::new(AtomicUsize::new(0));
        let generic_calls2 = Arc::clone(&generic_calls);
        let generic_runner = RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(move |_, _| {
                generic_calls2.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("runner crashed generically"))
            }),
        };
        let generic_spawner =
            HerdrSpawner::with_pane_runner(generic_fake.socket.clone(), Arc::new(generic_runner));
        let generic_err = generic_spawner
            .spawn(&custom_req(
                generic_fake.socket.clone(),
                cwd.path().to_path_buf(),
                vec!["configured-agent".into()],
            ))
            .unwrap_err();
        assert!(generic_err
            .to_string()
            .contains("runner crashed generically"));
        assert_eq!(generic_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            generic_fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request["method"] == "tab.create")
                .count(),
            1,
            "generic runner failures must remain terminal"
        );
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

    #[test]
    fn configured_pane_runner_resolves_herdr_bin_path_without_live_herdr() {
        // Run the real CLI runner in a child test process so HERDR_BIN_PATH is
        // configured for that process only. The empty PATH makes a hardcoded
        // `herdr` lookup fail rather than accidentally invoking a live Herdr.
        const CHILD_MARKER: &str = "HB_SPAWNER_BIN_PATH_TEST_CHILD";
        const CHILD_SOCKET: &str = "HB_SPAWNER_BIN_PATH_TEST_SOCKET";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let socket = PathBuf::from(std::env::var_os(CHILD_SOCKET).unwrap());
            HerdrCliPaneRunner
                .run(&socket, &["pane".into(), "run".into(), "w1:p-bin".into()])
                .unwrap();
            return;
        }

        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let recorder = dir.path().join("herdr-bin-recorder.sh");
        let invocation = dir.path().join("invocation");
        let socket = dir.path().join("selected.sock");
        let empty_path = dir.path().join("empty-path");
        std::fs::create_dir(&empty_path).unwrap();
        std::fs::write(
            &recorder,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf '%s' \"${{HERDR_SOCKET_PATH:-}}\" > {}\n",
                super::posix_quote(&invocation.to_string_lossy()),
                super::posix_quote(&dir.path().join("socket").to_string_lossy()),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();

        let status = Command::new(std::env::current_exe().unwrap())
            .arg("configured_pane_runner_resolves_herdr_bin_path_without_live_herdr")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env(CHILD_SOCKET, &socket)
            .env("HERDR_BIN_PATH", &recorder)
            .env("PATH", &empty_path)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "the configured PaneRunner must execute HERDR_BIN_PATH, not literal `herdr`"
        );

        let args = std::fs::read(&invocation)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args, ["pane", "run", "w1:p-bin"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("socket")).unwrap(),
            socket.to_string_lossy()
        );
    }

    #[test]
    fn configured_harness_uses_selected_socket_pane_run_with_exact_payload() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let command_dir = tempfile::tempdir().unwrap();
        let cwd = command_dir.path().join("custom cwd with spaces");
        std::fs::create_dir(&cwd).unwrap();
        let recorder = command_dir.path().join("custom command's recorder.py");
        let capture = command_dir.path().join("captured invocation.json");
        std::fs::write(
            &recorder,
            format!(
                "#!/usr/bin/env python3\nimport json, os, sys\nkeys = ['BOARD_PROMPT', 'BOARD_SYSTEM_PROMPT', 'HERDR_SOCKET_PATH']\njson.dump({{'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': {{k: os.environ[k] for k in keys}}}}, open({:?}, 'w'))\n",
                capture
            ),
        )
        .unwrap();
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let exact_argv = vec![
            recorder.to_string_lossy().into_owned(),
            "single'quote".into(),
            "literal argument with spaces".into(),
            "line one\nline two".into(),
        ];

        let selected = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => tab_created(req, "w1:p9"),
            "pane.rename" => pane_result(req, "w1:p9"),
            method => panic!("configured harness must not call managed/send-text method {method}"),
        });
        let default = serve_recording_herdr(|req, _| {
            panic!("request incorrectly used default socket: {req}")
        });

        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner_path = Arc::new(Mutex::new(None::<PathBuf>));
        let runner_path2 = Arc::clone(&runner_path);
        let runner_socket = selected.socket.clone();
        let runner = RecordingPaneRunner {
            calls: Arc::clone(&calls),
            behavior: Box::new(move |socket, argv| {
                assert_eq!(socket, runner_socket.as_path());
                assert_eq!(&argv[..3], ["pane", "run", "w1:p9"]);
                assert_eq!(
                    argv.len(),
                    4,
                    "script path must be one shell-free argv item"
                );
                let path = PathBuf::from(&argv[3]);
                assert_eq!(
                    std::fs::metadata(&path)?.permissions().mode() & 0o777,
                    0o700,
                    "startup script must be executable only by its owner",
                );
                *runner_path2.lock().unwrap() = Some(path.clone());

                // The configured runner only schedules the script. The pane
                // process opens it after the runner returns.
                Ok(())
            }),
        };
        let spawner = HerdrSpawner::with_pane_runner(default.socket.clone(), Arc::new(runner));

        let handle = spawner
            .spawn(&custom_req(
                selected.socket.clone(),
                cwd.clone(),
                exact_argv.clone(),
            ))
            .unwrap();
        assert_eq!(handle.pane_id.as_deref(), Some("w1:p9"));
        assert_eq!(
            handle.herdr_socket.as_deref(),
            Some(selected.socket.as_path())
        );

        let path = runner_path.lock().unwrap().clone().unwrap();
        let call = calls.lock().unwrap().clone();
        assert_eq!(
            call,
            [PaneRunCall {
                socket: selected.socket.clone(),
                argv: vec![
                    "pane".into(),
                    "run".into(),
                    "w1:p9".into(),
                    path.to_string_lossy().into_owned(),
                ],
            }],
            "exactly one CLI call must target the selected session and transport one script path",
        );
        assert!(
            path.exists(),
            "runner success must return before the pane opens the startup script"
        );

        // Simulate the selected pane opening the script after pane.run has
        // returned, including the pane's cwd and environment.
        let status = Command::new(&path)
            .current_dir(&cwd)
            .env(
                "BOARD_PROMPT",
                "configured task line one\nconfigured task line two",
            )
            .env(
                "BOARD_SYSTEM_PROMPT",
                "configured system line one\nconfigured system line two",
            )
            .env("HERDR_SOCKET_PATH", selected.socket.as_path())
            .status()
            .unwrap();
        assert!(status.success(), "fake pane launch payload failed");
        assert!(
            !path.exists(),
            "startup script must self-remove when the pane opens it"
        );

        let recorded: Value =
            serde_json::from_str(&std::fs::read_to_string(capture).unwrap()).unwrap();
        assert_eq!(recorded["argv"], serde_json::json!(exact_argv[1..]));
        assert_eq!(recorded["cwd"], cwd.to_string_lossy().as_ref());
        assert_eq!(
            recorded["env"],
            serde_json::json!({
                "BOARD_PROMPT": "configured task line one\nconfigured task line two",
                "BOARD_SYSTEM_PROMPT": "configured system line one\nconfigured system line two",
                "HERDR_SOCKET_PATH": selected.socket.to_string_lossy(),
            }),
            "configured payload must receive exact multiline prompt/system env and selected socket",
        );

        let requests = selected.requests.lock().unwrap();
        let methods: Vec<_> = requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect();
        assert_eq!(methods[..3], ["ping", "tab.list", "tab.create"]);
        assert_eq!(
            requests[2]["params"],
            serde_json::json!({
                "workspace_id": "w1",
                "label": "kanban",
                "cwd": cwd.to_string_lossy(),
                "env": {
                    "BOARD_PROMPT": "configured task line one\nconfigured task line two",
                    "BOARD_SYSTEM_PROMPT": "configured system line one\nconfigured system line two",
                    "HERDR_SOCKET_PATH": selected.socket.to_string_lossy(),
                },
                "focus": false,
            }),
            "tab placement establishes the configured child cwd and environment",
        );
        assert!(requests.iter().all(|r| {
            !matches!(
                r["method"].as_str(),
                Some("agent.start" | "pane.send_text" | "pane.send_keys")
            )
        }));
        assert!(default.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn recording_runner_drop_removes_only_recorded_startup_scripts() {
        let selected = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
            "tab.list" => empty_tab_list(req),
            "tab.create" => tab_created(req, "w1:p-drop-cleanup"),
            "pane.rename" => pane_result(req, "w1:p-drop-cleanup"),
            method => panic!("unexpected configured-runner method {method}"),
        });
        let cwd = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner = RecordingPaneRunner {
            calls: Arc::clone(&calls),
            behavior: Box::new(|_, _| Ok(())),
        };
        let spawner = HerdrSpawner::with_pane_runner(selected.socket.clone(), Arc::new(runner));

        spawner
            .spawn(&custom_req(
                selected.socket.clone(),
                cwd.path().to_path_buf(),
                vec!["configured-agent".into()],
            ))
            .unwrap();
        let recorded_path = PathBuf::from(&calls.lock().unwrap()[0].argv[3]);
        let decoy_path = recorded_path.with_file_name("herdr-board-run-decoy");
        std::fs::write(&decoy_path, "not a configured startup script\n").unwrap();
        assert!(recorded_path.exists());

        drop(spawner);

        assert!(
            !recorded_path.exists(),
            "dropping the recording runner must remove its unexecuted script"
        );
        assert!(
            decoy_path.exists(),
            "cleanup must not remove an unrecorded same-prefix file"
        );
        std::fs::remove_file(&decoy_path).unwrap();
        assert!(
            !decoy_path.exists(),
            "the test must leave no decoy artifact"
        );
    }

    #[test]
    fn configured_script_runs_child_then_reports_silent_exit_and_preserves_status() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child with spaces.sh");
        let child_capture = dir.path().join("child-argv");
        std::fs::write(
            &child,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nexit 23\n",
                super::posix_quote(&child_capture.to_string_lossy())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
        let board_bin = dir.path().join("board bin recorder.sh");
        let board_capture = dir.path().join("board-argv");
        std::fs::write(
            &board_bin,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nexit 1\n",
                super::posix_quote(&board_capture.to_string_lossy())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&board_bin, std::fs::Permissions::from_mode(0o700)).unwrap();

        let script_path = dir.path().join("startup script");
        let exact_argv = vec![
            child.to_string_lossy().into_owned(),
            "argument with spaces".into(),
            "line one\nline two".into(),
            "single'quote".into(),
        ];
        std::fs::write(
            &script_path,
            super::configured_script(&script_path, &exact_argv),
        )
        .unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();

        let status = Command::new(&script_path)
            .env("BOARD_BIN", &board_bin)
            .env("BOARD_CARD_ID", "card-42")
            .env("BOARD_RUN_ID", "run-42")
            .env("CHILD_CAPTURE", &child_capture)
            .env("BOARD_CAPTURE", &board_capture)
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(23),
            "the child status must be preserved"
        );
        assert!(
            !script_path.exists(),
            "the startup script removes itself first"
        );

        let nul_args = |path: &std::path::Path| {
            std::fs::read(path)
                .unwrap()
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(&nul_args(&child_capture), &exact_argv[1..]);
        assert_eq!(
            nul_args(&board_capture),
            vec!["__pane-exited", "--run-id", "run-42"]
        );
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
        let default = serve_recording_herdr(|req, _| {
            panic!("request incorrectly used default socket: {req}")
        });
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

    #[test]
    fn local_materializer_preserves_pi_historic_prompt_flag_and_card_argument() {
        let argv = materialize_local_argv(&managed_req("pi")).unwrap();
        assert_eq!(
            argv,
            vec![
                "pi",
                "--model",
                "m",
                "--append-system-prompt",
                "old system\nsecond line",
                "--session-id",
                "s",
                "Card task:\nexact task",
            ]
        );
    }

    #[test]
    fn local_materializer_preserves_claude_flag_order_and_final_prompt() {
        let argv = materialize_local_argv(&managed_req("claude")).unwrap();
        assert_eq!(
            argv,
            vec![
                "claude",
                "--model",
                "m",
                "--append-system-prompt",
                "old system\nsecond line",
                "--allowedTools",
                "Bash(*)",
                "--",
                "exact task",
            ]
        );
    }

    #[test]
    fn local_materializer_leaves_configured_argv_untouched() {
        let mut req = managed_req("custom");
        req.agent_kind = None;
        req.initial_prompt = None;
        req.system_prompt = None;
        req.argv = vec!["configured".into(), "literal\nargument".into()];
        assert_eq!(materialize_local_argv(&req).unwrap(), req.argv);
    }

    #[test]
    fn local_materializer_rejects_incomplete_or_unknown_managed_metadata() {
        let mut missing = managed_req("pi");
        missing.system_prompt = None;
        let err = materialize_local_argv(&missing).unwrap_err();
        assert!(err.to_string().contains("system_prompt"));

        let err = materialize_local_argv(&managed_req("other")).unwrap_err();
        assert!(err.to_string().contains("managed") || err.to_string().contains("harness"));
    }
}
