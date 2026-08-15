//! Built-in `antigravity` harness adapter (A7): argv, session syntax, and
//! prompt transport for the Antigravity CLI (`agy`) as a Herdr managed agent
//! (kind `"agy"`).
//!
//! Field-verified facts (agy 1.1.13 — `agy --help`, `agy --output-format
//! json models`):
//! - the public harness name is `antigravity`; the Herdr agent kind and the
//!   executable are `agy` (`herdr agent start --help`: `kinds:
//!   pi|claude|codex|gemini|cursor|devin|agy|…`);
//! - the TUI is `agy` with `--model <base>`, `--effort
//!   (low|medium|high)`, `--conversation <id>` (resume by conversation id),
//!   `--sandbox` (restricted execution), and
//!   `--dangerously-skip-permissions` (auto-approve). There is no
//!   `--permission-mode` flag: the user's `toolPermission` setting lives in
//!   the CLI's own config and the board never edits it — `current` means
//!   "no flag, keep whatever the user configured";
//! - permission modes are board-facing presets — `current` (no flag),
//!   `sandbox` (`--sandbox`), `always-proceed`
//!   (`--dangerously-skip-permissions`) — the only three spellings derived
//!   from verified CLI behavior; any other value is rejected up front by
//!   the engine's capability validation, so only these reach argv;
//! - a fresh TUI session mints its own conversation id (a UUID printed at
//!   startup: "Resume with -c (or command below): agy --conversation=<id>");
//!   there is **no way to pre-allocate one**, so an antigravity Mint carries
//!   no conversation flag and reports `resulting_session_id: None`, which
//!   is the daemon's signal to persist `NULL`. The reported id arrives only
//!   after launch via `agent.get.agent_session` and is promoted atomically
//!   onto run+card (`Db::promote_captured_session_uow`);
//! - resume and retry are `--conversation <id>`, appended last to the
//!   startup argv like every other harness's session flags. Antigravity has
//!   **no fork**: a retry creates a new run and a new pane that re-attaches
//!   to the SAME conversation (`--conversation <id>`) and re-sends the task
//!   — the fork plan degrades to the resume spelling, never a simulated
//!   fork. When the recorded conversation no longer exists agy starts a new
//!   one with a warning; the daemon detects the fallback by comparing the
//!   integration-reported id against the requested one;
//! - the model is the **normalized base id** from the live catalog
//!   ([`crate::agy_catalog`]) — variants like `gemini-3.7-flash-high` are
//!   listed as model `gemini-3.7-flash` with efforts `low|medium|high` and
//!   run as `--model gemini-3.7-flash --effort high`. Fixed-effort models
//!   (e.g. `claude-sonnet-4-6`) never send `--effort`;
//! - agy has no system-prompt file equivalent and the card task must never
//!   ride startup argv: the managed prompt channels (`initial_prompt` /
//!   `system_prompt`) are the only prompt transport, and the daemon submits
//!   them only after the agent is interactive (Mint:
//!   `system_prompt + task` delimited block via [`mint_prompt`]; Resume/
//!   retry: task only; rescue: neither).

use crate::harness::{protocol_system_prompt, HarnessError, HarnessInvocation, SessionPlan};
use crate::prompt::EffectiveSettings;
use crate::protocol::Effort;

/// The `--effort` value agy expects for each board effort. agy only knows
/// `low|medium|high` (verified against `agy --help`); every other board
/// level has no agy spelling and is filtered out by the capability ladder
/// before it can reach argv.
pub(super) fn effort_value(effort: Effort) -> Option<&'static str> {
    match effort {
        Effort::Low => Some("low"),
        Effort::Medium => Some("medium"),
        Effort::High => Some("high"),
        _ => None,
    }
}

/// The exact startup flags each board-facing permission mode maps to.
/// Verified against the installed CLI:
/// - `current` → nothing (no flag: the CLI keeps the user's configured
///   `toolPermission` — the board never edits `settings.json`);
/// - `sandbox` → `--sandbox` ("Run in a sandbox with terminal restrictions
///   enabled"; approval rules stay the user's);
/// - `always-proceed` → `--dangerously-skip-permissions` ("Auto-approve all
///   tool permission requests without prompting").
///
/// Unknown values map to no flag for forward compatibility; the engine's
/// capability validation gates card/column permission values to the catalog
/// before any launch, so only the three modes reach argv in practice.
pub(super) fn permission_argv(permission: &str) -> Vec<String> {
    match permission {
        "sandbox" => vec!["--sandbox".to_string()],
        "always-proceed" => vec!["--dangerously-skip-permissions".to_string()],
        _ => Vec::new(),
    }
}

/// The exact startup flags agy uses to express a [`SessionPlan`], plus the
/// harness conversation id the run should persist.
///
/// Mint takes no conversation flag at all: agy mints its own id, and the
/// offered `target_uuid` is deliberately ignored — a board-invented uuid
/// must never surface in argv or persist. Resume and retry carry the real
/// recorded id (`--conversation <id>`); antigravity has no fork, so the
/// Fork plan (board retry) degrades to the same resume spelling — the
/// retry's whole point is re-attaching to the SAME conversation in a new
/// pane. The id agy reports after launch replaces the recorded one at
/// promotion when the fallback fired (the recorded conversation no longer
/// exists and agy minted another).
pub(super) fn session_argv(
    session: &SessionPlan,
    _target_uuid: Option<&str>,
) -> Result<(Vec<String>, Option<String>), HarnessError> {
    Ok(match session {
        SessionPlan::Mint => (Vec::new(), None),
        // Resume AND Fork share the one agy spelling: `--conversation <id>`.
        // Antigravity deliberately never simulates a fork.
        SessionPlan::Resume(id) | SessionPlan::Fork(id) => (
            vec!["--conversation".to_string(), id.clone()],
            Some(id.clone()),
        ),
    })
}

/// Session-carrying argv tokens for re-threading a *persisted* agy argv:
/// `--conversation <id>` (takes a value), so
/// [`crate::harness::strip_session_flags`] treats it as one unit.
pub(super) const SESSION_FLAGS: &[(&str, bool)] = &[("--conversation", true)];

/// Section delimiter naming the system-instructions half of the agy Mint
/// prompt block (see [`mint_prompt`]).
pub const MINT_SYSTEM_DELIMITER: &str = "## herdr-board system instructions";
/// Section delimiter naming the card-task half of the agy Mint prompt block
/// (see [`mint_prompt`]).
pub const MINT_TASK_DELIMITER: &str = "## herdr-board card task";

/// Compose the single `agent.prompt` block an agy **Mint** receives: the
/// system instructions first, then the card task, each under its own
/// clearly delimited heading (the same convention as the codex and opencode
/// adapters).
///
/// agy has no system-prompt file equivalent and the task must never ride
/// startup argv, so this block is the *only* prompt transport for a fresh
/// conversation: the integration reads the system instructions and the task
/// from one prompt, with the delimiters making the boundary explicit (and
/// byte-assertable). Resume, retry and same-pane reuse deliver the task
/// alone; a rescue delivers neither.
pub fn mint_prompt(system_prompt: &str, task: &str) -> String {
    format!("{MINT_SYSTEM_DELIMITER}\n{system_prompt}\n\n{MINT_TASK_DELIMITER}\n{task}")
}

/// Build a managed Herdr agy launch:
/// `agy [--model <base>] [--effort E] [--sandbox |
/// --dangerously-skip-permissions] [--conversation <id>]`.
///
/// The model is always the normalized base id (`--model gemini-3.7-flash`);
/// the effort is `--effort low|medium|high`, only when the board effort has
/// an agy spelling (fixed-effort models never send one). Both prompt
/// channels ride outside argv — `initial_prompt` carries the card task,
/// `system_prompt` the column instructions plus the protocol trailer — and
/// the daemon submits them only after the agent is interactive (Mint:
/// `system_prompt + task` delimited block; Resume/retry: task only; rescue
/// re-sends neither). Startup argv therefore contains no prompt text and no
/// `--` delimiter.
pub fn managed_antigravity_invocation(
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    let mut argv = vec!["agy".to_string()];
    if let Some(model) = &settings.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    if let Some(effort) = settings.effort.and_then(effort_value) {
        argv.extend(["--effort".to_string(), effort.to_string()]);
    }
    if let Some(permission) = &settings.permission_mode {
        argv.extend(permission_argv(permission));
    }

    let (session_flags, resulting_session_id) = session_argv(session, minted_uuid)?;
    argv.extend(session_flags);

    Ok(HarnessInvocation {
        agent_kind: Some("agy".to_string()),
        initial_prompt: Some(prompt.to_string()),
        system_prompt: Some(protocol_system_prompt(settings.system_prompt.as_deref())),
        argv,
        env: Vec::new(),
        resulting_session_id,
    })
}
