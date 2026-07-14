//! Harness adapters: turn resolved settings + a prompt into `(argv, env)`.
//!
//! Two kinds:
//! - builtin `claude` — flags exactly per `docs/protocol.md`, prompt positional.
//! - config-defined harnesses — an argv template with `{model}`/`{effort}`/
//!   `{permission_mode}` placeholders; prompt via `BOARD_PROMPT` env.

use crate::config::Config;
use crate::prompt::EffectiveSettings;

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
    pub argv: Vec<String>,
    /// Extra env pairs the harness itself needs (e.g. `BOARD_PROMPT` for custom
    /// harnesses). The daemon adds `BOARD_CARD_ID`/`BOARD_RUN_ID`/`BOARD_SOCKET`.
    pub env: Vec<(String, String)>,
}

/// Errors from building a harness invocation.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    #[error("unknown harness: {0}")]
    UnknownHarness(String),
    /// `SessionPlan::Mint` was requested but no minted uuid was supplied.
    #[error("mint session requested without a minted uuid")]
    MissingMintedSession,
}

/// Board-protocol trailer appended to every builtin-claude system prompt: the
/// close-out contract must not depend on users remembering it in each column's
/// prompt. `board comment`/`done` read `$BOARD_CARD_ID` from the run env.
pub const BOARD_PROTOCOL_TRAILER: &str = "\
## herdr-board protocol
You are running a herdr-board card ($BOARD_CARD_ID is preset). When this stage's goal \
is met you MUST finish with exactly two commands: first `board comment \"<your results, \
files touched, findings>\"`, then `board done --outcome ok`. If the stage goal was NOT \
met, use `board done --outcome fail --summary \"<why>\"` instead. Always comment before \
done. Never use `board move`/`cancel`/`retry` on your own card.";

/// Build the argv for the builtin `claude` harness.
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
    let system_prompt = match &settings.system_prompt {
        Some(sp) => format!("{sp}\n\n{BOARD_PROTOCOL_TRAILER}"),
        None => BOARD_PROTOCOL_TRAILER.to_string(),
    };
    argv.push("--append-system-prompt".to_string());
    argv.push(system_prompt);
    argv.push("--allowedTools".to_string());
    argv.push("Bash(board:*)".to_string());

    match session {
        SessionPlan::Mint => {
            let uuid = minted_uuid.ok_or(HarnessError::MissingMintedSession)?;
            argv.push("--session-id".to_string());
            argv.push(uuid.to_string());
        }
        SessionPlan::Resume(id) => {
            argv.push("--resume".to_string());
            argv.push(id.clone());
        }
        SessionPlan::Fork(id) => {
            argv.push("--resume".to_string());
            argv.push(id.clone());
            argv.push("--fork-session".to_string());
        }
    }

    argv.push("--".to_string());
    argv.push(prompt.to_string());
    Ok(argv)
}

/// Build a full invocation for `harness_name`, using the builtin `claude`
/// adapter or a config-defined harness template.
pub fn build_invocation(
    harness_name: &str,
    config: &Config,
    settings: &EffectiveSettings,
    session: &SessionPlan,
    minted_uuid: Option<&str>,
    prompt: &str,
) -> Result<HarnessInvocation, HarnessError> {
    if harness_name == "claude" {
        let argv = claude_argv(settings, session, minted_uuid, prompt)?;
        return Ok(HarnessInvocation {
            argv,
            env: Vec::new(),
        });
    }

    let def = config
        .harness
        .get(harness_name)
        .ok_or_else(|| HarnessError::UnknownHarness(harness_name.to_string()))?;

    let argv = substitute_template(&def.argv, settings);

    let mut env = vec![("BOARD_PROMPT".to_string(), prompt.to_string())];
    if let Some(sp) = &settings.system_prompt {
        env.push(("BOARD_SYSTEM_PROMPT".to_string(), sp.clone()));
    }

    Ok(HarnessInvocation { argv, env })
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
