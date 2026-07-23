use serde_json::Value;

use crate::db::{Db, FinalizeEffects, FinalizeRun, BOARD_ID};

use crate::engine;

use crate::protocol::{
    BoardGetParams, BoardListResult, BoardOpenParams, BoardSnapshot, CardArchiveParams,
    CardCreateParams, CardDetail, CardListParams, CardMoveParams, CardUpdateParams,
    ColumnCreateParams, ColumnDeleteParams, ColumnReorderParams, ColumnUpdateParams,
    CommentAddParams, DeletedResult, Event, RunActionResult, RunDoneParams, RunFocusParams,
    RunFocusResult,
};

use super::BoardClient;

/// In-memory board state machine for TUI tests. Backed by an in-memory
/// SQLite db, so CRUD/move/positions/comments behave exactly like the real
/// store — but there is no dispatch: moving into an auto column just moves.
pub struct FakeBoardClient {
    db: Db,
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
                serde_json::to_value(db.move_card(p.id, p.column_id, p.position)?)?
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
                let cards = match p.column_id {
                    Some(c) => {
                        let column = db
                            .get_column(c)?
                            .ok_or_else(|| anyhow::anyhow!("column {c} not found"))?;
                        if column.board_id != board_id {
                            anyhow::bail!("column {c} belongs to another board");
                        }
                        db.list_cards_in_column(c)?
                    }
                    None => db.list_cards(board_id)?,
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
                let author = p.author.as_deref().unwrap_or("user");
                serde_json::to_value(db.add_comment(p.card_id, author, &p.body)?)?
            }
            other => anyhow::bail!("FakeBoardClient: unsupported method {other}"),
        };
        Ok(v)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        Ok(Box::new(std::iter::empty()))
    }
}
