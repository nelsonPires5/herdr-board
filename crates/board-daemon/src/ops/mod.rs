//! Synchronous request handlers for every protocol method (except
//! `events.subscribe`, handled by the connection layer). DB work is quick and
//! serialized; spawning is deferred to the dispatcher via `wake_dispatch`.

use std::sync::Arc;

use board_core::protocol::*;
use board_core::{Error, Result};
use serde_json::{json, Value};

use crate::state::Daemon;
use crate::template;

mod boards;
mod cards;
mod columns;
mod comments;
mod discovery;
mod runs;
#[cfg(test)]
mod tests;

/// Route one request. Returns the `result` payload or a `board_core::Error`
/// (mapped to a protocol error code by the caller).
pub fn handle_request(d: &Arc<Daemon>, method: &str, params: Value) -> Result<Value> {
    match method {
        "daemon.status" => boards::daemon_status(d),
        "daemon.stop" => {
            d.trigger_shutdown();
            Ok(json!(StopResult { stopping: true }))
        }
        "board.open" => boards::board_open(d, from(params)?),
        "board.list" => boards::board_list(d),
        "board.get" => boards::board_get(
            d,
            if params.is_null() {
                BoardGetParams::default()
            } else {
                from(params)?
            },
        ),
        "column.create" => columns::column_create(d, from(params)?),
        "column.update" => columns::column_update(d, from(params)?),
        "column.reorder" => columns::column_reorder(d, from(params)?),
        "column.delete" => columns::column_delete(d, from(params)?),
        "template.apply" => template::apply(d, from(params)?),
        "card.create" => cards::card_create(d, from(params)?),
        "card.update" => cards::card_update(d, from(params)?),
        "card.delete" => cards::card_delete(d, from(params)?),
        "card.archive" => cards::card_archive(d, from(params)?),
        "card.move" => cards::card_move(d, from(params)?),
        "card.get" => cards::card_get(d, from(params)?),
        "card.list" => cards::card_list(d, from(params)?),
        "comment.add" => comments::comment_add(d, from(params)?),
        "run.done" => runs::run_done(d, from(params)?),
        "run.pane_exited" => runs::run_pane_exited(d, from(params)?),
        "run.cancel" => runs::run_cancel(d, from(params)?),
        "run.retry" => runs::run_retry(d, from(params)?),
        "run.focus" => runs::run_focus(d, from(params)?),
        "harness.capabilities" => discovery::harness_capabilities(d, from(params)?),
        "harness.list" => discovery::harness_list(d),
        "space.list" => discovery::space_list(d, from(params)?),
        "session.list" => discovery::session_list(d),
        other => Err(Error::BadRequest(format!("unknown method: {other}"))),
    }
}

fn from<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::BadRequest(format!("bad params: {e}")))
}

fn require_card(d: &Arc<Daemon>, id: i64) -> Result<board_core::model::Card> {
    d.store
        .lock()
        .get_card(id)?
        .ok_or_else(|| Error::NotFound(format!("card {id}")))
}

fn require_column(d: &Arc<Daemon>, id: i64) -> Result<board_core::model::Column> {
    d.store
        .lock()
        .get_column(id)?
        .ok_or_else(|| Error::NotFound(format!("column {id}")))
}
