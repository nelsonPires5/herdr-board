//! Prompt assembly and effective-settings resolution (pure).

use crate::engine::{self, ValidationError};
use crate::model::{Card, Column, Comment};
use crate::protocol::Effort;

/// Number of trailing comments folded into the run prompt.
pub const MAX_PROMPT_COMMENTS: usize = 20;

/// Close-out reminder appended to every run prompt. The system-prompt trailer
/// alone is not enough: weaker models drift after a few turns, and the end of
/// the user prompt is where instruction-following is strongest (field-tested:
/// haiku completed a stage but never called `board done` without this).
pub const PROMPT_CLOSEOUT: &str = "(When the stage goal is met, finish with \
`board comment \"<results>\"` then `board done --outcome ok` — or `--outcome fail` \
if the goal was not met.)";

/// Build the run prompt: the card description, plus a `## Card comments` section
/// listing the last [`MAX_PROMPT_COMMENTS`] comments as `author (ts): body`
/// (omitted when there are none), plus the [`PROMPT_CLOSEOUT`] reminder.
pub fn assemble_prompt(description: &str, comments: &[Comment]) -> String {
    let mut out = String::from(description);
    if !comments.is_empty() {
        let start = comments.len().saturating_sub(MAX_PROMPT_COMMENTS);
        out.push_str("\n\n## Card comments\n");
        for c in &comments[start..] {
            out.push_str(&format!("{} ({}): {}\n", c.author, c.created_at, c.body));
        }
        out.truncate(out.trim_end_matches('\n').len());
    }
    out.push_str("\n\n");
    out.push_str(PROMPT_CLOSEOUT);
    out
}

/// Settings resolved for a run: the card's values, overridden by any column
/// `*_override` that is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission_mode: Option<String>,
    pub system_prompt: Option<String>,
    pub fresh_session: bool,
    pub timeout_minutes: Option<i64>,
}

/// Resolve effective settings for running `card` in `column`.
///
/// A column override wins over the card value when present. A `bypassPermissions`
/// column override is rejected (per-card opt-in only).
pub fn effective_settings(
    card: &Card,
    column: &Column,
) -> Result<EffectiveSettings, ValidationError> {
    engine::validate_column_permission_override(column.permission_override.as_deref())?;

    let harness = column
        .harness_override
        .clone()
        .unwrap_or_else(|| card.harness.clone());

    let model = column.model_override.clone().or_else(|| card.model.clone());

    let effort = match &column.effort_override {
        Some(s) => Effort::parse_str(s),
        None => card.effort,
    };

    let permission_mode = column
        .permission_override
        .clone()
        .or_else(|| card.permission_mode.clone());

    Ok(EffectiveSettings {
        harness,
        model,
        effort,
        permission_mode,
        system_prompt: column.system_prompt.clone(),
        fresh_session: column.fresh_session,
        timeout_minutes: column.timeout_minutes,
    })
}
