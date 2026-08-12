//! Synchronous request handlers for every protocol method (except
//! `events.subscribe`, handled by the connection layer). DB work is quick and
//! serialized; spawning is deferred to the dispatcher via `wake_dispatch`.

use std::sync::Arc;

use board_core::protocol::*;
use board_core::{Error, Result};
use serde_json::{json, Value};

use crate::state::Daemon;

mod boards;
mod cards;
mod columns;
mod comments;
mod discovery;
mod errors;
mod panes;
mod runs;
#[cfg(test)]
mod tests;

/// Declare the routing table exactly once.
///
/// The macro emits both [`handle_request`]'s `match` and [`ROUTED_METHODS`], so
/// the exported list cannot drift from what is actually routed. The handler
/// arguments (`d`, `params`) are named at the invocation below so the arms keep
/// ordinary call-site scoping.
macro_rules! routes {
    ($d:ident, $params:ident, { $($method:literal => $handler:expr),* $(,)? }) => {
        /// Every method name [`handle_request`] routes, in dispatch order.
        ///
        /// `board-core`'s [`FakeBoardClient`](board_core::client::FakeBoardClient)
        /// re-implements part of this surface for the board-tui test tier; the
        /// parity guard in `ops::tests::parity` pins the difference.
        pub const ROUTED_METHODS: &[&str] = &[$($method),*];

        /// Route one request. Returns the `result` payload or a
        /// `board_core::Error` (mapped to a protocol error code by the caller).
        pub fn handle_request($d: &Arc<Daemon>, method: &str, $params: Value) -> Result<Value> {
            match method {
                $($method => $handler,)*
                other => Err(Error::BadRequest(format!("unknown method: {other}"))),
            }
        }
    };
}

routes!(d, params, {
    "daemon.status" => boards::daemon_status(d),
    "daemon.stop" => {
        d.trigger_shutdown();
        Ok(json!(StopResult { stopping: true }))
    },
    "board.open" => boards::board_open(d, from(params)?),
    "board.rename" => boards::board_rename(d, from(params)?),
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
    "template.apply" => boards::template_apply(d, from(params)?),
    "card.create" => cards::card_create(d, from(params)?),
    "card.duplicate" => cards::card_duplicate(d, from(params)?),
    "card.update" => cards::card_update(d, from(params)?),
    "card.delete" => cards::card_delete(d, from(params)?),
    "card.archive" => cards::card_archive(d, from(params)?),
    "card.move" => cards::card_move(d, from(params)?),
    "card.get" => cards::card_get(d, from(params)?),
    "card.list" => cards::card_list(d, from(params)?),
    "comment.add" => comments::comment_add(d, from(params)?),
    "comment.get" => comments::comment_get(d, from(params)?),
    "comment.update" => comments::comment_update(d, from(params)?),
    "comment.delete" => comments::comment_delete(d, from(params)?),
    "comment.history" => comments::comment_history(d, from(params)?),
    "run.done" => runs::run_done(d, from(params)?),
    "run.pane_exited" => runs::run_pane_exited(d, from(params)?),
    "run.cancel" => runs::run_cancel(d, from(params)?),
    "run.retry" => runs::run_retry(d, from(params)?),
    "run.focus" => runs::run_focus(d, from(params)?),
    "harness.capabilities" => discovery::harness_capabilities(d, from(params)?),
    "harness.list" => discovery::harness_list(d),
    "space.list" => discovery::space_list(d, from(params)?),
    "session.list" => discovery::session_list(d),
    "pane.set_title" => panes::pane_set_title(from(params)?),
});

fn from<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::BadRequest(format!("bad params: {e}")))
}

fn require_card(d: &Arc<Daemon>, id: i64) -> Result<board_core::model::Card> {
    d.store.lock().require_card(id)
}
