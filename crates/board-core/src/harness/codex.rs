//! Built-in `codex` harness adapter (C7): argv, session syntax, and prompt
//! transport for the Codex CLI as a Herdr managed agent (kind `"codex"`).
//!
//! Field-verified facts (the Codex harness plan + `docs/research.md`):
//! - codex mints its own thread/session UUID; there is **no public
//!   `--session-id` for Mint**, so the board never invents one: a codex Mint
//!   carries no session flag and reports `resulting_session_id: None`, which
//!   is the daemon's signal to persist `NULL`. The thread id arrives only
//!   after launch via `agent.get.agent_session` and is promoted atomically
//!   onto run+card (`Db::promote_captured_session_uow`);
//! - resume and fork are **subcommands**: `codex resume <id>` / `codex fork
//!   <id>`, appended last to the startup argv like every other harness's
//!   session flags;
//! - the model is free-form via `--model`;
//! - reasoning effort rides `-c model_reasoning_effort=<value>`; codex spells
//!   the lowest level `none` where the board says `off`. The mapping happens
//!   only here, while building argv — the `Effort` enum is unchanged;
//! - approval rides board-facing presets — `ask-for-approval` →
//!   `--sandbox workspace-write --ask-for-approval on-request`,
//!   `approve-for-me` → `--approve-for-me` (which routes through the
//!   workspace-write sandbox per the CLI's own help), `full-access` →
//!   `--dangerously-bypass-approvals-and-sandbox`. Sandbox is a separate
//!   dimension and is only ever spelled explicitly for the first preset;
//! - codex has no system-prompt-file equivalent: the managed prompt channels
//!   (`initial_prompt` / `system_prompt`) are the only prompt transport, so
//!   startup argv carries neither task nor system text.

use crate::harness::{protocol_system_prompt, HarnessError, HarnessInvocation, SessionPlan};
use crate::prompt::EffectiveSettings;
use crate::protocol::Effort;

/// The `model_reasoning_effort` value codex expects for each board effort.
/// The board's lowest effort is `off`; codex calls it `none`. Every other
/// level keeps its canonical spelling (no `ultra` in this version).
pub(super) fn effort_value(effort: Effort) -> &'static str {
    match effort {
        Effort::Off => "none",
        Effort::Minimal => "minimal",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
}

/// The exact startup flags each board-facing approval preset maps to.
/// Verified against the installed codex CLI's help:
/// - `ask-for-approval` → `--sandbox workspace-write --ask-for-approval
///   on-request` (the explicit sandbox keeps the pair self-contained);
/// - `approve-for-me` → `--approve-for-me` alone — the CLI's own help routes
///   it through the workspace-write sandbox, so no `--sandbox` is needed;
/// - `full-access` → `--dangerously-bypass-approvals-and-sandbox` alone.
///
/// Unknown values pass through as `--ask-for-approval <value>` for forward
/// compatibility; the engine's capability validation gates card/column
/// permission values to the catalog before any launch, so only the three
/// presets reach argv in practice.
pub(super) fn approval_argv(permission: &str) -> Vec<String> {
    match permission {
        "ask-for-approval" => vec![
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--ask-for-approval".to_string(),
            "on-request".to_string(),
        ],
        "approve-for-me" => vec!["--approve-for-me".to_string()],
        "full-access" => vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
        other => vec!["--ask-for-approval".to_string(), other.to_string()],
    }
}

/// The exact startup flags codex uses to express a [`SessionPlan`], plus the
/// harness conversation id the run should persist.
///
/// Mint takes no session flag at all: codex mints its own thread id, and the
/// offered `target_uuid` is deliberately ignored — a board-invented uuid must
/// never surface in argv or persist. Resume and fork are subcommands carrying
/// the real recorded id; the fork's NEW thread id replaces it atomically at
/// promotion once the integration reports it.
pub(super) fn session_argv(
    session: &SessionPlan,
    _target_uuid: Option<&str>,
) -> Result<(Vec<String>, Option<String>), HarnessError> {
    Ok(match session {
        SessionPlan::Mint => (Vec::new(), None),
        SessionPlan::Resume(id) => (vec!["resume".to_string(), id.clone()], Some(id.clone())),
        SessionPlan::Fork(id) => (vec!["fork".to_string(), id.clone()], Some(id.clone())),
    })
}

/// Session-carrying argv tokens for re-threading a *persisted* codex argv:
/// `resume <id>` / `fork <id>` are subcommand + value pairs, so
/// [`crate::harness::strip_session_flags`] treats each as a two-token unit.
pub(super) const SESSION_FLAGS: &[(&str, bool)] = &[("resume", true), ("fork", true)];

/// Section delimiter naming the system-instructions half of the codex Mint
/// prompt block (see [`mint_prompt`]).
pub const MINT_SYSTEM_DELIMITER: &str = "## herdr-board system instructions";
/// Section delimiter naming the card-task half of the codex Mint prompt block
/// (see [`mint_prompt`]).
pub const MINT_TASK_DELIMITER: &str = "## herdr-board card task";

/// Compose the single `agent.prompt` block a codex **Mint** receives: the
/// system instructions first, then the card task, each under its own clearly
/// delimited heading.
///
/// Codex has no system-prompt file equivalent and no prompt in startup argv,
/// so this block is the *only* prompt transport for a fresh conversation: the
/// integration reads the system instructions and the task from one prompt,
/// with the delimiters making the boundary explicit (and byte-assertable).
/// Resume, fork and same-pane reuse deliver the task alone; a rescue delivers
/// neither.
pub fn mint_prompt(system_prompt: &str, task: &str) -> String {
    format!("{MINT_SYSTEM_DELIMITER}\n{system_prompt}\n\n{MINT_TASK_DELIMITER}\n{task}")
}

/// Build a managed Herdr codex launch: `codex [--model M]
/// [-c model_reasoning_effort=E] [permission preset flags] [resume|fork <id>]`.
///
/// Both prompt channels ride outside argv — `initial_prompt` carries the card
/// task, `system_prompt` the column instructions plus the protocol trailer —
/// and the daemon submits them only after the agent is interactive (Mint:
/// `system_prompt + task` delimited block; Resume/Fork/reuse: task only;
/// rescue re-sends neither). Startup argv therefore contains no prompt text
/// and no `--` delimiter.
pub fn managed_codex_invocation(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    let mut argv = vec!["codex".to_string()];
    if let Some(model) = &settings.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(effort) = settings.effort {
        argv.extend([
            "-c".to_string(),
            format!("model_reasoning_effort={}", effort_value(effort)),
        ]);
    }
    if let Some(permission) = &settings.permission_mode {
        argv.extend(approval_argv(permission));
    }

    let (session_flags, resulting_session_id) = session_argv(session, minted_uuid)?;
    argv.extend(session_flags);

    Ok(HarnessInvocation {
        agent_kind: Some("codex".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env: Vec::new(),
        resulting_session_id,
    })
}
