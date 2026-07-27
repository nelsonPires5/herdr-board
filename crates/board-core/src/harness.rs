//! Harness adapters: turn resolved settings + a prompt into `(argv, env)`.
//!
//! Two kinds:
//! - built-ins `pi` and `claude` — flags exactly per `docs/protocol.md`;
//! - config-defined harnesses — an argv template with `{model}`/`{effort}`/
//!   `{permission_mode}` placeholders; prompt via `BOARD_PROMPT` env.

use crate::capability::ResumeSupport;
use crate::config::Config;
use crate::launch::ExecutionSpec;
use crate::prompt::EffectiveSettings;

/// Harness stored on newly-created cards when the caller omits one.
pub const DEFAULT_HARNESS: &str = "pi";
/// Built-ins routed without config-defined argv/env reconstruction.
pub const BUILTIN_HARNESSES: [&str; 2] = ["pi", "claude"];

pub fn is_builtin_harness(name: &str) -> bool {
    BUILTIN_HARNESSES.contains(&name)
}

/// How to thread the harness session for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPlan {
    /// Mint a brand new session (`--session-id <uuid>`). Caller supplies the uuid.
    Mint,
    /// Resume an existing session (`--resume <id>`).
    Resume(String),
    /// Fork an existing session on retry (`--resume <id> --fork-session`).
    Fork(String),
}

/// Decide the session strategy for a run.
///
/// A forced-fresh column always mints; so does the absence of a prior session.
/// Otherwise a retry forks the existing session, and a normal continuation resumes it.
pub fn plan_session(existing: Option<&str>, fresh_session: bool, is_retry: bool) -> SessionPlan {
    match existing {
        Some(id) if !fresh_session && is_retry => SessionPlan::Fork(id.to_string()),
        Some(id) if !fresh_session => SessionPlan::Resume(id.to_string()),
        _ => SessionPlan::Mint,
    }
}

/// A fully-resolved process invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessInvocation {
    /// Explicit Herdr managed-agent kind. Configured harnesses are unmanaged,
    /// even when their executable happens to be named `pi` or `claude`.
    pub agent_kind: Option<String>,
    /// Card task submitted after a managed agent becomes interactive. Custom
    /// harnesses receive the same value through `BOARD_PROMPT` instead.
    pub initial_prompt: Option<String>,
    /// Authoritative managed-agent system instructions. The daemon transports
    /// these separately from startup argv. Custom harnesses receive them
    /// through `BOARD_SYSTEM_PROMPT` instead.
    pub system_prompt: Option<String>,
    /// Startup command and flags. Managed invocations contain no prompt text;
    /// configured harnesses retain their exact configured argv.
    pub argv: Vec<String>,
    /// Extra env pairs the harness itself needs (e.g. `BOARD_PROMPT` for custom
    /// harnesses). The daemon adds `BOARD_CARD_ID`/`BOARD_RUN_ID`/`BOARD_SOCKET`.
    pub env: Vec<(String, String)>,
    /// Harness session id that the card/run should persist. Custom harnesses do
    /// not participate in built-in session bookkeeping and return `None`.
    pub resulting_session_id: Option<String>,
}

/// Errors from building a harness invocation.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    #[error("unknown harness: {0}")]
    UnknownHarness(String),
    /// `SessionPlan::Mint` was requested but no minted uuid was supplied.
    #[error("mint session requested without a minted uuid")]
    MissingMintedSession,
    #[error("pi fork session requested without a new target uuid")]
    MissingForkTargetSession,
    #[error("pi does not support permission modes")]
    PiPermissionModeUnsupported,
    /// The harness declares no way to resume a conversation by id. There is
    /// deliberately no fallback: launching a *fresh* conversation instead would
    /// silently re-run the task.
    #[error("harness '{0}' cannot resume a recorded conversation, so its run cannot be reopened")]
    ResumeUnsupported(String),
    /// Resume was requested for a run that never recorded a conversation id.
    #[error("resume requires a recorded harness conversation id, and this run has none")]
    MissingResumeSession,
    /// The persisted argv is the legacy all-in-one form that embeds the prompt
    /// positionally, so resume flags cannot be appended safely. See the
    /// invariant documented on [`resume_invocation`].
    #[error(
        "harness '{0}' recorded a legacy all-in-one command line that embeds the task text, \
         so it cannot be re-threaded onto a resume without re-sending that task"
    )]
    ResumeLegacyArgv(String),
}

// ---------------------------------------------------------------------------
// Session flags — one owner for every harness's session syntax
// ---------------------------------------------------------------------------

/// The exact startup flags a built-in harness uses to express a
/// [`SessionPlan`], plus the harness conversation id the run should persist.
///
/// This is the single source of truth for "how does this harness spell mint /
/// resume / fork". Every argv builder in this module goes through it, and so
/// does [`resume_invocation`], so a rescue can never drift into a second,
/// hand-rolled resume syntax.
pub fn session_argv(
    harness_name: &str,
    session: &SessionPlan,
    target_uuid: Option<&str>,
) -> Result<(Vec<String>, Option<String>), HarnessError> {
    match harness_name {
        "pi" => match session {
            SessionPlan::Mint => {
                let id = target_uuid.ok_or(HarnessError::MissingMintedSession)?;
                Ok((
                    vec!["--session-id".to_string(), id.to_string()],
                    Some(id.to_string()),
                ))
            }
            // Pi has no separate resume flag: handing back a session id it
            // already knows re-attaches to that conversation.
            SessionPlan::Resume(id) => Ok((
                vec!["--session-id".to_string(), id.clone()],
                Some(id.clone()),
            )),
            SessionPlan::Fork(source) => {
                let target = target_uuid.ok_or(HarnessError::MissingForkTargetSession)?;
                Ok((
                    vec![
                        "--fork".to_string(),
                        source.clone(),
                        "--session-id".to_string(),
                        target.to_string(),
                    ],
                    Some(target.to_string()),
                ))
            }
        },
        "claude" => match session {
            SessionPlan::Mint => {
                let id = target_uuid.ok_or(HarnessError::MissingMintedSession)?;
                Ok((
                    vec!["--session-id".to_string(), id.to_string()],
                    Some(id.to_string()),
                ))
            }
            SessionPlan::Resume(id) => {
                Ok((vec!["--resume".to_string(), id.clone()], Some(id.clone())))
            }
            SessionPlan::Fork(id) => Ok((
                vec![
                    "--resume".to_string(),
                    id.clone(),
                    "--fork-session".to_string(),
                ],
                Some(id.clone()),
            )),
        },
        other => Err(HarnessError::UnknownHarness(other.to_string())),
    }
}

/// Session-carrying startup flags per built-in harness, as `(flag, takes_value)`.
/// Re-threading a *persisted* argv onto another [`SessionPlan`] means removing
/// exactly these and appending [`session_argv`]; listing them explicitly keeps
/// that surgery per-harness instead of a generic guess about flag shapes.
fn session_flags(harness_name: &str) -> &'static [(&'static str, bool)] {
    match harness_name {
        "pi" => &[("--session-id", true), ("--fork", true)],
        "claude" => &[
            ("--session-id", true),
            ("--resume", true),
            ("--fork-session", false),
        ],
        _ => &[],
    }
}

/// Drop every session-carrying flag (and its value) from a persisted argv.
fn strip_session_flags(harness_name: &str, argv: &[String]) -> Vec<String> {
    let flags = session_flags(harness_name);
    let mut out = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        match flags.iter().find(|(flag, _)| *flag == argv[index]) {
            Some((_, true)) => index += 2,
            Some((_, false)) => index += 1,
            None => {
                out.push(argv[index].clone());
                index += 1;
            }
        }
    }
    out
}

/// Environment variable carrying the conversation id to a *configured* harness
/// on resume. A configured run's argv is persisted fully materialized, so there
/// is no placeholder left to substitute; declaring `resume = true` in
/// `[harness.NAME]` is a promise to read this variable.
pub const BOARD_RESUME_SESSION_ID: &str = "BOARD_RESUME_SESSION_ID";

/// Marker env var set on every rescued pane. It tells the harness (and anything
/// reading the run env) that this pane continues an already-finished run and is
/// not board-managed, so `board done` no longer applies to it.
pub const BOARD_RESCUE: &str = "BOARD_RESCUE";

/// Build the launch that **resumes** a run's conversation in a fresh pane.
///
/// The spec is derived from the run's *persisted* [`ExecutionSpec`] rather than
/// rebuilt from current config, so a rescue continues the same work in the same
/// execution environment (model, effort, permission mode, env) instead of
/// silently switching model after a config edit.
///
/// Two invariants are explicit here, not incidental:
/// - `initial_prompt` is cleared and `BOARD_PROMPT` is removed from the env, so
///   resuming can never re-send the original task and re-run it;
/// - an unsupported harness or a missing conversation id is an error, never a
///   fresh-conversation fallback.
///
/// # Invariant: the persisted built-in argv carries no positional prompt
///
/// Re-threading works by [`strip_session_flags`] + append, which is only sound
/// while the persisted argv is *startup-only*. That holds for every spec
/// [`build_invocation`] produces (protocol-17 managed launches keep both prompt
/// channels out of argv). The legacy all-in-one helpers [`claude_argv`] /
/// [`pi_argv`] do **not** hold it: they end in `-- "<prompt>"` (claude) or a
/// bare positional (pi). If such an argv were ever persisted, appending resume
/// flags would produce `… -- "<prompt>" --resume <id>` — re-sending the task
/// *and* handing `--resume` to the harness as prompt text. Rather than trust
/// that this never happens, the `--` form is detected and refused below.
pub fn resume_invocation(
    harness_name: &str,
    support: ResumeSupport,
    persisted: &ExecutionSpec,
    session_id: &str,
) -> Result<ExecutionSpec, HarnessError> {
    if !support.is_supported() {
        return Err(HarnessError::ResumeUnsupported(harness_name.to_string()));
    }
    if session_id.trim().is_empty() {
        return Err(HarnessError::MissingResumeSession);
    }

    let argv = if is_builtin_harness(harness_name) {
        // Fail closed on the legacy all-in-one form (see the invariant above).
        if persisted.argv.iter().any(|arg| arg == "--") {
            return Err(HarnessError::ResumeLegacyArgv(harness_name.to_string()));
        }
        let mut argv = strip_session_flags(harness_name, &persisted.argv);
        let (session, _) = session_argv(
            harness_name,
            &SessionPlan::Resume(session_id.to_string()),
            None,
        )?;
        argv.extend(session);
        argv
    } else {
        // A configured harness runs its persisted argv unchanged; the id is
        // delivered through the environment (see BOARD_RESUME_SESSION_ID).
        persisted.argv.clone()
    };

    let mut env: Vec<(String, String)> = persisted
        .env
        .iter()
        .filter(|(key, _)| key != "BOARD_PROMPT")
        .cloned()
        .collect();
    env.push((BOARD_RESUME_SESSION_ID.to_string(), session_id.to_string()));
    env.push((BOARD_RESCUE.to_string(), "1".to_string()));

    Ok(ExecutionSpec {
        argv,
        env,
        agent_kind: persisted.agent_kind.clone(),
        // Never re-send the card task: the conversation already contains it.
        initial_prompt: None,
        system_prompt: persisted.system_prompt.clone(),
    })
}

/// Board-protocol trailer appended to EVERY run's system prompt (built-in argv
/// and custom-harness `BOARD_SYSTEM_PROMPT` alike): the close-out contract must
/// not depend on users remembering it in each column's prompt.
/// `board comment`/`done` read `$BOARD_CARD_ID` from the run env.
pub const BOARD_PROTOCOL_TRAILER: &str = "\
## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal \
is met you MUST finish with exactly two commands: first `board comment \"<your results, \
files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT \
met — something failed or you got lost — use `board done --outcome fail --summary \
\"<why>\"` instead. Always comment before done. Never use `board move`/`cancel`/`retry` \
on your own card. Finishing or going idle WITHOUT `board done` leaves the card in \
`awaiting` for human review — a run is never auto-completed.";

/// Compose the effective system prompt for a run: the column's (if any) plus
/// the unconditional [`BOARD_PROTOCOL_TRAILER`]. Every dispatch — independent
/// of column config — carries the close-out protocol.
pub fn protocol_system_prompt(column_prompt: Option<&str>) -> String {
    match column_prompt {
        Some(sp) => format!("{sp}\n\n{BOARD_PROTOCOL_TRAILER}"),
        None => BOARD_PROTOCOL_TRAILER.to_string(),
    }
}

/// Build the legacy all-in-one argv for the builtin `claude` harness.
///
/// This public helper is retained for existing callers; protocol-17 dispatch
/// uses [`build_invocation`] so prompts are not included in startup argv.
///
/// `claude [--model M] [--effort E] [--permission-mode P] --append-system-prompt SP
/// --allowedTools "Bash(board:*)" (--session-id UUID | --resume ID) [--fork-session]
/// -- "PROMPT"`. The system prompt is the column's (if any) plus
/// [`BOARD_PROTOCOL_TRAILER`]; `--allowedTools` keeps `board comment`/`done` from
/// hitting a permission prompt under restrictive permission modes.
pub fn claude_argv(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<Vec<String>, HarnessError> {
    let mut argv = vec!["claude".to_string()];

    if let Some(m) = &settings.model {
        argv.push("--model".to_string());
        argv.push(m.clone());
    }
    if let Some(e) = &settings.effort {
        argv.push("--effort".to_string());
        argv.push(e.as_str().to_string());
    }
    if let Some(p) = &settings.permission_mode {
        argv.push("--permission-mode".to_string());
        argv.push(p.clone());
    }
    argv.push("--append-system-prompt".to_string());
    argv.push(protocol_system_prompt(settings.system_prompt.as_deref()));
    argv.push("--allowedTools".to_string());
    argv.push("Bash(board:*)".to_string());

    argv.extend(session_argv("claude", session, minted_uuid)?.0);

    argv.push("--".to_string());
    argv.push(prompt.to_string());
    Ok(argv)
}

/// Build the legacy all-in-one argv/session result for the built-in Pi harness.
///
/// This public helper is retained for existing callers; protocol-17 dispatch
/// uses [`build_invocation`] so prompts are not included in startup argv.
/// Pi takes a normal positional prompt (no Claude `--` delimiter). Prefixing it
/// with non-flag text ensures a card description beginning with `-` cannot be
/// interpreted as another CLI option.
pub fn pi_argv(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    target_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    if settings.permission_mode.is_some() {
        return Err(HarnessError::PiPermissionModeUnsupported);
    }

    let mut argv = vec!["pi".to_string()];
    if let Some(model) = &settings.model {
        argv.push("--model".to_string());
        argv.push(model.clone());
    }
    if let Some(effort) = settings.effort {
        argv.push("--thinking".to_string());
        argv.push(effort.as_str().to_string());
    }
    argv.push("--append-system-prompt".to_string());
    argv.push(protocol_system_prompt(settings.system_prompt.as_deref()));

    let (session_flags, resulting_session_id) = session_argv("pi", session, target_uuid)?;
    argv.extend(session_flags);
    let resulting_session_id = resulting_session_id.ok_or(HarnessError::MissingMintedSession)?;
    argv.push(format!("Card task:\n{prompt}"));

    Ok(HarnessInvocation {
        agent_kind: Some("pi".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env: Vec::new(),
        resulting_session_id: Some(resulting_session_id),
    })
}

/// Build a full invocation for `harness_name`, using a built-in adapter or a
/// config-defined harness template.
pub fn build_invocation(
    harness_name: &str,
    config: &Config,
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    if harness_name == "pi" {
        return managed_pi_invocation(settings, session, minted_uuid, prompt);
    }
    if harness_name == "claude" {
        return managed_claude_invocation(settings, session, minted_uuid, prompt);
    }

    let def = config
        .harness
        .get(harness_name)
        .ok_or_else(|| HarnessError::UnknownHarness(harness_name.to_string()))?;

    let argv = substitute_template(&def.argv, settings);

    // The protocol trailer is unconditional: custom harnesses get it via
    // BOARD_SYSTEM_PROMPT even when the column sets no system prompt.
    let env = vec![
        ("BOARD_PROMPT".to_string(), prompt.to_string()),
        (
            "BOARD_SYSTEM_PROMPT".to_string(),
            protocol_system_prompt(settings.system_prompt.as_deref()),
        ),
    ];

    Ok(HarnessInvocation {
        agent_kind: None,
        initial_prompt: None,
        system_prompt: None,
        argv,
        env,
        resulting_session_id: None,
    })
}

/// Build a protocol-17 managed Pi launch. Unlike the legacy argv helper above,
/// this keeps both prompt channels out of startup argv so Herdr can start the
/// agent first and submit the card task only after it is interactive.
fn managed_pi_invocation(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    target_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    if settings.permission_mode.is_some() {
        return Err(HarnessError::PiPermissionModeUnsupported);
    }

    let mut argv = vec!["pi".to_string()];
    if let Some(model) = &settings.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(effort) = settings.effort {
        argv.extend(["--thinking".to_string(), effort.as_str().to_string()]);
    }

    let (session_flags, resulting_session_id) = session_argv("pi", session, target_uuid)?;
    argv.extend(session_flags);
    let resulting_session_id = resulting_session_id.ok_or(HarnessError::MissingMintedSession)?;

    Ok(HarnessInvocation {
        agent_kind: Some("pi".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env: Vec::new(),
        resulting_session_id: Some(resulting_session_id),
    })
}

/// Build a protocol-17 managed Claude launch while preserving the established
/// model/effort/permission/session flag ordering exactly.
fn managed_claude_invocation(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    let mut argv = vec!["claude".to_string()];
    if let Some(model) = &settings.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(effort) = settings.effort {
        argv.extend(["--effort".to_string(), effort.as_str().to_string()]);
    }
    if let Some(permission) = &settings.permission_mode {
        argv.extend(["--permission-mode".to_string(), permission.clone()]);
    }
    argv.extend(["--allowedTools".to_string(), "Bash(board:*)".to_string()]);

    let (session_flags, resulting_session_id) = session_argv("claude", session, minted_uuid)?;
    argv.extend(session_flags);

    Ok(HarnessInvocation {
        agent_kind: Some("claude".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env: Vec::new(),
        resulting_session_id,
    })
}

/// Substitute `{model}`/`{effort}`/`{permission_mode}` in each template element.
/// An element referencing an unset placeholder is dropped entirely.
fn substitute_template(template: &[String], settings: &EffectiveSettings) -> Vec<String> {
    let model = settings.model.as_deref();
    let effort = settings.effort.map(|e| e.as_str());
    let perm = settings.permission_mode.as_deref();

    let mut out = Vec::with_capacity(template.len());
    'items: for item in template {
        let mut cur = item.clone();
        for (ph, val) in [
            ("{model}", model),
            ("{effort}", effort),
            ("{permission_mode}", perm),
        ] {
            if cur.contains(ph) {
                match val {
                    Some(v) => cur = cur.replace(ph, v),
                    None => continue 'items, // unset placeholder → drop element
                }
            }
        }
        out.push(cur);
    }
    out
}
