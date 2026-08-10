//! Built-in `opencode` harness adapter (O7): argv, session syntax, and prompt
//! transport for the OpenCode TUI as a Herdr managed agent (kind
//! `"opencode"`).
//!
//! Field-verified facts (opencode 1.18.15 — `opencode --help`,
//! `opencode run --help`, `opencode models --verbose`):
//! - the TUI is `opencode [project]` with `-m/--model provider/model`,
//!   `-s/--session <id>`, `--fork` (with `--session`), and `--auto`
//!   (auto-approve); the **root/TUI does NOT accept `--variant`** — that is
//!   the `opencode run` subcommand's per-model reasoning-effort spelling
//!   ("model variant (provider-specific reasoning effort, e.g., high, max,
//!   minimal)" on `opencode run --help`);
//! - effort therefore never rides argv: when a board effort is set, the
//!   adapter injects a process-local config through the `OPENCODE_CONFIG_CONTENT`
//!   env var defining a stable custom agent ([`AGENT_NAME`]) with exactly
//!   `model` + `variant`, and selects it with `--agent herdr-board` (verified:
//!   the TUI accepts `--agent`, and the backend applies the agent's
//!   `variant` when its model matches). The JSON is built with
//!   `serde_json`, never string interpolation, so any model text is escaped
//!   safely. A board effort with **no model is an error** — the config agent
//!   carries the model, so effort is only expressible per-model. Without an
//!   effort no config is injected and the model stays `-m provider/model`;
//! - a fresh TUI session mints its own `ses_…` id; there is **no way to
//!   pre-allocate one**, so an opencode Mint carries no session flag and
//!   reports `resulting_session_id: None`, which is the daemon's signal to
//!   persist `NULL`. The reported id arrives only after launch via
//!   `agent.get.agent_session` and is promoted atomically onto run+card
//!   (`Db::promote_captured_session_uow`);
//! - resume and fork are `-s <root>` / `-s <root> --fork`, appended last to
//!   the startup argv like every other harness's session flags; the fork's
//!   NEW id replaces the source id atomically at promotion once the
//!   integration reports it;
//! - the model is free-form via `-m provider/model` when no effort is set;
//! - the board calls the effort dimension **effort** everywhere in the
//!   API/UI/DB, and only the opencode spelling is `variant`. opencode spells
//!   the lowest level `none` where the board says `off`; the mapping happens
//!   only here, while building the agent config — the `Effort` enum is
//!   unchanged;
//! - permission modes are board-facing presets — `default` (no flag) and
//!   `auto-approve` (`--auto`) — the only two spellings derived from verified
//!   CLI behavior; any other value is rejected up front by the engine's
//!   capability validation, so only these reach argv;
//! - opencode has no system-prompt file equivalent and the card task must
//!   never ride `--prompt` in startup argv: the managed prompt channels
//!   (`initial_prompt` / `system_prompt`) are the only prompt transport, and
//!   the daemon submits them only after the agent is interactive (Mint:
//!   `system_prompt + task` delimited block via [`mint_prompt`]; Resume/Fork/
//!   reuse: task only; rescue: neither).

use crate::harness::{protocol_system_prompt, HarnessError, HarnessInvocation, SessionPlan};
use crate::prompt::EffectiveSettings;
use crate::protocol::Effort;

/// The stable custom agent injected into the process-local opencode config
/// when a board effort is set. The TUI has no `--variant` flag, so effort
/// rides this agent's per-model `variant` instead.
pub const AGENT_NAME: &str = "herdr-board";
/// Env var carrying the process-local opencode config JSON (a custom agent
/// holding `model` + `variant`). The TUI reads it in addition to the
/// user-level config; only this env is set, so nothing global is touched.
pub const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// The `variant` value opencode expects for each board effort, embedded in
/// the [`AGENT_NAME`] agent config. The board's lowest effort is `off`;
/// opencode calls it `none` (observed in `opencode models --verbose` variant
/// keys). Every other level keeps its canonical spelling.
pub(super) fn effort_variant(effort: Effort) -> &'static str {
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

/// The exact `OPENCODE_CONFIG_CONTENT` JSON defining the [`AGENT_NAME`] agent
/// with exactly `model` and `variant` (board `off` → opencode `none`).
///
/// Built through `serde_json` — never string interpolation — so arbitrary
/// model text (quotes, backslashes, control characters) is escaped safely and
/// the payload is always valid JSON.
pub fn effort_agent_config(model: &str, effort: Effort) -> String {
    serde_json::json!({
        "agent": {
            AGENT_NAME: {
                "model": model,
                "variant": effort_variant(effort),
            }
        }
    })
    .to_string()
}

/// The exact startup flags each board-facing permission mode maps to.
/// Verified against the installed CLI:
/// - `default` → nothing (no flag: the CLI's own default is manual approval);
/// - `auto-approve` → `--auto`.
///
/// Unknown values map to no flag for forward compatibility; the engine's
/// capability validation gates card/column permission values to the catalog
/// before any launch, so only the two modes reach argv in practice.
pub(super) fn permission_argv(permission: &str) -> Vec<String> {
    if permission == "auto-approve" {
        vec!["--auto".to_string()]
    } else {
        Vec::new()
    }
}

/// The exact startup flags opencode uses to express a [`SessionPlan`], plus
/// the harness conversation id the run should persist.
///
/// Mint takes no session flag at all: opencode mints its own `ses_…` id, and
/// the offered `target_uuid` is deliberately ignored — a board-invented uuid
/// must never surface in argv or persist. Resume and fork carry the real
/// recorded root id (`-s <id>` / `-s <id> --fork`); the fork's NEW id
/// replaces it atomically at promotion once the integration reports it.
pub(super) fn session_argv(
    session: &SessionPlan,
    _target_uuid: Option<&str>,
) -> Result<(Vec<String>, Option<String>), HarnessError> {
    Ok(match session {
        SessionPlan::Mint => (Vec::new(), None),
        SessionPlan::Resume(id) => (vec!["-s".to_string(), id.clone()], Some(id.clone())),
        SessionPlan::Fork(id) => (
            vec!["-s".to_string(), id.clone(), "--fork".to_string()],
            Some(id.clone()),
        ),
    })
}

/// Session-carrying argv tokens for re-threading a *persisted* opencode argv:
/// `-s <id>` (takes a value) and `--fork` (no value), so
/// [`crate::harness::strip_session_flags`] treats each as its own unit.
pub(super) const SESSION_FLAGS: &[(&str, bool)] = &[("-s", true), ("--fork", false)];

/// Section delimiter naming the system-instructions half of the opencode Mint
/// prompt block (see [`mint_prompt`]).
pub const MINT_SYSTEM_DELIMITER: &str = "## herdr-board system instructions";
/// Section delimiter naming the card-task half of the opencode Mint prompt
/// block (see [`mint_prompt`]).
pub const MINT_TASK_DELIMITER: &str = "## herdr-board card task";

/// Compose the single `agent.prompt` block an opencode **Mint** receives: the
/// system instructions first, then the card task, each under its own clearly
/// delimited heading (the same convention as the codex adapter).
///
/// OpenCode has no system-prompt file equivalent and the task must never ride
/// startup argv, so this block is the *only* prompt transport for a fresh
/// conversation: the integration reads the system instructions and the task
/// from one prompt, with the delimiters making the boundary explicit (and
/// byte-assertable). Resume, fork and same-pane reuse deliver the task alone;
/// a rescue delivers neither.
pub fn mint_prompt(system_prompt: &str, task: &str) -> String {
    format!("{MINT_SYSTEM_DELIMITER}\n{system_prompt}\n\n{MINT_TASK_DELIMITER}\n{task}")
}

/// Build a managed Herdr opencode launch: `opencode [--agent herdr-board |
/// -m provider/model] [--auto] [-s <id>] [--fork]`.
///
/// With a board effort set, `--variant` never rides argv (the TUI does not
/// accept it): a process-local config via [`CONFIG_ENV`] defines the stable
/// [`AGENT_NAME`] agent carrying `model` + `variant`, selected with
/// `--agent herdr-board`, and `-m` is dropped (the agent owns the model). An
/// effort with no model is a typed error, not a silently lost setting. Without
/// an effort no config/agent is injected and the model stays `-m`.
///
/// Both prompt channels ride outside argv — `initial_prompt` carries the card
/// task, `system_prompt` the column instructions plus the protocol trailer —
/// and the daemon submits them only after the agent is interactive (Mint:
/// `system_prompt + task` delimited block; Resume/Fork/reuse: task only;
/// rescue re-sends neither). Startup argv therefore contains no prompt text
/// and no `--` delimiter, and the config env carries no prompt text either.
pub fn managed_opencode_invocation(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    let mut argv = vec!["opencode".to_string()];
    let mut env = Vec::new();
    if let Some(effort) = settings.effort {
        let model = settings
            .model
            .as_deref()
            .ok_or(HarnessError::OpenCodeEffortRequiresModel)?;
        argv.extend(["--agent".to_string(), AGENT_NAME.to_string()]);
        env.push((CONFIG_ENV.to_string(), effort_agent_config(model, effort)));
    } else if let Some(model) = &settings.model {
        argv.extend(["-m".to_string(), model.clone()]);
    }
    if let Some(permission) = &settings.permission_mode {
        argv.extend(permission_argv(permission));
    }

    let (session_flags, resulting_session_id) = session_argv(session, minted_uuid)?;
    argv.extend(session_flags);

    Ok(HarnessInvocation {
        agent_kind: Some("opencode".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env,
        resulting_session_id,
    })
}
