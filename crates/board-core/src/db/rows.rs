use rusqlite::{types::Type, Error as SqliteError, Result as SqliteResult, Row};

use super::conv_err;
use crate::model::{Board, Card, Column, Comment, Run};
use crate::protocol::{AwaitingReason, CardStatus, Effort, RunOutcome, SpaceKind, Trigger};
use crate::{Error, Result};

pub(super) fn opt<T>(r: SqliteResult<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::Sqlite(e)),
    }
}

pub(super) fn row_to_board(row: &Row) -> SqliteResult<Board> {
    Ok(Board {
        id: row.get("id")?,
        name: row.get("name")?,
        scope_path: row.get("scope_path")?,
    })
}

pub(super) fn row_to_column(row: &Row) -> SqliteResult<Column> {
    let trigger_s: String = row.get("trigger")?;
    let fresh: i64 = row.get("fresh_session")?;
    Ok(Column {
        id: row.get("id")?,
        board_id: row.get("board_id")?,
        name: row.get("name")?,
        position: row.get("position")?,
        system_prompt: row.get("system_prompt")?,
        trigger: Trigger::parse_str(&trigger_s).ok_or_else(|| conv_err("trigger"))?,
        on_success_column_id: row.get("on_success_column_id")?,
        on_fail_column_id: row.get("on_fail_column_id")?,
        fresh_session: fresh != 0,
        harness_override: row.get("harness_override")?,
        model_override: row.get("model_override")?,
        effort_override: row.get("effort_override")?,
        permission_override: row.get("permission_override")?,
        timeout_minutes: row.get("timeout_minutes")?,
    })
}

pub(super) fn row_to_card(row: &Row) -> SqliteResult<Card> {
    let effort_s: Option<String> = row.get("effort")?;
    let effort = match effort_s {
        Some(s) => Some(Effort::parse_str(&s).ok_or_else(|| conv_err("effort"))?),
        None => None,
    };
    let space_s: String = row.get("space_kind")?;
    let status_s: String = row.get("status")?;
    let reason_s: Option<String> = row.get("awaiting_reason")?;
    let awaiting_reason = match reason_s {
        Some(s) => Some(AwaitingReason::parse_str(&s).ok_or_else(|| conv_err("awaiting_reason"))?),
        None => None,
    };
    Ok(Card {
        id: row.get("id")?,
        board_id: row.get("board_id")?,
        column_id: row.get("column_id")?,
        position: row.get("position")?,
        title: row.get("title")?,
        description: row.get("description")?,
        harness: row.get("harness")?,
        model: row.get("model")?,
        effort,
        permission_mode: row.get("permission_mode")?,
        session: row.get("session")?,
        space_kind: SpaceKind::parse_str(&space_s).ok_or_else(|| conv_err("space_kind"))?,
        space_ref: row.get("space_ref")?,
        space_cwd: row.get("space_cwd")?,
        status: CardStatus::parse_str(&status_s).ok_or_else(|| conv_err("status"))?,
        awaiting_reason,
        session_id: row.get("session_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        archived_at: row.get("archived_at")?,
    })
}

pub(super) fn row_to_comment(row: &Row) -> SqliteResult<Comment> {
    Ok(Comment {
        id: row.get("id")?,
        card_id: row.get("card_id")?,
        author: row.get("author")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
    })
}

pub(super) fn row_to_run(row: &Row) -> SqliteResult<Run> {
    let outcome_s: Option<String> = row.get("outcome")?;
    let outcome = match outcome_s {
        Some(s) => Some(RunOutcome::parse_str(&s).ok_or_else(|| conv_err("outcome"))?),
        None => None,
    };
    Ok(Run {
        id: row.get("id")?,
        card_id: row.get("card_id")?,
        column_id: row.get("column_id")?,
        harness: row.get("harness")?,
        argv_json: row.get("argv_json")?,
        prompt_snapshot: row.get("prompt_snapshot")?,
        system_prompt_snapshot: row.get("system_prompt_snapshot")?,
        launch_spec: row
            .get::<_, Option<String>>("launch_spec_json")?
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|e| SqliteError::FromSqlConversionFailure(0, Type::Text, Box::new(e)))
            })
            .transpose()?,
        herdr_workspace_id: row.get("herdr_workspace_id")?,
        herdr_pane_id: row.get("herdr_pane_id")?,
        session_id: row.get("session_id")?,
        session: row.get("session")?,
        started_at: row.get("started_at")?,
        timeout_deadline_at_ms: row.get("timeout_deadline_at_ms")?,
        timeout_paused_at_ms: row.get("timeout_paused_at_ms")?,
        ended_at: row.get("ended_at")?,
        outcome,
        result_summary: row.get("result_summary")?,
        log_path: row.get("log_path")?,
    })
}
