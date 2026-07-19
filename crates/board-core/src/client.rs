//! Blocking NDJSON client over a Unix socket, plus a typed convenience layer and
//! an in-memory `FakeBoardClient` (behind the `fake-client` feature) for TUI tests.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::model::{Card, Column, Comment};
use crate::protocol::{
    BoardGetParams, BoardListResult, BoardOpenParams, BoardSnapshot, CardArchiveParams,
    CardCreateParams, CardDetail, CardListParams, CardMoveParams, CardUpdateParams,
    ColumnCreateParams, ColumnDeleteParams, ColumnReorderParams, ColumnUpdateParams,
    CommentAddParams, DaemonStatus, DeletedResult, Event, Request, Response, RunActionResult,
    RunCardParams, RunDoneParams, RunFocusParams, RunFocusResult, RunOutcome, StopResult,
    TemplateApplyParams,
};

/// Blocking client to boardd. Object-safe so the TUI can hold `Box<dyn BoardClient>`.
///
/// Only [`BoardClient::call`] and [`BoardClient::subscribe`] are abstract; the typed
/// wrappers are non-generic default methods (keeping the trait object-safe).
pub trait BoardClient {
    /// Send one request and return its `result` payload (or an error).
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value>;

    /// Open an event subscription (usually a dedicated connection).
    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>>;

    // -- typed wrappers ------------------------------------------------------

    fn daemon_status(&mut self) -> anyhow::Result<DaemonStatus> {
        Ok(serde_json::from_value(
            self.call("daemon.status", json!({}))?,
        )?)
    }
    fn daemon_stop(&mut self) -> anyhow::Result<StopResult> {
        Ok(serde_json::from_value(
            self.call("daemon.stop", json!({}))?,
        )?)
    }

    fn board_get(&mut self) -> anyhow::Result<BoardSnapshot> {
        Ok(serde_json::from_value(self.call("board.get", json!({}))?)?)
    }
    fn board_get_by_id(&mut self, board_id: i64) -> anyhow::Result<BoardSnapshot> {
        let params = BoardGetParams {
            board_id: Some(board_id),
        };
        Ok(serde_json::from_value(
            self.call("board.get", serde_json::to_value(params)?)?,
        )?)
    }
    fn board_open(&mut self, scope_path: &str) -> anyhow::Result<BoardSnapshot> {
        let params = BoardOpenParams {
            scope_path: scope_path.to_string(),
        };
        Ok(serde_json::from_value(
            self.call("board.open", serde_json::to_value(params)?)?,
        )?)
    }
    fn board_list(&mut self) -> anyhow::Result<BoardListResult> {
        Ok(serde_json::from_value(self.call("board.list", json!({}))?)?)
    }

    fn column_create(&mut self, p: &ColumnCreateParams) -> anyhow::Result<Column> {
        Ok(serde_json::from_value(
            self.call("column.create", serde_json::to_value(p)?)?,
        )?)
    }
    fn column_update(&mut self, p: &ColumnUpdateParams) -> anyhow::Result<Column> {
        Ok(serde_json::from_value(
            self.call("column.update", serde_json::to_value(p)?)?,
        )?)
    }
    fn column_reorder(&mut self, id: i64, position: i64) -> anyhow::Result<Vec<Column>> {
        let p = ColumnReorderParams { id, position };
        Ok(serde_json::from_value(
            self.call("column.reorder", serde_json::to_value(p)?)?,
        )?)
    }
    fn column_delete(
        &mut self,
        id: i64,
        move_cards_to: Option<i64>,
    ) -> anyhow::Result<DeletedResult> {
        let p = ColumnDeleteParams { id, move_cards_to };
        Ok(serde_json::from_value(
            self.call("column.delete", serde_json::to_value(p)?)?,
        )?)
    }
    fn template_apply(&mut self, name: &str) -> anyhow::Result<Vec<Column>> {
        self.template_apply_for_board(name, None)
    }
    fn template_apply_for_board(
        &mut self,
        name: &str,
        board_id: Option<i64>,
    ) -> anyhow::Result<Vec<Column>> {
        let p = TemplateApplyParams {
            name: name.to_string(),
            board_id,
        };
        Ok(serde_json::from_value(
            self.call("template.apply", serde_json::to_value(p)?)?,
        )?)
    }

    fn card_create(&mut self, p: &CardCreateParams) -> anyhow::Result<Card> {
        Ok(serde_json::from_value(
            self.call("card.create", serde_json::to_value(p)?)?,
        )?)
    }
    fn card_update(&mut self, p: &CardUpdateParams) -> anyhow::Result<Card> {
        Ok(serde_json::from_value(
            self.call("card.update", serde_json::to_value(p)?)?,
        )?)
    }
    fn card_delete(&mut self, id: i64) -> anyhow::Result<DeletedResult> {
        Ok(serde_json::from_value(
            self.call("card.delete", json!({ "id": id }))?,
        )?)
    }
    fn card_archive(&mut self, id: i64, archived: bool) -> anyhow::Result<Card> {
        let p = CardArchiveParams { id, archived };
        Ok(serde_json::from_value(
            self.call("card.archive", serde_json::to_value(p)?)?,
        )?)
    }
    fn card_move(&mut self, p: &CardMoveParams) -> anyhow::Result<Card> {
        Ok(serde_json::from_value(
            self.call("card.move", serde_json::to_value(p)?)?,
        )?)
    }
    fn card_get(&mut self, id: i64) -> anyhow::Result<CardDetail> {
        Ok(serde_json::from_value(
            self.call("card.get", json!({ "id": id }))?,
        )?)
    }
    fn card_list(&mut self, column_id: Option<i64>) -> anyhow::Result<Vec<Card>> {
        self.card_list_for_board(None, column_id)
    }
    fn card_list_for_board(
        &mut self,
        board_id: Option<i64>,
        column_id: Option<i64>,
    ) -> anyhow::Result<Vec<Card>> {
        let p = CardListParams {
            board_id,
            column_id,
        };
        Ok(serde_json::from_value(
            self.call("card.list", serde_json::to_value(p)?)?,
        )?)
    }

    fn comment_add(
        &mut self,
        card_id: i64,
        body: &str,
        author: Option<&str>,
    ) -> anyhow::Result<Comment> {
        let p = CommentAddParams {
            card_id,
            body: body.to_string(),
            author: author.map(str::to_string),
        };
        Ok(serde_json::from_value(
            self.call("comment.add", serde_json::to_value(p)?)?,
        )?)
    }

    fn run_done(
        &mut self,
        card_id: i64,
        outcome: RunOutcome,
        summary: Option<&str>,
    ) -> anyhow::Result<RunActionResult> {
        let p = RunDoneParams {
            card_id,
            outcome,
            summary: summary.map(str::to_string),
        };
        Ok(serde_json::from_value(
            self.call("run.done", serde_json::to_value(p)?)?,
        )?)
    }
    fn run_cancel(&mut self, card_id: i64) -> anyhow::Result<RunActionResult> {
        let p = RunCardParams { card_id };
        Ok(serde_json::from_value(
            self.call("run.cancel", serde_json::to_value(p)?)?,
        )?)
    }
    fn run_retry(&mut self, card_id: i64) -> anyhow::Result<RunActionResult> {
        let p = RunCardParams { card_id };
        Ok(serde_json::from_value(
            self.call("run.retry", serde_json::to_value(p)?)?,
        )?)
    }
    fn run_focus(&mut self, card_id: i64, origin_socket: &str) -> anyhow::Result<RunFocusResult> {
        let p = RunFocusParams {
            card_id,
            origin_socket: origin_socket.to_string(),
        };
        Ok(serde_json::from_value(
            self.call("run.focus", serde_json::to_value(p)?)?,
        )?)
    }
}

/// The real Unix-socket client.
pub struct UnixClient {
    path: PathBuf,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl UnixClient {
    pub fn connect(path: &Path) -> anyhow::Result<UnixClient> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(UnixClient {
            path: path.to_path_buf(),
            reader,
            writer: stream,
            next_id: 0,
        })
    }

    pub fn connect_default() -> anyhow::Result<UnixClient> {
        UnixClient::connect(&crate::paths::socket_path())
    }
}

impl BoardClient for UnixClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let req = Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;

        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                anyhow::bail!("boardd connection closed");
            }
            // Skip anything that isn't a matching response (e.g. event lines).
            let resp: Response = match serde_json::from_str(buf.trim_end()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if resp.id != id {
                continue;
            }
            if let Some(err) = resp.error {
                anyhow::bail!("boardd error {}: {}", err.code, err.message);
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        let stream = UnixStream::connect(&self.path)?;
        let mut writer = stream.try_clone()?;
        let reader = BufReader::new(stream);
        let req = Request {
            id: "sub".to_string(),
            method: "events.subscribe".to_string(),
            params: json!({}),
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        writer.flush()?;
        Ok(Box::new(EventStream { reader }))
    }
}

/// Iterator over streamed events; skips the subscribe ack and any non-event lines.
pub struct EventStream {
    reader: BufReader<UnixStream>,
}

impl Iterator for EventStream {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            let mut buf = String::new();
            match self.reader.read_line(&mut buf) {
                Ok(0) => return None,
                Ok(_) => {
                    if let Ok(ev) = serde_json::from_str::<Event>(buf.trim_end()) {
                        return Some(ev);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FakeBoardClient
// ---------------------------------------------------------------------------

#[cfg(feature = "fake-client")]
mod fake {
    use super::*;
    use crate::db::{Db, BOARD_ID};
    use crate::engine;

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
                    };
                    serde_json::to_value(snap)?
                }
                "board.open" => {
                    let p: BoardOpenParams = serde_json::from_value(params)?;
                    let board = db.open_board(&p.scope_path)?;
                    serde_json::to_value(BoardSnapshot {
                        columns: db.list_columns(board.id)?,
                        cards: db.list_cards(board.id)?,
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
                    engine::validate_column_delete(
                        !cards.is_empty(),
                        has_open_run,
                        p.move_cards_to,
                    )?;
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

                    let run = db.finish_run(run.id, p.outcome, p.summary.as_deref())?;
                    db.add_comment(p.card_id, "system", &decision.system_comment)?;
                    if let Some(target) = decision.target_column_id {
                        db.set_card_column(p.card_id, target)?;
                    }
                    let card = db.set_card_status(p.card_id, decision.new_status)?;
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
}

#[cfg(feature = "fake-client")]
pub use fake::FakeBoardClient;
