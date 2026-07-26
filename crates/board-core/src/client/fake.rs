use serde_json::Value;

use crate::db::{Db, FinalizeEffects, FinalizeRun, BOARD_ID};

use crate::engine;

use crate::protocol::{
    BoardGetParams, BoardListResult, BoardOpenParams, BoardRenameParams, BoardSnapshot,
    CardArchiveParams, CardCreateParams, CardDetail, CardListParams, CardMoveParams,
    CardUpdateParams, ColumnCreateParams, ColumnDeleteParams, ColumnReorderParams,
    ColumnUpdateParams, CommentAddParams, CommentDeleteParams, CommentGetParams,
    CommentHistoryParams, CommentUpdateParams, DeletedResult, Event, RunActionResult,
    RunDoneParams, RunFocusParams, RunFocusResult,
};

use super::BoardClient;

/// In-memory board state machine for TUI tests. Backed by an in-memory
/// SQLite db, so CRUD/move/positions/comments behave exactly like the real
/// store — but there is no dispatch: moving into an auto column just moves.
pub struct FakeBoardClient {
    db: Db,
}

/// Validate an agent actor exactly as the daemon does. The fake harness may
/// invoke the board before its lifecycle row exists, so retain that narrow
/// compatibility path only for fake cards with no open durable run.
fn require_agent_run(db: &Db, actor_run_id: i64, card_id: i64, author: &str) -> anyhow::Result<()> {
    let expected_author = format!("agent:{actor_run_id}");
    if author != expected_author {
        anyhow::bail!("agent run {actor_run_id} may only act as {expected_author}");
    }

    match db.get_run(actor_run_id) {
        Ok(run) => {
            if run.card_id != card_id {
                anyhow::bail!("agent run {actor_run_id} does not belong to comment card {card_id}");
            }
            if run.ended_at.is_some() {
                anyhow::bail!("agent run {actor_run_id} is no longer open");
            }
            Ok(())
        }
        Err(crate::Error::NotFound(_)) => {
            let card = db
                .get_card(card_id)?
                .ok_or_else(|| anyhow::anyhow!("card {card_id} not found"))?;
            if card.harness == "fake" && db.open_run_for_card(card_id)?.is_none() {
                Ok(())
            } else {
                anyhow::bail!("run {actor_run_id} not found");
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn comment_for_mutation(
    db: &Db,
    id: i64,
    actor_run_id: Option<i64>,
) -> anyhow::Result<crate::model::CommentRecord> {
    let comment = db
        .get_comment(id)?
        .ok_or_else(|| anyhow::anyhow!("comment {id} not found"))?;
    if let Some(run_id) = actor_run_id {
        require_agent_run(db, run_id, comment.card_id, &comment.author)?;
    }
    Ok(comment)
}

impl FakeBoardClient {
    pub fn new() -> anyhow::Result<FakeBoardClient> {
        Ok(FakeBoardClient {
            db: Db::open_in_memory()?,
        })
    }

    /// Direct access to the underlying store (tests may seed runs/comments).
    pub fn db(&self) -> &Db {
        &self.db
    }
}

impl BoardClient for FakeBoardClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let db = &self.db;
        let v = match method {
            "board.get" => {
                let p: BoardGetParams = serde_json::from_value(params)?;
                let board_id = p.board_id.unwrap_or(BOARD_ID);
                let snap = BoardSnapshot {
                    board: db.get_board(board_id)?,
                    columns: db.list_columns(board_id)?,
                    cards: db.list_cards(board_id)?,
                    active_runs: db.active_run_summaries(board_id)?,
                };
                serde_json::to_value(snap)?
            }
            "board.open" => {
                let p: BoardOpenParams = serde_json::from_value(params)?;
                let board = db.open_board(&p.scope_path)?;
                serde_json::to_value(BoardSnapshot {
                    columns: db.list_columns(board.id)?,
                    cards: db.list_cards(board.id)?,
                    active_runs: db.active_run_summaries(board.id)?,
                    board,
                })?
            }
            "board.list" => serde_json::to_value(BoardListResult {
                boards: db.list_boards()?,
            })?,
            "board.rename" => {
                let p: BoardRenameParams = serde_json::from_value(params)?;
                serde_json::to_value(db.rename_board(p.board_id, &p.name)?)?
            }
            "column.create" => {
                let p: ColumnCreateParams = serde_json::from_value(params)?;
                serde_json::to_value(db.create_column(&p)?)?
            }
            "column.update" => {
                let p: ColumnUpdateParams = serde_json::from_value(params)?;
                serde_json::to_value(db.update_column(&p)?)?
            }
            "column.reorder" => {
                let p: ColumnReorderParams = serde_json::from_value(params)?;
                serde_json::to_value(db.reorder_column(p.id, p.position)?)?
            }
            "column.delete" => {
                let p: ColumnDeleteParams = serde_json::from_value(params)?;
                let cards = db.list_cards_in_column(p.id)?;
                let has_open_run = db.column_has_open_run(p.id)?;
                engine::validate_column_delete(!cards.is_empty(), has_open_run, p.move_cards_to)?;
                db.delete_column(p.id, p.move_cards_to)?;
                serde_json::to_value(DeletedResult { deleted: true })?
            }
            "card.create" => {
                let p: CardCreateParams = serde_json::from_value(params)?;
                serde_json::to_value(db.create_card(&p)?)?
            }
            "card.update" => {
                let p: CardUpdateParams = serde_json::from_value(params)?;
                serde_json::to_value(db.update_card(&p)?)?
            }
            "card.delete" => {
                let id = params["id"].as_i64().unwrap_or_default();
                db.delete_card(id)?;
                serde_json::to_value(DeletedResult { deleted: true })?
            }
            "card.archive" => {
                let p: CardArchiveParams = serde_json::from_value(params)?;
                let card = db
                    .get_card(p.id)?
                    .ok_or_else(|| anyhow::anyhow!("card {} not found", p.id))?;
                engine::validate_card_archive(card.status)?;
                serde_json::to_value(db.set_card_archived(p.id, p.archived)?)?
            }
            "card.move" => {
                let p: CardMoveParams = serde_json::from_value(params)?;
                let card = db
                    .get_card(p.id)?
                    .ok_or_else(|| anyhow::anyhow!("card {} not found", p.id))?;
                if card.archived_at.is_some() {
                    anyhow::bail!("archived card must be restored before moving");
                }
                let card = match p.board_id {
                    // Cross-board transfer only when an explicit destination
                    // board is named and differs from the current one.
                    Some(bid) if bid != card.board_id => {
                        db.transfer_card(p.id, bid, p.column_id, p.position)?
                    }
                    _ => db.move_card(p.id, p.column_id, p.position)?,
                };
                serde_json::to_value(card)?
            }
            "card.get" => {
                let id = params["id"].as_i64().unwrap_or_default();
                let card = db
                    .get_card(id)?
                    .ok_or_else(|| anyhow::anyhow!("card {id} not found"))?;
                let detail = CardDetail {
                    card,
                    comments: db.list_comments(id)?,
                    runs: db.list_runs(id)?,
                };
                serde_json::to_value(detail)?
            }
            "card.list" => {
                let p: CardListParams = serde_json::from_value(params)?;
                let board_id = p.board_id.unwrap_or(BOARD_ID);
                let visibility = p
                    .visibility
                    .unwrap_or(crate::protocol::CardVisibility::Active);
                let cards = match p.column_id {
                    Some(c) => {
                        let column = db
                            .get_column(c)?
                            .ok_or_else(|| anyhow::anyhow!("column {c} not found"))?;
                        if column.board_id != board_id {
                            anyhow::bail!("column {c} belongs to another board");
                        }
                        db.list_cards_in_column_visible(c, visibility)?
                    }
                    None => db.list_cards_visible(board_id, visibility)?,
                };
                serde_json::to_value(cards)?
            }
            "run.done" => {
                let p: RunDoneParams = serde_json::from_value(params)?;
                let run = db
                    .active_run_for_card(p.card_id)?
                    .ok_or_else(|| anyhow::anyhow!("no active run for card {}", p.card_id))?;
                let card = db
                    .get_card(p.card_id)?
                    .ok_or_else(|| anyhow::anyhow!("card {} not found", p.card_id))?;
                let column = db
                    .get_column(run.column_id)?
                    .ok_or_else(|| anyhow::anyhow!("column {} not found", run.column_id))?;
                let columns = db.list_columns(card.board_id)?;
                let decision = engine::decide_transition(&column, &columns, p.outcome, None);

                let FinalizeEffects {
                    card,
                    finished_run: run,
                    next_run: _,
                } = db.finalize_run_uow(&FinalizeRun {
                    run_id: run.id,
                    outcome: p.outcome,
                    summary: p.summary.as_deref(),
                    comments: &[("system", &decision.system_comment)],
                    target_column_id: decision.target_column_id,
                    final_status: decision.new_status,
                    final_awaiting_reason: None,
                    next: None,
                })?;
                serde_json::to_value(RunActionResult { run, card })?
            }
            "run.focus" => {
                let p: RunFocusParams = serde_json::from_value(params)?;
                let run = db
                    .latest_run_with_pane(p.card_id)?
                    .ok_or_else(|| anyhow::anyhow!("no run with an accessible pane"))?;
                serde_json::to_value(RunFocusResult {
                    run_id: run.id,
                    pane_id: run
                        .herdr_pane_id
                        .ok_or_else(|| anyhow::anyhow!("run has no pane"))?,
                })?
            }
            "comment.add" => {
                let p: CommentAddParams = serde_json::from_value(params)?;
                let author = match p.actor_run_id {
                    Some(run_id) => {
                        let expected = format!("agent:{run_id}");
                        if let Some(author) = p.author.as_deref() {
                            require_agent_run(db, run_id, p.card_id, author)?;
                        } else {
                            require_agent_run(db, run_id, p.card_id, &expected)?;
                        }
                        expected
                    }
                    None => p.author.unwrap_or_else(|| "user".into()),
                };
                serde_json::to_value(db.add_comment(p.card_id, &author, &p.body)?)?
            }
            "comment.get" => {
                let p: CommentGetParams = serde_json::from_value(params)?;
                serde_json::to_value(
                    db.get_comment(p.id)?
                        .ok_or_else(|| anyhow::anyhow!("comment {} not found", p.id))?,
                )?
            }
            "comment.update" => {
                let p: CommentUpdateParams = serde_json::from_value(params)?;
                comment_for_mutation(db, p.id, p.actor_run_id)?;
                serde_json::to_value(db.update_comment(p.id, &p.body)?)?
            }
            "comment.delete" => {
                let p: CommentDeleteParams = serde_json::from_value(params)?;
                comment_for_mutation(db, p.id, p.actor_run_id)?;
                db.soft_delete_comment(p.id)?;
                serde_json::to_value(DeletedResult { deleted: true })?
            }
            "comment.history" => {
                let p: CommentHistoryParams = serde_json::from_value(params)?;
                serde_json::to_value(db.list_comment_history(p.id)?)?
            }
            other => anyhow::bail!("FakeBoardClient: unsupported method {other}"),
        };
        Ok(v)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        Ok(Box::new(std::iter::empty()))
    }
}
