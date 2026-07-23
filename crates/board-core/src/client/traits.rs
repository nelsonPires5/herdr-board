use serde_json::{json, Value};

use crate::capability::HarnessCapabilities;

use crate::model::{Card, Column, Comment};

use crate::protocol::{
    BoardGetParams, BoardListResult, BoardOpenParams, BoardSnapshot, CardArchiveParams,
    CardCreateParams, CardDetail, CardListParams, CardMoveParams, CardUpdateParams,
    ColumnCreateParams, ColumnDeleteParams, ColumnReorderParams, ColumnUpdateParams,
    CommentAddParams, DaemonStatus, DeletedResult, Event, HarnessCapabilitiesParams,
    HarnessListResult, RunActionResult, RunCardParams, RunDoneParams, RunFocusParams,
    RunFocusResult, RunOutcome, RunPaneExitedParams, SessionListResult, SpaceListParams,
    SpaceListResult, StopResult, TemplateApplyParams,
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

    fn harness_capabilities(&mut self, harness: &str) -> anyhow::Result<HarnessCapabilities> {
        let p = HarnessCapabilitiesParams {
            harness: harness.to_string(),
        };
        Ok(serde_json::from_value(
            self.call("harness.capabilities", serde_json::to_value(p)?)?,
        )?)
    }
    fn harness_list(&mut self) -> anyhow::Result<HarnessListResult> {
        Ok(serde_json::from_value(
            self.call("harness.list", json!({}))?,
        )?)
    }
    fn space_list(&mut self, session: Option<&str>) -> anyhow::Result<SpaceListResult> {
        let p = SpaceListParams {
            session: session.map(str::to_string),
        };
        Ok(serde_json::from_value(
            self.call("space.list", serde_json::to_value(p)?)?,
        )?)
    }
    fn session_list(&mut self) -> anyhow::Result<SessionListResult> {
        Ok(serde_json::from_value(
            self.call("session.list", json!({}))?,
        )?)
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
        self.run_done_for_run(card_id, outcome, summary, None)
    }
    fn run_done_for_run(
        &mut self,
        card_id: i64,
        outcome: RunOutcome,
        summary: Option<&str>,
        run_id: Option<i64>,
    ) -> anyhow::Result<RunActionResult> {
        let p = RunDoneParams {
            card_id,
            outcome,
            summary: summary.map(str::to_string),
            run_id,
        };
        Ok(serde_json::from_value(
            self.call("run.done", serde_json::to_value(p)?)?,
        )?)
    }
    fn run_pane_exited(&mut self, card_id: i64, run_id: i64) -> anyhow::Result<RunActionResult> {
        let p = RunPaneExitedParams { card_id, run_id };
        Ok(serde_json::from_value(
            self.call("run.pane_exited", serde_json::to_value(p)?)?,
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
