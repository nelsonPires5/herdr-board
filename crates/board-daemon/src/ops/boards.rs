use super::*;
use board_core::db::{ColumnTarget, ColumnWiring, BOARD_ID};
use board_core::protocol::{
    BoardChangedReason, BoardCreateParams, BoardListParams, BoardSelectParams, ColumnCreateParams,
    TemplateApplyParams, Trigger,
};
pub(super) fn daemon_status(d: &Arc<Daemon>) -> Result<Value> {
    let (active_runs, queued_runs) = {
        let db = d.store.lock();
        (db.count_active_runs()?, db.count_queued_runs()?)
    };
    let herdr_connected = match &d.herdr {
        Some(h) => {
            let mut c = h.clone();
            c.require_supported_protocol().is_ok()
        }
        None => false,
    };
    Ok(json!(DaemonStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        db_path: d.db_path.to_string_lossy().into_owned(),
        herdr_connected,
        active_runs,
        queued_runs,
    }))
}

pub(super) fn board_snapshot(d: &Arc<Daemon>, board_id: i64) -> Result<BoardSnapshot> {
    let db = d.store.lock();
    let mut cards = db.list_cards(board_id)?;
    for card in &mut cards {
        crate::ops::cards::stamp_card_labels(d, card);
    }
    Ok(BoardSnapshot {
        board: db.get_board(board_id)?,
        columns: db.list_columns(board_id)?,
        cards,
        active_runs: db.active_run_summaries(board_id)?,
    })
}

pub(super) fn board_open(d: &Arc<Daemon>, p: BoardOpenParams) -> Result<Value> {
    let board = d.store.lock().open_board(&p.scope_path)?;
    Ok(json!(board_snapshot(d, board.id)?))
}

pub(super) fn board_list(d: &Arc<Daemon>, p: BoardListParams) -> Result<Value> {
    let db = d.store.lock();
    let boards = match p.project_id {
        Some(project_id) => db.list_boards_for_project(project_id)?,
        None => db.list_boards()?,
    };
    Ok(json!(BoardListResult { boards }))
}

/// `board.create`: a named board in a project, auto-selected (creation is an
/// explicit selection, so it also updates recency). The snapshot response
/// lets the caller land directly on the new board.
pub(super) fn board_create(d: &Arc<Daemon>, p: BoardCreateParams) -> Result<Value> {
    let board = d.store.lock().create_board(p.project_id, &p.name)?;
    Ok(json!(board_snapshot(d, board.id)?))
}

/// `board.select`: persist this board — and its project — as the context.
pub(super) fn board_select(d: &Arc<Daemon>, p: BoardSelectParams) -> Result<Value> {
    let (_, board) = d.store.lock().select_board(p.board_id)?;
    Ok(json!(board_snapshot(d, board.id)?))
}

pub(super) fn board_rename(d: &Arc<Daemon>, p: BoardRenameParams) -> Result<Value> {
    let board = d.store.lock().rename_board(p.board_id, &p.name)?;
    // There is no board-renamed reason in protocol v1. ColumnChanged is the
    // existing board-structure refresh signal and, unlike the legacy coarse
    // emit, scopes the refresh to the renamed board.
    d.emit_changed_board(BoardChangedReason::ColumnChanged, board.id, None, None);
    Ok(json!(board))
}

pub(super) fn board_get(d: &Arc<Daemon>, p: BoardGetParams) -> Result<Value> {
    Ok(json!(board_snapshot(d, p.board_id.unwrap_or(BOARD_ID))?))
}

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

/// Apply the pipeline template as one DB unit of work. Preparation and the
/// post-commit event are outside the transaction; no dispatcher wake is needed
/// because a template creates no cards or runs.
pub(super) fn template_apply(d: &Arc<Daemon>, p: TemplateApplyParams) -> Result<Value> {
    if p.name != "pipeline" {
        return Err(Error::BadRequest(format!("unknown template: {}", p.name)));
    }
    let board_id = p.board_id.unwrap_or(BOARD_ID);
    let columns = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.get_board(board_id)?;
        let existing = db.list_columns(board_id)?;
        let cards = db.list_cards(board_id)?;
        if existing.len() != 1 || existing[0].name != "Todo" || !cards.is_empty() {
            return Err(Error::InvalidState(
                "template.apply requires an empty board (only the seed Todo column, no cards)"
                    .into(),
            ));
        }
        let todo = existing[0].id;
        let specs = vec![
            ColumnCreateParams {
                name: "Plan".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(PLAN_PROMPT.into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Execute".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(EXECUTE_PROMPT.into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Review".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(REVIEW_PROMPT.into()),
                model_override: Some("opus".into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Human Review".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Done".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            },
        ];
        let wiring = [
            ColumnWiring {
                column_index: 0,
                on_success: Some(ColumnTarget::Created(1)),
                on_fail: Some(ColumnTarget::Existing(todo)),
            },
            ColumnWiring {
                column_index: 1,
                on_success: Some(ColumnTarget::Created(2)),
                on_fail: None,
            },
            ColumnWiring {
                column_index: 2,
                on_success: Some(ColumnTarget::Created(3)),
                on_fail: Some(ColumnTarget::Created(1)),
            },
        ];
        db.apply_template_columns_uow(board_id, &specs, &wiring)?
    };
    d.emit_changed_board(BoardChangedReason::ColumnChanged, board_id, None, None);
    Ok(json!(columns))
}

// -- columns ----------------------------------------------------------------
