//! Managed Herdr launch: `agent.start` on a board-owned pane, with the
//! name/busy retry taxonomy, the two-stage readiness gate (terminal
//! interactive, then agent session identity for pi/claude and reuse), the
//! bounded `agent.prompt` busy retry, and delivery confirmation — plus, for
//! self-minting harnesses like codex, opencode and agy, the bounded capture
//! of the integration-reported conversation/session id, ordered per harness:
//! codex captures after readiness and before the prompt, opencode and agy
//! after the prompt (real OpenCode mints `agent_session` only once its first
//! prompt lands, and the agy integration reports its conversation id the same
//! way; a prompt-less rescue reduces to capture-after-readiness). Readiness
//! now has two stages: `interactive_ready && !launch_pending` (pane shell)
//! and `agent_session` presence (pi/claude CLI initialised); the prompt is
//! retried only on explicit `agent_pane_busy` with a bounded doubling backoff
//! and an inter-attempt `is_interactive` check, then confirmation polls
//! `agent.get` until the agent leaves `Idle` or its session appears/changes.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use board_herdr::{
    AgentInfo, AgentPromptParams, AgentStartParams, AgentStarted, AgentStatus, HerdrClient,
    HerdrError,
};

use super::super::placement::{RetryablePlacementRace, ERR_PANE_NOT_FOUND};
use super::super::{
    HerdrLaunchPlan, AGENT_PROMPT_BUSY_BACKOFF, AGENT_PROMPT_BUSY_RETRIES,
    AGENT_START_BUSY_BACKOFF, AGENT_START_BUSY_RETRIES, AGENT_START_TIMEOUT_MS,
    IMMEDIATE_READINESS_PROBES, PROMPT_CONFIRM_PROBES, PROMPT_CONFIRM_TIMEOUT, READINESS_BACKOFF,
    READINESS_TIMEOUT, SESSION_CAPTURE_PROBES, SESSION_CAPTURE_TIMEOUT, SESSION_READY_PROBES,
    SESSION_READY_TIMEOUT,
};

const ERR_AGENT_NAME_TAKEN: &str = "agent_name_taken";
const ERR_AGENT_PANE_BUSY: &str = "agent_pane_busy";

pub(crate) type DelayFn = dyn Fn(Duration) + Send + Sync;

/// The production `agent.start` busy-retry delay, for launch callers outside
/// `HerdrSpawner` (the run-pane rescue) that have no injected clock of their own.
pub(crate) const DEFAULT_AGENT_START_DELAY: &DelayFn = &(thread::sleep as fn(Duration));

/// Launch one managed agent and return the harness-captured conversation id
/// (self-minting harnesses like codex/opencode), or `None` when no capture
/// applies (pi/claude, same-pane reuse) or none validated.
///
/// The capture travels through the spawn handle so the daemon persists it
/// atomically with run promotion inside [`crate::dispatch::launch_plan::
/// register_spawned_run`]'s cancellation critical section — a cancel that
/// ended the run while the launch was in flight discards the captured id
/// together with the handle.
pub(crate) fn launch_managed(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    kind: &str,
    pane_id: &str,
    reuse: bool,
    delay: &DelayFn,
    socket: &Path,
) -> anyhow::Result<Option<String>> {
    // A same-conversation resume hop reuses the prior run's agent pane. The
    // conversation + agent are already live there, so there is nothing to
    // `agent.start`: the pane name is exclusive while open (re-starting would
    // collide with `agent_name_taken`), the startup system prompt was consumed
    // by the original start, and the persisted `--resume`/`--session-id` argv
    // is irrelevant (the conversation is in-process). Just wait for the agent
    // to finish the previous stage's turn (it may still be Working right after
    // `board done`) and deliver the new stage's prompt — task only, never a
    // system block, and nothing to capture (the conversation id is already
    // persisted on the card).
    if reuse {
        await_reuse_ready(client, pane_id)?;
        if let Some(text) = req.initial_prompt.clone() {
            let initial_session = await_session_ready(client, pane_id, delay);
            agent_prompt_with_busy_retry(client, pane_id, text, &req.name, delay)?;
            confirm_delivery_after_prompt(socket, client, pane_id, delay, initial_session);
        }
        return Ok(None);
    }

    match kind {
        // Self-minting harnesses (codex/opencode/agy) mint their own id and
        // have no system-prompt file: startup argv carries no prompt text,
        // and the reported id is captured through the same gated connection
        // after readiness — codex before the prompt, opencode and agy after
        // it (C5/C7/O7/A7).
        "codex" => launch_managed_codex(client, req, pane_id, delay, socket),
        "opencode" => launch_managed_opencode(client, req, pane_id, delay, socket),
        "agy" => launch_managed_agy(client, req, pane_id, delay, socket),
        // Pi/Claude keep the authoritative 0600 startup system-prompt file.
        "pi" | "claude" => {
            launch_managed_prompt_file(client, req, kind, pane_id, delay, socket)?;
            Ok(None)
        }
        other => bail!("unsupported managed harness kind: {other}"),
    }
}

/// A codex launch: `agent.start` with the startup-only argv (no prompt file,
/// no `--` delimiter), readiness polling, the bounded session capture, and
/// then the prompt — Mint receives one delimited `system + task` block,
/// resume/fork receive the task alone, a rescue sends nothing. The capture
/// runs before the prompt: the codex integration reports its thread id as
/// soon as the CLI is interactive.
fn launch_managed_codex(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    pane_id: &str,
    delay: &DelayFn,
    socket: &Path,
) -> anyhow::Result<Option<String>> {
    launch_managed_self_minting(
        client,
        req,
        pane_id,
        delay,
        socket,
        codex_prompt_text,
        capture_codex_session,
        CaptureTiming::BeforePrompt,
    )
}

/// An opencode launch: the same self-minting shape as codex — startup-only
/// argv, bounded session capture, and prompt delivery (Mint: delimited
/// system+task block; resume/fork: task alone; rescue: nothing) — with the
/// capture ordered AFTER the prompt: real OpenCode mints its `ses_…` id and
/// reports `agent_session` only once the first `agent.prompt` lands, so a
/// pre-prompt capture would lose the id (and with it the atomic promotion).
fn launch_managed_opencode(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    pane_id: &str,
    delay: &DelayFn,
    socket: &Path,
) -> anyhow::Result<Option<String>> {
    launch_managed_self_minting(
        client,
        req,
        pane_id,
        delay,
        socket,
        opencode_prompt_text,
        capture_opencode_session,
        CaptureTiming::AfterPrompt,
    )
}

/// An agy launch: the same self-minting shape as opencode — startup-only
/// argv, bounded session capture, and prompt delivery (Mint: delimited
/// system+task block; resume/retry: task alone; rescue: nothing) — with the
/// capture ordered AFTER the prompt: the agy integration reports its
/// conversation id via `agent_session` ({agent: agy, kind: id, source:
/// herdr:antigravity_cli, value}) once the CLI is up; capturing after the
/// prompt is strictly safer and matches opencode.
fn launch_managed_agy(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    pane_id: &str,
    delay: &DelayFn,
    socket: &Path,
) -> anyhow::Result<Option<String>> {
    launch_managed_self_minting(
        client,
        req,
        pane_id,
        delay,
        socket,
        agy_prompt_text,
        capture_agy_session,
        CaptureTiming::AfterPrompt,
    )
}

/// The `agent.prompt` text for an agy launch: Mint receives one clearly
/// delimited block with the system instructions first, then the card task;
/// resume/retry (fresh pane) receive the task alone; a rescue sends nothing
/// (`initial_prompt` was cleared by `resume_invocation`). Same-pane reuse is
/// handled by [`launch_managed`] before this point and delivers the task
/// alone.
fn agy_prompt_text(req: &HerdrLaunchPlan) -> Option<String> {
    let task = req.initial_prompt.as_deref()?;
    if is_agy_mint(&req.argv) {
        let system = req.system_prompt.as_deref().unwrap_or_default();
        Some(board_core::harness::agy::mint_prompt(system, task))
    } else {
        Some(task.to_string())
    }
}

/// Whether a board-built agy argv is a Mint. The agy session adapter appends
/// `--conversation <id>` as the LAST two argv tokens for resume/retry; a Mint
/// argv carries no conversation flag at all. Only the presence of the flag is
/// inspected (not position) because the model value after `--model` could
/// itself be spelled `--conversation`.
fn is_agy_mint(argv: &[String]) -> bool {
    !argv.iter().any(|arg| arg == "--conversation")
}

/// The agy view of [`capture_self_minted_session`]: an `id`-kind reference
/// owned by the agy agent, pinned to the exact source the Herdr 0.8.0
/// antigravity integration (embedded hook v1, `HERDR_INTEGRATION_ID=
/// antigravity_cli`) reports — the source Herdr echoes verbatim into
/// `AgentInfo.agent_session`.
fn capture_agy_session(client: &mut HerdrClient, pane_id: &str, delay: &DelayFn) -> Option<String> {
    capture_self_minted_session(
        client,
        pane_id,
        delay,
        "agy",
        "agy conversation id",
        Some("herdr:antigravity_cli"),
    )
}

/// When the bounded session capture runs relative to prompt delivery for a
/// self-minting harness.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureTiming {
    /// Capture first, then deliver the prompt (codex: the integration reports
    /// the thread id as soon as the CLI is interactive).
    BeforePrompt,
    /// Deliver the prompt first, then capture (opencode: the integration
    /// mints `agent_session` only after the first prompt; a rescue has no
    /// prompt, so the capture still runs right after readiness).
    AfterPrompt,
}

/// The shared self-minting launch body: `agent.start` with the startup-only
/// argv (no prompt file, no `--` delimiter), readiness polling, the bounded
/// session capture, and then the prompt — or the prompt and then the capture,
/// per [`CaptureTiming`].
#[allow(clippy::too_many_arguments)]
fn launch_managed_self_minting(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    pane_id: &str,
    delay: &DelayFn,
    socket: &Path,
    prompt_text: fn(&HerdrLaunchPlan) -> Option<String>,
    capture: fn(&mut HerdrClient, &str, &DelayFn) -> Option<String>,
    capture_timing: CaptureTiming,
) -> anyhow::Result<Option<String>> {
    let kind = req
        .agent_kind
        .as_deref()
        .ok_or_else(|| anyhow!("managed self-minting invocation has no agent kind"))?;
    let (_, startup_tail) = req
        .argv
        .split_first()
        .ok_or_else(|| anyhow!("managed {kind} invocation has empty startup argv"))?;
    let params = AgentStartParams {
        name: req.name.clone(),
        kind: kind.to_string(),
        pane_id: pane_id.to_string(),
        args: startup_tail.to_vec(),
        timeout_ms: Some(AGENT_START_TIMEOUT_MS),
    };
    let started = agent_start_retry_name(client, &params, req.name_fallback.as_deref(), delay)
        .map_err(|error| map_start_error(req, error))?;
    await_interactive_ready(client, &started)?;

    // Post-launch capture: the minted id is reported only once the
    // integration is up. Bounded poll on the SAME gated connection; a
    // missing/mismatched report degrades to None with a warning and the
    // launch stays successful (enqueue-time identity is kept as-is). For
    // opencode the capture waits until after the prompt, which is what
    // triggers the mint in the first place.
    let capture_after_prompt = capture_timing == CaptureTiming::AfterPrompt;
    let captured_session_id = if capture_after_prompt {
        None
    } else {
        capture(client, pane_id, delay)
    };

    if let Some(text) = prompt_text(req) {
        let initial_for_confirm = if capture_after_prompt {
            None
        } else {
            captured_session_id.clone()
        };
        agent_prompt_with_busy_retry(client, pane_id, text, &req.name, delay)?;
        confirm_delivery_after_prompt(socket, client, pane_id, delay, initial_for_confirm);
    }

    let captured_session_id = if capture_after_prompt {
        capture(client, pane_id, delay)
    } else {
        captured_session_id
    };
    Ok(captured_session_id)
}

/// The `agent.prompt` text for a codex launch: Mint receives one clearly
/// delimited block with the system instructions first, then the card task;
/// resume/fork (fresh pane) receive the task alone; a rescue sends nothing
/// (`initial_prompt` was cleared by `resume_invocation`). Same-pane reuse is
/// handled by [`launch_managed`] before this point and delivers the task
/// alone.
fn codex_prompt_text(req: &HerdrLaunchPlan) -> Option<String> {
    let task = req.initial_prompt.as_deref()?;
    if is_codex_mint(&req.argv) {
        let system = req.system_prompt.as_deref().unwrap_or_default();
        Some(board_core::harness::codex::mint_prompt(system, task))
    } else {
        Some(task.to_string())
    }
}

/// The `agent.prompt` text for an opencode launch: Mint receives one clearly
/// delimited block with the system instructions first, then the card task;
/// resume/fork (fresh pane) receive the task alone; a rescue sends nothing
/// (`initial_prompt` was cleared by `resume_invocation`). Same-pane reuse is
/// handled by [`launch_managed`] before this point and delivers the task
/// alone.
fn opencode_prompt_text(req: &HerdrLaunchPlan) -> Option<String> {
    let task = req.initial_prompt.as_deref()?;
    if is_opencode_mint(&req.argv) {
        let system = req.system_prompt.as_deref().unwrap_or_default();
        Some(board_core::harness::opencode::mint_prompt(system, task))
    } else {
        Some(task.to_string())
    }
}

/// Whether a board-built codex argv is a Mint. The codex session adapter
/// appends `resume <id>` / `fork <id>` as the LAST two tokens for
/// resume/fork; a Mint argv carries no session tokens at all. Only the tail
/// is inspected because the model value after `--model` could itself be
/// spelled `resume` or `fork`.
fn is_codex_mint(argv: &[String]) -> bool {
    !(argv.len() >= 2 && matches!(argv[argv.len() - 2].as_str(), "resume" | "fork"))
}

/// Whether a board-built opencode argv is a Mint. The opencode session
/// adapter appends `-s <id>` / `-s <id> --fork` as the LAST argv tokens for
/// resume/fork; a Mint argv carries no session flags at all. Only the tail is
/// inspected because the model value after `-m` could itself be spelled `-s`
/// or `--fork`.
fn is_opencode_mint(argv: &[String]) -> bool {
    let tail = &argv[argv.len().saturating_sub(3)..];
    !tail.iter().any(|arg| arg == "-s")
}

/// Bounded post-launch capture of the integration-reported conversation id for
/// a self-minting pane, over the launch's already-gated connection.
///
/// The report rides `AgentInfo.agent_session` (`AgentSessionInfo` in the
/// protocol-19 schema): `{agent, kind, source, value}`. A usable id must be
/// owned by the expected agent (`agent == <expected_agent>`), an `id`-kind
/// reference with a non-empty value; `expected_source` additionally pins the
/// exact integration source the pane must report (the opencode contract —
/// codex deliberately leaves the source unconstrained). A wrong agent, a
/// `path` kind, a wrong source or a blank value means the pane's identity is
/// not board-resumable, so the capture degrades to `None` with a warning. The
/// poll is bounded by [`SESSION_CAPTURE_PROBES`] and
/// [`SESSION_CAPTURE_TIMEOUT`]; a session that never appears degrades the
/// same way and the launch continues.
fn capture_self_minted_session(
    client: &mut HerdrClient,
    pane_id: &str,
    delay: &DelayFn,
    expected_agent: &str,
    subject: &str,
    expected_source: Option<&str>,
) -> Option<String> {
    let deadline = Instant::now() + SESSION_CAPTURE_TIMEOUT;
    let mut probes = 0_usize;
    loop {
        match client.agent_get(pane_id) {
            Ok(agent) => {
                if let Some(session) = agent.agent_session {
                    let agent_ok = session.agent == expected_agent;
                    let kind_ok = session.kind == "id";
                    let source_ok = match expected_source {
                        Some(expected) => session.source == expected,
                        None => true,
                    };
                    let value_ok = !session.value.trim().is_empty();
                    if agent_ok && kind_ok && source_ok && value_ok {
                        tracing::info!(
                            pane_id,
                            session_id = %session.value,
                            "captured {subject}"
                        );
                        return Some(session.value);
                    }
                    tracing::warn!(
                        pane_id,
                        agent = %session.agent,
                        kind = %session.kind,
                        source = %session.source,
                        has_value = value_ok,
                        error_category = "harness",
                        "the agent reported an unusable agent_session; run continues without a captured {subject}"
                    );
                    return None;
                }
            }
            Err(error) => {
                tracing::warn!(
                    pane_id,
                    error = %format!("{error:#}"),
                    error_category = "herdr",
                    "agent.get failed while capturing the {subject}; run continues without it"
                );
                return None;
            }
        }
        probes += 1;
        if probes >= SESSION_CAPTURE_PROBES || Instant::now() >= deadline {
            tracing::warn!(
                pane_id,
                probes,
                error_category = "harness",
                "no agent_session within the capture bound; run continues without a captured {subject}"
            );
            return None;
        }
        // Session identity may be minted asynchronously after the first
        // prompt (OpenCode does this), so do not burn the whole probe budget
        // in a tight loop. Spread the remaining probes across the wall-clock
        // bound; the injected delay keeps unit tests deterministic.
        let interval = SESSION_CAPTURE_TIMEOUT / (SESSION_CAPTURE_PROBES as u32 - 1);
        delay(interval.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// The codex view of [`capture_self_minted_session`]: an `id`-kind reference
/// owned by the codex agent; the source spelling stays unconstrained.
fn capture_codex_session(
    client: &mut HerdrClient,
    pane_id: &str,
    delay: &DelayFn,
) -> Option<String> {
    capture_self_minted_session(client, pane_id, delay, "codex", "codex thread id", None)
}

/// The opencode view of [`capture_self_minted_session`]: an `id`-kind
/// reference owned by the opencode agent, pinned to the exact source the
/// Herdr 0.8.0 opencode integration (plugin v9, `const SOURCE =
/// "herdr:opencode"` in the embedded `herdr-agent-state.js`) reports — the
/// source Herdr echoes verbatim into `AgentInfo.agent_session`.
fn capture_opencode_session(
    client: &mut HerdrClient,
    pane_id: &str,
    delay: &DelayFn,
) -> Option<String> {
    capture_self_minted_session(
        client,
        pane_id,
        delay,
        "opencode",
        "opencode session id",
        Some("herdr:opencode"),
    )
}

/// Pi/Claude fresh launch: startup argv plus the authoritative 0600
/// system-prompt file, readiness polling, then the card prompt.
fn launch_managed_prompt_file(
    client: &mut HerdrClient,
    req: &HerdrLaunchPlan,
    kind: &str,
    pane_id: &str,
    delay: &DelayFn,
    socket: &Path,
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
        let started = agent_start_retry_name(client, &params, req.name_fallback.as_deref(), delay)
            .map_err(|error| map_start_error(req, error))?;
        await_interactive_ready(client, &started)?;
        if let Some(text) = req.initial_prompt.clone() {
            let initial_session = await_session_ready(client, pane_id, delay);
            agent_prompt_with_busy_retry(client, pane_id, text, &req.name, delay)?;
            confirm_delivery_after_prompt(socket, client, pane_id, delay, initial_session);
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

/// Wrap an `agent.start` failure, classifying the owned-pane race exactly as
/// placement does so the caller can retry the whole allocation.
fn map_start_error(req: &HerdrLaunchPlan, error: HerdrError) -> anyhow::Error {
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
}

fn agent_start_retry_name(
    client: &mut HerdrClient,
    params: &AgentStartParams,
    fallback: Option<&str>,
    delay: &DelayFn,
) -> Result<AgentStarted, HerdrError> {
    // The fallback is part of the same interaction: busy retries already
    // spent on the primary name must not be available again for the fallback.
    let mut busy_budget = AgentStartBusyRetryBudget::new();
    match agent_start_retry_busy(client, params, delay, &mut busy_budget) {
        Err(HerdrError::Protocol { code, message }) if code == ERR_AGENT_NAME_TAKEN => {
            if let Some(name) = fallback {
                let mut retry = params.clone();
                retry.name = name.to_string();
                agent_start_retry_busy(client, &retry, delay, &mut busy_budget)
            } else {
                Err(HerdrError::Protocol { code, message })
            }
        }
        result => result,
    }
}

struct AgentStartBusyRetryBudget {
    retries_remaining: usize,
    backoff: Duration,
}

impl AgentStartBusyRetryBudget {
    fn new() -> Self {
        Self {
            retries_remaining: AGENT_START_BUSY_RETRIES,
            backoff: AGENT_START_BUSY_BACKOFF,
        }
    }

    fn take_retry(&mut self) -> Option<Duration> {
        let delay = self
            .retries_remaining
            .checked_sub(1)
            .map(|_| self.backoff)?;
        self.retries_remaining -= 1;
        self.backoff = self.backoff.saturating_mul(2);
        Some(delay)
    }
}

/// A newly allocated board-owned pane can answer `agent_pane_busy` both
/// because Herdr retains the previous agent state and because its login shell
/// has not reached an interactive prompt yet (slow shell boot can last
/// ~0.5s). Retry the exact start request on that same pane before giving up;
/// the caller's owned-pane cleanup handles a persistent busy response.
fn agent_start_retry_busy(
    client: &mut HerdrClient,
    params: &AgentStartParams,
    delay: &DelayFn,
    busy_budget: &mut AgentStartBusyRetryBudget,
) -> Result<AgentStarted, HerdrError> {
    loop {
        match client.agent_start(params) {
            Err(error)
                if matches!(
                    &error,
                    HerdrError::Protocol { code, .. } if code == ERR_AGENT_PANE_BUSY
                ) =>
            {
                if let Some(backoff) = busy_budget.take_retry() {
                    delay(backoff);
                } else {
                    return Err(error);
                }
            }
            result => return result,
        }
    }
}

fn is_interactive(agent: &AgentInfo) -> bool {
    agent.interactive_ready && !agent.launch_pending
}

/// Bounded poll until Herdr reports an `agent_session` with a non-empty
/// trimmed value (any agent/kind/source — pi/claude report a path-kind
/// reference). Reuses the immediate-probe-then-backoff pattern via the
/// injected [`DelayFn`]. On timeout or `agent.get` failure, logs a
/// `warn!` with `error_category = "harness"`/`"herdr"`, pane id and probe
/// count (never prompt text or secrets) and returns `None` so the caller can
/// degrade and proceed to the prompt without blocking launch.
fn await_session_ready(client: &mut HerdrClient, pane_id: &str, delay: &DelayFn) -> Option<String> {
    let deadline = Instant::now() + SESSION_READY_TIMEOUT;
    let mut probes = 0_usize;
    loop {
        match client.agent_get(pane_id) {
            Ok(agent) => {
                if let Some(session) = agent.agent_session {
                    let value = session.value.trim().to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    pane_id,
                    error = %format!("{error:#}"),
                    error_category = "herdr",
                    "agent.get failed while waiting for session readiness; proceeding to prompt"
                );
                return None;
            }
        }
        probes += 1;
        if probes >= SESSION_READY_PROBES || Instant::now() >= deadline {
            tracing::warn!(
                pane_id,
                probes,
                error_category = "harness",
                "no agent_session within the readiness bound; proceeding to prompt"
            );
            return None;
        }
        // Spread the remaining probes across the wall-clock bound exactly as
        // the post-launch session capture does: a provider resolving
        // credentials through a shell command needs seconds, not a tight
        // 100ms loop, and the injected delay keeps unit tests deterministic.
        let interval = SESSION_READY_TIMEOUT / (SESSION_READY_PROBES as u32 - 1);
        delay(interval.min(deadline.saturating_duration_since(Instant::now())));
    }
}

struct AgentPromptBusyRetryBudget {
    retries_remaining: usize,
    backoff: Duration,
}

impl AgentPromptBusyRetryBudget {
    fn new() -> Self {
        Self {
            retries_remaining: AGENT_PROMPT_BUSY_RETRIES,
            backoff: AGENT_PROMPT_BUSY_BACKOFF,
        }
    }

    fn take_retry(&mut self) -> Option<Duration> {
        let delay = self
            .retries_remaining
            .checked_sub(1)
            .map(|_| self.backoff)?;
        self.retries_remaining -= 1;
        self.backoff = self.backoff.saturating_mul(2);
        Some(delay)
    }
}

/// Deliver `agent.prompt` with bounded retry on explicit Herdr refusal.
///
/// The exact same `text` is re-sent only when Herdr returns a typed
/// `agent_pane_busy` protocol error. Between attempts the pane is re-checked
/// via `agent.get` for `is_interactive`; a pane that went non-interactive
/// is treated as a non-retryable failure. All other errors (including
/// `Io`/`Deadline`/`Disconnected`/`internal_error`) propagate immediately
/// without retry to avoid duplicate delivery of a prompt that may have
/// landed. Preserves the `with_context("herdr agent.prompt for {name}")`
/// error wrapping.
fn agent_prompt_with_busy_retry(
    client: &mut HerdrClient,
    pane_id: &str,
    text: String,
    name: &str,
    delay: &DelayFn,
) -> anyhow::Result<()> {
    let mut budget = AgentPromptBusyRetryBudget::new();
    loop {
        let result = client.agent_prompt(&AgentPromptParams {
            target: pane_id.to_string(),
            text: text.clone(),
            wait: None,
        });
        match result {
            Ok(_) => return Ok(()),
            Err(error) => {
                let is_busy = matches!(
                    &error,
                    HerdrError::Protocol { code, .. } if code == ERR_AGENT_PANE_BUSY
                );
                if is_busy {
                    match client.agent_get(pane_id) {
                        Ok(agent) if is_interactive(&agent) => {}
                        _ => {
                            return Err(anyhow::Error::new(error))
                                .with_context(|| format!("herdr agent.prompt for {name}"));
                        }
                    }
                    if let Some(backoff) = budget.take_retry() {
                        delay(backoff);
                        continue;
                    } else {
                        return Err(anyhow::Error::new(error))
                            .with_context(|| format!("herdr agent.prompt for {name}"));
                    }
                } else {
                    return Err(anyhow::Error::new(error))
                        .with_context(|| format!("herdr agent.prompt for {name}"));
                }
            }
        }
    }
}

/// Post-prompt delivery confirmation (diagnostics only).
///
/// After a successful `agent.prompt`, polls `agent.get` for a bounded
/// window (`PROMPT_CONFIRM_TIMEOUT` / `PROMPT_CONFIRM_PROBES`) with the same
/// immediate-then-backoff pattern via the injected `DelayFn`. Confirmed when
/// `agent_status` leaves `Idle` (`Working`/`Blocked`/`Done`) or
/// `agent_session` appears (None -> Some) or changes value compared to
/// `initial_session`. On timeout logs a distinct `tracing::warn!` with
/// `error_category = "harness"` that the prompt was delivered but the
/// agent never left idle within the bound, so a later `idle_expired` is
/// suspect. Never re-sends and never fails the launch.
/// Launch the post-prompt delivery confirmation without delaying the launch.
///
/// In production the poll runs on a detached diagnostics connection of its
/// own: the confirmation must never hold the spawn (and with it run
/// promotion) open, because an agent's first `board done` can land seconds
/// after the prompt. Under `cfg(test)` it runs inline on the launch's own
/// recorded connection instead, so method-sequence assertions stay
/// deterministic.
fn confirm_delivery_after_prompt(
    socket: &Path,
    client: &mut HerdrClient,
    pane_id: &str,
    delay: &DelayFn,
    initial_session: Option<String>,
) {
    #[cfg(test)]
    {
        let _ = socket;
        confirm_prompt_delivery(client, pane_id, delay, initial_session);
    }
    #[cfg(not(test))]
    {
        let _ = (client, delay);
        let pane = pane_id.to_string();
        let socket = socket.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("hb-prompt-confirm".to_string())
            .spawn(move || {
                let mut diagnostics = match HerdrClient::connect(&socket) {
                    Ok(client) => client,
                    Err(error) => {
                        tracing::warn!(
                            pane_id = %pane,
                            error = %format!("{error:#}"),
                            error_category = "herdr",
                            "opening the prompt-delivery diagnostics connection failed"
                        );
                        return;
                    }
                };
                confirm_prompt_delivery(
                    &mut diagnostics,
                    &pane,
                    &(thread::sleep as fn(Duration)),
                    initial_session,
                );
            });
        if let Err(error) = spawned {
            tracing::warn!(
                pane_id,
                error = %error.to_string(),
                error_category = "daemon",
                "spawning the prompt-delivery diagnostics poll failed"
            );
        }
    }
}

fn confirm_prompt_delivery(
    client: &mut HerdrClient,
    pane_id: &str,
    delay: &DelayFn,
    initial_session: Option<String>,
) {
    let deadline = Instant::now() + PROMPT_CONFIRM_TIMEOUT;
    let mut probes = 0_usize;
    loop {
        match client.agent_get(pane_id) {
            Ok(agent) => {
                let status_confirms = matches!(
                    agent.agent_status,
                    AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done
                );
                let session_value = agent
                    .agent_session
                    .as_ref()
                    .map(|s| s.value.trim().to_string())
                    .filter(|v| !v.is_empty());
                let session_confirms = match (&initial_session, &session_value) {
                    (None, Some(_)) => true,
                    (Some(initial), Some(current)) if initial != current => true,
                    _ => false,
                };
                if status_confirms || session_confirms {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(
                    pane_id,
                    error = %format!("{error:#}"),
                    error_category = "herdr",
                    "agent.get failed while confirming prompt delivery; diagnostics only"
                );
                return;
            }
        }
        probes += 1;
        if probes >= PROMPT_CONFIRM_PROBES || Instant::now() >= deadline {
            tracing::warn!(
                pane_id,
                probes,
                error_category = "harness",
                "prompt delivered to pane {pane_id} but the agent never left idle within {}s; if this run ends idle_expired suspect a dropped prompt",
                PROMPT_CONFIRM_TIMEOUT.as_secs()
            );
            return;
        }
        // Spread the remaining probes across the wall-clock bound (same
        // pacing as the session capture) so the diagnostic window really is
        // [`PROMPT_CONFIRM_TIMEOUT`] wide, while the injected delay keeps
        // unit tests free of wall sleeps.
        let interval = PROMPT_CONFIRM_TIMEOUT / (PROMPT_CONFIRM_PROBES as u32 - 1);
        delay(interval.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Wait for a reused pane's already-running agent to be quiescent and
/// interactive before re-prompting it. Herdr protocol 19 may expose an
/// end-of-turn agent as either Idle or derived Done; Done still has a live
/// interactive process and is therefore reusable. The pane proved out the prior
/// stage, so this covers the brief window where it is still finishing that turn
/// right after `board done`; a persistently busy pane times out rather than
/// silently opening a second pane.
fn await_reuse_ready(client: &mut HerdrClient, pane_id: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut probes = 0_usize;
    loop {
        let agent = client
            .agent_get(pane_id)
            .with_context(|| format!("herdr agent.get while waiting to reuse pane {pane_id}"))?;
        if is_interactive(&agent)
            && matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for managed agent in pane {pane_id} to become quiescent for reuse"
            );
        }
        probes += 1;
        if probes >= IMMEDIATE_READINESS_PROBES {
            thread::sleep(
                READINESS_BACKOFF.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
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
