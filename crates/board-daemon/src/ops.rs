//! Synchronous request handlers for every protocol method (except
//! `events.subscribe`, handled by the connection layer). DB work is quick and
//! serialized; spawning is deferred to the dispatcher via `wake_dispatch`.

use std::sync::Arc;

use board_core::capability::capabilities_for;
use board_core::db::BOARD_ID;
use board_core::engine::{
    decide_entry, validate_card_edit, validate_column_delete, validate_column_permission_override,
};
use board_core::protocol::*;
use board_core::{Error, Result};
use serde_json::{json, Value};

use crate::dispatch::{enqueue_run, finalize_run};
use crate::state::Daemon;
use crate::template;

/// Route one request. Returns the `result` payload or a `board_core::Error`
/// (mapped to a protocol error code by the caller).
pub fn handle_request(d: &Arc<Daemon>, method: &str, params: Value) -> Result<Value> {
    match method {
        "daemon.status" => daemon_status(d),
        "daemon.stop" => {
            d.trigger_shutdown();
            Ok(json!(StopResult { stopping: true }))
        }
        "board.get" => board_get(d),
        "column.create" => column_create(d, from(params)?),
        "column.update" => column_update(d, from(params)?),
        "column.reorder" => column_reorder(d, from(params)?),
        "column.delete" => column_delete(d, from(params)?),
        "template.apply" => template::apply(d, from(params)?),
        "card.create" => card_create(d, from(params)?),
        "card.update" => card_update(d, from(params)?),
        "card.delete" => card_delete(d, from(params)?),
        "card.move" => card_move(d, from(params)?),
        "card.get" => card_get(d, from(params)?),
        "card.list" => card_list(d, from(params)?),
        "comment.add" => comment_add(d, from(params)?),
        "run.done" => run_done(d, from(params)?),
        "run.cancel" => run_cancel(d, from(params)?),
        "run.retry" => run_retry(d, from(params)?),
        "harness.capabilities" => harness_capabilities(d, from(params)?),
        "space.list" => space_list(d),
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

// -- daemon / board ---------------------------------------------------------

fn daemon_status(d: &Arc<Daemon>) -> Result<Value> {
    let (active_runs, queued_runs) = {
        let db = d.store.lock();
        (db.count_active_runs()?, db.count_queued_runs()?)
    };
    let herdr_connected = match &d.herdr {
        Some(h) => {
            let mut c = h.clone();
            c.is_live()
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

fn board_get(d: &Arc<Daemon>) -> Result<Value> {
    let db = d.store.lock();
    Ok(json!(BoardSnapshot {
        board: db.get_board(BOARD_ID)?,
        columns: db.list_columns(BOARD_ID)?,
        cards: db.list_cards(BOARD_ID)?,
    }))
}

// -- columns ----------------------------------------------------------------

fn column_create(d: &Arc<Daemon>, p: ColumnCreateParams) -> Result<Value> {
    validate_column_permission_override(p.permission_override.as_deref())?;
    let col = d.store.lock().create_column(&p)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, Some(col.id));
    Ok(json!(col))
}

fn column_update(d: &Arc<Daemon>, p: ColumnUpdateParams) -> Result<Value> {
    validate_column_permission_override(p.permission_override.as_deref())?;
    let col = d.store.lock().update_column(&p)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, Some(col.id));
    Ok(json!(col))
}

fn column_reorder(d: &Arc<Daemon>, p: ColumnReorderParams) -> Result<Value> {
    let cols = d.store.lock().reorder_column(p.id, p.position)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, None);
    Ok(json!(cols))
}

fn column_delete(d: &Arc<Daemon>, p: ColumnDeleteParams) -> Result<Value> {
    {
        let db = d.store.lock();
        let cards = db.list_cards_in_column(p.id)?;
        let has_active = cards
            .iter()
            .any(|c| matches!(c.status, CardStatus::Running | CardStatus::Queued));
        validate_column_delete(!cards.is_empty(), has_active, p.move_cards_to)?;
        db.delete_column(p.id, p.move_cards_to)?;
    }
    d.emit_changed(BoardChangedReason::ColumnChanged, None, None);
    Ok(json!(DeletedResult { deleted: true }))
}

// -- cards ------------------------------------------------------------------

fn card_create(d: &Arc<Daemon>, p: CardCreateParams) -> Result<Value> {
    let card = d.store.lock().create_card(&p)?;
    let column = require_column(d, card.column_id)?;
    d.emit_changed(
        BoardChangedReason::CardCreated,
        Some(card.id),
        Some(card.column_id),
    );

    // Creating directly into an auto column dispatches immediately.
    if column.trigger == Trigger::Auto {
        let entry = decide_entry(&column, card.status, false);
        if entry.enqueue {
            d.sched.lock().unwrap().chain_hops.remove(&card.id);
            enqueue_run(d, card.id, card.column_id, false)?;
            d.wake_dispatch();
        }
    }
    Ok(json!(require_card(d, card.id)?))
}

fn card_update(d: &Arc<Daemon>, p: CardUpdateParams) -> Result<Value> {
    let card = require_card(d, p.id)?;
    let edits_locked = p.harness.is_some()
        || p.model.is_some()
        || p.effort.is_some()
        || p.permission_mode.is_some()
        || p.space_kind.is_some()
        || p.space_ref.is_some()
        || p.worktree_base.is_some();
    validate_card_edit(card.status, edits_locked)?;
    let card = d.store.lock().update_card(&p)?;
    d.emit_changed(BoardChangedReason::CardUpdated, Some(card.id), None);
    Ok(json!(card))
}

fn card_delete(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    require_card(d, p.id)?;
    if d.store.lock().active_run_for_card(p.id)?.is_some() {
        return Err(Error::InvalidState(
            "card is running; cancel it first".into(),
        ));
    }
    d.store.lock().delete_card(p.id)?;
    d.emit_changed(BoardChangedReason::CardDeleted, Some(p.id), None);
    Ok(json!(DeletedResult { deleted: true }))
}

fn card_move(d: &Arc<Daemon>, p: CardMoveParams) -> Result<Value> {
    require_card(d, p.id)?;
    let target = require_column(d, p.column_id)?;
    let card = d.store.lock().move_card(p.id, p.column_id, p.position)?;
    d.emit_changed(
        BoardChangedReason::CardMoved,
        Some(card.id),
        Some(p.column_id),
    );

    let entry = decide_entry(&target, card.status, false);
    if entry.enqueue {
        // Human move resets the auto-chain counter.
        d.sched.lock().unwrap().chain_hops.remove(&card.id);
        enqueue_run(d, card.id, p.column_id, false)?;
        d.wake_dispatch();
    }
    Ok(json!(require_card(d, card.id)?))
}

fn card_get(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    let db = d.store.lock();
    let card = db
        .get_card(p.id)?
        .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
    Ok(json!(CardDetail {
        comments: db.list_comments(p.id)?,
        runs: db.list_runs(p.id)?,
        card,
    }))
}

fn card_list(d: &Arc<Daemon>, p: CardListParams) -> Result<Value> {
    let db = d.store.lock();
    let cards = match p.column_id {
        Some(c) => db.list_cards_in_column(c)?,
        None => db.list_cards(BOARD_ID)?,
    };
    Ok(json!(cards))
}

// -- comments / runs --------------------------------------------------------

fn comment_add(d: &Arc<Daemon>, p: CommentAddParams) -> Result<Value> {
    let author = p.author.as_deref().unwrap_or("user");
    let comment = d.store.lock().add_comment(p.card_id, author, &p.body)?;
    d.emit_changed(BoardChangedReason::CommentAdded, Some(p.card_id), None);
    Ok(json!(comment))
}

fn run_done(d: &Arc<Daemon>, p: RunDoneParams) -> Result<Value> {
    let run = d
        .store
        .lock()
        .active_run_for_card(p.card_id)?
        .ok_or_else(|| Error::NotFound(format!("no active run for card {}", p.card_id)))?;
    let (run, card) = finalize_run(d, run.id, p.outcome, p.summary, None, false, true)?;
    Ok(json!(RunActionResult { run, card }))
}

fn run_cancel(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    // Prefer the active run; else cancel the latest queued run for the card.
    let active = d.store.lock().active_run_for_card(p.card_id)?;
    if let Some(run) = active {
        let (run, card) = finalize_run(
            d,
            run.id,
            RunOutcome::Cancelled,
            Some("cancelled by user".into()),
            None,
            true,
            false,
        )?;
        return Ok(json!(RunActionResult { run, card }));
    }

    // Queued (never started) run.
    let queued = d
        .store
        .queued_runs()?
        .into_iter()
        .filter(|(_, c)| c.id == p.card_id)
        .map(|(r, _)| r)
        .next_back()
        .ok_or_else(|| {
            Error::NotFound(format!("no active or queued run for card {}", p.card_id))
        })?;
    let (run, card) = {
        let db = d.store.lock();
        let run = db.finish_run(queued.id, RunOutcome::Cancelled, Some("cancelled by user"))?;
        db.add_comment(p.card_id, "system", "queued run cancelled by user")?;
        let card = db.set_card_status(p.card_id, CardStatus::Failed)?;
        (run, card)
    };
    d.sched.lock().unwrap().chain_hops.remove(&p.card_id);
    d.emit_run_ended(p.card_id, run.id, RunOutcome::Cancelled);
    d.wake_dispatch();
    Ok(json!(RunActionResult { run, card }))
}

fn run_retry(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    let card = require_card(d, p.card_id)?;
    if matches!(card.status, CardStatus::Running | CardStatus::Queued) {
        return Err(Error::InvalidState(
            "card is running or queued; cannot retry".into(),
        ));
    }
    // Human action: reset the auto-chain counter and fork the session.
    d.sched.lock().unwrap().chain_hops.remove(&p.card_id);
    let run = enqueue_run(d, p.card_id, card.column_id, true)?;
    d.wake_dispatch();
    d.emit_changed(BoardChangedReason::CardUpdated, Some(p.card_id), None);
    let card = require_card(d, p.card_id)?;
    Ok(json!(RunActionResult { run, card }))
}

// -- harness / space --------------------------------------------------------

fn harness_capabilities(d: &Arc<Daemon>, p: HarnessCapabilitiesParams) -> Result<Value> {
    match capabilities_for(&p.harness, &d.config) {
        Some(caps) => Ok(json!(caps)),
        None => {
            // Known harnesses: the builtin `claude` plus config-defined ones.
            let mut known = vec!["claude".to_string()];
            known.extend(d.config.harness.keys().cloned());
            known.sort();
            Err(Error::NotFound(format!(
                "unknown harness '{}'; known: {}",
                p.harness,
                known.join(", ")
            )))
        }
    }
}

fn space_list(d: &Arc<Daemon>) -> Result<Value> {
    let herdr = d
        .herdr
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    let mut client = herdr.clone();
    let workspaces = client
        .workspace_list()
        .map_err(|e| Error::HerdrUnavailable(format!("workspace.list: {e}")))?;
    let spaces = workspaces
        .into_iter()
        .map(|w| SpaceInfo {
            id: w.workspace_id,
            label: w.label,
        })
        .collect();
    Ok(json!(SpaceListResult { spaces }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::DaemonSettings;
    use crate::spawner::LocalSpawner;
    use crate::store::Store;
    use board_core::config::{Config, HarnessDef};
    use board_core::db::Db;
    use std::path::PathBuf;
    use tokio::sync::{broadcast, mpsc, watch};

    fn test_daemon(config: Config) -> Arc<Daemon> {
        let db = Db::open_in_memory().unwrap();
        let store = Store::new(db);
        let (events_tx, _events_rx) = broadcast::channel(16);
        let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        Arc::new(Daemon::new(
            store,
            config,
            DaemonSettings::default(),
            PathBuf::from("/tmp/board-test.db"),
            PathBuf::from("/tmp/board-test.sock"),
            Arc::new(LocalSpawner::new()),
            None, // no herdr
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ))
    }

    #[test]
    fn harness_capabilities_claude_ok() {
        let d = test_daemon(Config::default());
        let v = handle_request(&d, "harness.capabilities", json!({ "harness": "claude" })).unwrap();
        assert_eq!(v["harness"], "claude");
        assert_eq!(v["model_freeform"], true);
        assert!(v["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "sonnet"));
    }

    #[test]
    fn harness_capabilities_config_defined_ok() {
        let mut config = Config::default();
        config.harness.insert(
            "fake".to_string(),
            HarnessDef {
                argv: vec!["bash".into(), "fake.sh".into()],
                models: vec!["m1".into()],
                efforts: vec!["low".into()],
                permission_modes: vec!["auto".into()],
            },
        );
        let d = test_daemon(config);
        let v = handle_request(&d, "harness.capabilities", json!({ "harness": "fake" })).unwrap();
        assert_eq!(v["harness"], "fake");
        assert_eq!(v["permission_modes"][0], "auto");
    }

    #[test]
    fn harness_capabilities_unknown_is_not_found() {
        let d = test_daemon(Config::default());
        let err =
            handle_request(&d, "harness.capabilities", json!({ "harness": "ghost" })).unwrap_err();
        assert_eq!(err.code(), 2);
        let msg = err.to_string();
        assert!(msg.contains("ghost"), "message: {msg}");
        assert!(
            msg.contains("claude"),
            "message lists known harnesses: {msg}"
        );
    }

    #[test]
    fn space_list_without_herdr_is_herdr_unavailable() {
        let d = test_daemon(Config::default());
        let err = handle_request(&d, "space.list", json!({})).unwrap_err();
        assert_eq!(err.code(), 4);
    }
}
