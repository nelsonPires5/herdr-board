use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, bail, Context};
use board_herdr::{
    AgentInfo, AgentPromptParams, AgentStartParams, AgentStarted, HerdrClient, HerdrError,
    PaneRenameParams,
};

use super::placement::{
    allocate_owned_pane, close_owned_after_error, close_owned_for_retry,
    is_retryable_placement_race, mark_retryable_placement_race, mark_retryable_runner_race,
    RetryablePlacementRace, ERR_PANE_NOT_FOUND,
};
use super::{
    HerdrLaunchPlan, RuntimeHandle, Spawner, AGENT_START_TIMEOUT_MS, HERDR_PROTOCOL,
    IMMEDIATE_READINESS_PROBES, READINESS_BACKOFF, READINESS_TIMEOUT,
};

// ---------------------------------------------------------------------------
// PaneRunner bridge
// ---------------------------------------------------------------------------

/// Injectable bridge for configured harnesses. Keeping the CLI boundary here
/// lets tests verify the exact shell-free invocation.
pub(crate) trait PaneRunner: Send + Sync {
    fn run(&self, socket: &Path, argv: &[String]) -> anyhow::Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct HerdrCliPaneRunner;

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

// ---------------------------------------------------------------------------
// HerdrSpawner
// ---------------------------------------------------------------------------

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
    pub(crate) fn with_pane_runner(
        socket: PathBuf,
        pane_runner: Arc<dyn PaneRunner>,
    ) -> HerdrSpawner {
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
// Managed protocol-17 launch
// ---------------------------------------------------------------------------

const ERR_AGENT_NAME_TAKEN: &str = "agent_name_taken";

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

pub(crate) fn configured_script(path: &Path, argv: &[String]) -> String {
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

pub(crate) fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
