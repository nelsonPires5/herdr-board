//! `template.apply` — builds the example pipeline from `docs/design.md` §4 onto
//! an otherwise-empty board (seed `Todo` column, no cards).

use std::sync::Arc;

use board_core::db::BOARD_ID;
use board_core::protocol::{ColumnCreateParams, ColumnUpdateParams, Trigger};
use board_core::{Error, Result};
use serde_json::{json, Value};

use crate::state::Daemon;

const PLAN_PROMPT: &str =
    "You are in the PLAN stage. Use /quick-planner style planning: produce a written
implementation plan and save it under docs/plans/ (or .plans/). Do not write code.
When finished you MUST run:
  board comment $BOARD_CARD_ID \"Plan ready at <filepath>. <3-line summary>\"
  board done $BOARD_CARD_ID --outcome ok";

const EXECUTE_PROMPT: &str =
    "You are in the EXECUTE stage. Implement the plan referenced in the card comments.
Run tests. When finished:
  board comment $BOARD_CARD_ID \"<what changed, files touched, test results>\"
  board done $BOARD_CARD_ID --outcome ok    # or --outcome fail with reasons";

const REVIEW_PROMPT: &str =
    "You are in the REVIEW stage. Review the diff against the card description and the
plan/execution comments. Be adversarial. Then:
  board comment $BOARD_CARD_ID \"<verdict + findings>\"
  board done $BOARD_CARD_ID --outcome ok    # ok = ship to human; fail = back to Execute";

/// Apply the named template. Only `pipeline` exists.
pub fn apply(d: &Arc<Daemon>, p: board_core::protocol::TemplateApplyParams) -> Result<Value> {
    if p.name != "pipeline" {
        return Err(Error::BadRequest(format!("unknown template: {}", p.name)));
    }

    // Precondition: only the seed Todo column, and no cards.
    {
        let db = d.store.lock();
        let cols = db.list_columns(BOARD_ID)?;
        let cards = db.list_cards(BOARD_ID)?;
        let only_seed = cols.len() == 1 && cols[0].name == "Todo";
        if !only_seed || !cards.is_empty() {
            return Err(Error::InvalidState(
                "template.apply requires an empty board (only the seed Todo column, no cards)"
                    .into(),
            ));
        }
    }

    // Create the new columns (transitions resolved in a second pass).
    let mk = |name: &str, trigger: Trigger, system_prompt: Option<&str>, model: Option<&str>| {
        ColumnCreateParams {
            name: name.to_string(),
            trigger: Some(trigger),
            system_prompt: system_prompt.map(str::to_string),
            model_override: model.map(str::to_string),
            ..Default::default()
        }
    };

    {
        let db = d.store.lock();
        db.create_column(&mk("Plan", Trigger::Auto, Some(PLAN_PROMPT), None))?;
        db.create_column(&mk("Execute", Trigger::Auto, Some(EXECUTE_PROMPT), None))?;
        db.create_column(&mk(
            "Review",
            Trigger::Auto,
            Some(REVIEW_PROMPT),
            Some("opus"),
        ))?;
        db.create_column(&mk("Human Review", Trigger::Manual, None, None))?;
        db.create_column(&mk("Done", Trigger::Manual, None, None))?;
    }

    // Resolve name → id and wire transitions.
    let cols = d.store.lock().list_columns(BOARD_ID)?;
    let id_of = |name: &str| cols.iter().find(|c| c.name == name).map(|c| c.id);

    let todo = id_of("Todo");
    let plan = id_of("Plan");
    let execute = id_of("Execute");
    let review = id_of("Review");
    let human = id_of("Human Review");

    let wire = |id: Option<i64>, on_success: Option<i64>, on_fail: Option<i64>| -> Result<()> {
        if let Some(id) = id {
            d.store.lock().update_column(&ColumnUpdateParams {
                id,
                on_success_column_id: on_success,
                on_fail_column_id: on_fail,
                ..Default::default()
            })?;
        }
        Ok(())
    };

    // Plan: ok→Execute, fail→Todo
    wire(plan, execute, todo)?;
    // Execute: ok→Review
    wire(execute, review, None)?;
    // Review: ok→Human Review, fail→Execute
    wire(review, human, execute)?;

    d.emit_changed(
        board_core::protocol::BoardChangedReason::ColumnChanged,
        None,
        None,
    );
    Ok(json!(d.store.lock().list_columns(BOARD_ID)?))
}
