//! Synchronous request handlers for every protocol method (except
//! `events.subscribe`, handled by the connection layer). DB work is quick and
//! serialized; spawning is deferred to the dispatcher via `wake_dispatch`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use board_core::capability::{available_harnesses, capabilities_for};
use board_core::db::BOARD_ID;
use board_core::engine::{
    decide_entry, merge_card_update, merge_column_update, validate_card_archive,
    validate_card_edit, validate_card_settings, validate_card_values, validate_column_delete,
    validate_column_update, validate_column_values, PermissionContext,
};
use board_core::harness::{is_builtin_harness, DEFAULT_HARNESS};
use board_core::pi_catalog;
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
        "board.open" => board_open(d, from(params)?),
        "board.list" => board_list(d),
        "board.get" => board_get(
            d,
            if params.is_null() {
                BoardGetParams::default()
            } else {
                from(params)?
            },
        ),
        "column.create" => column_create(d, from(params)?),
        "column.update" => column_update(d, from(params)?),
        "column.reorder" => column_reorder(d, from(params)?),
        "column.delete" => column_delete(d, from(params)?),
        "template.apply" => template::apply(d, from(params)?),
        "card.create" => card_create(d, from(params)?),
        "card.update" => card_update(d, from(params)?),
        "card.delete" => card_delete(d, from(params)?),
        "card.archive" => card_archive(d, from(params)?),
        "card.move" => card_move(d, from(params)?),
        "card.get" => card_get(d, from(params)?),
        "card.list" => card_list(d, from(params)?),
        "comment.add" => comment_add(d, from(params)?),
        "run.done" => run_done(d, from(params)?),
        "run.pane_exited" => run_pane_exited(d, from(params)?),
        "run.cancel" => run_cancel(d, from(params)?),
        "run.retry" => run_retry(d, from(params)?),
        "run.focus" => run_focus(d, from(params)?),
        "harness.capabilities" => harness_capabilities(d, from(params)?),
        "harness.list" => harness_list(d),
        "space.list" => space_list(d, from(params)?),
        "session.list" => session_list(d),
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

fn board_snapshot(d: &Arc<Daemon>, board_id: i64) -> Result<Value> {
    let db = d.store.lock();
    Ok(json!(BoardSnapshot {
        board: db.get_board(board_id)?,
        columns: db.list_columns(board_id)?,
        cards: db.list_cards(board_id)?,
    }))
}

fn board_open(d: &Arc<Daemon>, p: BoardOpenParams) -> Result<Value> {
    let board = d.store.lock().open_board(&p.scope_path)?;
    board_snapshot(d, board.id)
}

fn board_list(d: &Arc<Daemon>) -> Result<Value> {
    Ok(json!(BoardListResult {
        boards: d.store.lock().list_boards()?,
    }))
}

fn board_get(d: &Arc<Daemon>, p: BoardGetParams) -> Result<Value> {
    board_snapshot(d, p.board_id.unwrap_or(BOARD_ID))
}

// -- columns ----------------------------------------------------------------

fn column_create(d: &Arc<Daemon>, p: ColumnCreateParams) -> Result<Value> {
    validate_column_values(
        p.harness_override.as_deref(),
        p.model_override.as_deref(),
        p.effort_override.as_deref(),
        p.permission_override.as_deref(),
        &d.config,
        PermissionContext::ColumnOverride,
    )?;
    let col = d.store.lock().create_column(&p)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, Some(col.id));
    Ok(json!(col))
}

fn column_update(d: &Arc<Daemon>, p: ColumnUpdateParams) -> Result<Value> {
    let col = {
        let sched = d.sched.lock().unwrap();
        sched.ensure_no_finalizing_cards("update a column")?;
        let db = d.store.lock();
        let current = db
            .get_column(p.id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", p.id)))?;
        let merged = merge_column_update(&current, &p);
        validate_column_update(&current, &merged, &p, &d.config)?;
        db.update_column(&p)?
    };
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
        // A pending transition may still read its source or move into its
        // target, so column deletion pauses briefly for any finalizing card.
        let sched = d.sched.lock().unwrap();
        sched.ensure_no_finalizing_cards("delete a column")?;
        let db = d.store.lock();
        let cards = db.list_cards_in_column(p.id)?;
        let has_open_run = db.column_has_open_run(p.id)?;
        validate_column_delete(!cards.is_empty(), has_open_run, p.move_cards_to)?;
        db.delete_column(p.id, p.move_cards_to)?;
    }
    d.emit_changed(BoardChangedReason::ColumnChanged, None, None);
    Ok(json!(DeletedResult { deleted: true }))
}

// -- cards ------------------------------------------------------------------

fn card_create(d: &Arc<Daemon>, p: CardCreateParams) -> Result<Value> {
    validate_card_values(
        p.harness.as_deref().unwrap_or(DEFAULT_HARNESS),
        p.model.as_deref(),
        p.effort,
        p.permission_mode.as_deref(),
        p.space_kind.unwrap_or(SpaceKind::Workspace),
        p.space_ref.as_deref(),
        p.space_cwd.as_deref(),
        &d.config,
    )?;
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
    let edits_locked = p.harness.is_some()
        || !p.model.is_unchanged()
        || !p.effort.is_unchanged()
        || !p.permission_mode.is_unchanged()
        || !p.session.is_unchanged()
        || p.space_kind.is_some()
        || !p.space_ref.is_unchanged()
        || !p.space_cwd.is_unchanged();
    let card = {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        // Status validation remains a defense against inconsistent card state;
        // the open-run/finalization checks are authoritative and serialized
        // with locked-field updates.
        if edits_locked {
            sched.ensure_card_not_finalizing(p.id)?;
        }
        validate_card_edit(card.status, edits_locked)?;
        if edits_locked && db.open_run_for_card(p.id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; cannot edit harness/space fields".into(),
            ));
        }
        let merged = merge_card_update(&card, &p);
        validate_card_settings(&merged, &d.config)?;
        db.update_card(&p)?
    };
    d.emit_changed(BoardChangedReason::CardUpdated, Some(card.id), None);
    Ok(json!(card))
}

fn card_delete(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        sched.ensure_card_not_finalizing(p.id)?;
        if db.open_run_for_card(p.id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; cancel it first".into(),
            ));
        }
        db.delete_card(p.id)?;
    }
    d.emit_changed(BoardChangedReason::CardDeleted, Some(p.id), None);
    Ok(json!(DeletedResult { deleted: true }))
}

fn card_archive(d: &Arc<Daemon>, p: CardArchiveParams) -> Result<Value> {
    let card = {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        sched.ensure_card_not_finalizing(p.id)?;
        if p.archived {
            validate_card_archive(card.status)?;
            if db.open_run_for_card(p.id)?.is_some() {
                return Err(Error::InvalidState(
                    "card has an open run; cancel it before archiving".into(),
                ));
            }
        }
        db.set_card_archived(p.id, p.archived)?
    };
    d.emit_changed(BoardChangedReason::CardArchived, Some(p.id), None);
    Ok(json!(card))
}

fn card_move(d: &Arc<Daemon>, p: CardMoveParams) -> Result<Value> {
    let (card, target) = {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let current = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        sched.ensure_card_not_finalizing(p.id)?;
        if current.archived_at.is_some() {
            return Err(Error::InvalidState(
                "archived card must be restored before moving".into(),
            ));
        }
        let target = db
            .get_column(p.column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", p.column_id)))?;
        let card = db.move_card(p.id, p.column_id, p.position)?;
        (card, target)
    };
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
    let board_id = p.board_id.unwrap_or(BOARD_ID);
    let cards = match p.column_id {
        Some(c) => {
            let column = db
                .get_column(c)?
                .ok_or_else(|| Error::NotFound(format!("column {c}")))?;
            if column.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "column {c} belongs to board {}, expected {board_id}",
                    column.board_id
                )));
            }
            db.list_cards_in_column(c)?
        }
        None => db.list_cards(board_id)?,
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
    let run = {
        // Keep the scheduler -> store lock order and finalization guard used
        // by the normal active-run path. A configured runner can call board
        // done before pane registration, while its run is still queued; only
        // that exact, sole queued run is eligible for this narrow exception.
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        sched.ensure_card_not_finalizing(p.card_id)?;
        if let Some(run) = db.active_run_for_card(p.card_id)? {
            if let Some(run_id) = p.run_id {
                if run.id != run_id {
                    return Err(Error::InvalidState(format!(
                        "run {run_id} does not match active run {} for card {}",
                        run.id, p.card_id
                    )));
                }
            }
            run
        } else {
            let card = db
                .get_card(p.card_id)?
                .ok_or_else(|| Error::NotFound(format!("no active run for card {}", p.card_id)))?;
            match db.open_run_for_card(p.card_id)? {
                Some(run)
                    if card.status == CardStatus::Queued
                        && run.started_at.is_none()
                        && !is_builtin_harness(&run.harness)
                        && p.run_id == Some(run.id) =>
                {
                    run
                }
                _ => {
                    return Err(Error::NotFound(format!(
                        "no active run for card {}",
                        p.card_id
                    )))
                }
            }
        }
    };
    let (run, card) = finalize_run(d, run.id, p.outcome, p.summary, None, false, true)?;
    Ok(json!(RunActionResult { run, card }))
}

fn run_pane_exited(d: &Arc<Daemon>, p: RunPaneExitedParams) -> Result<Value> {
    {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        sched.ensure_card_not_finalizing(p.card_id)?;
        let open = db
            .open_run_for_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("no open run for card {}", p.card_id)))?;
        if open.id != p.run_id {
            return Err(Error::InvalidState(format!(
                "open run {} for card {} does not match pane-exited run {}",
                open.id, p.card_id, p.run_id
            )));
        }
        if is_builtin_harness(&open.harness) {
            return Err(Error::InvalidState(
                "pane-exited callback is only valid for configured harnesses".into(),
            ));
        }
    }

    let (run, card) = finalize_run(
        d,
        p.run_id,
        RunOutcome::Fail,
        Some("configured harness exited without calling board done".into()),
        Some("pane exited without board done".into()),
        false,
        false,
    )?;
    Ok(json!(RunActionResult { run, card }))
}

fn run_cancel(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    // Prefer the active run; else cancel the latest queued run for the card.
    let active = {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        sched.ensure_card_not_finalizing(p.card_id)?;
        db.active_run_for_card(p.card_id)?
    };
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
    let (run, card) = {
        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        sched.ensure_card_not_finalizing(p.card_id)?;
        let queued = db
            .open_run_for_card(p.card_id)?
            .filter(|run| run.started_at.is_none())
            .ok_or_else(|| {
                Error::NotFound(format!("no active or queued run for card {}", p.card_id))
            })?;
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

fn run_focus(d: &Arc<Daemon>, p: RunFocusParams) -> Result<Value> {
    let run = d
        .store
        .lock()
        .latest_run_with_pane(p.card_id)?
        .ok_or_else(|| {
            Error::NotFound(format!(
                "no run with an accessible pane for card {}",
                p.card_id
            ))
        })?;
    let pane_id = run
        .herdr_pane_id
        .clone()
        .ok_or_else(|| Error::NotFound(format!("run {} has no pane", run.id)))?;
    let registry = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("jump to pane requires Herdr".into()))?;
    let target_socket = match run.session.as_deref() {
        None => registry.default_socket().to_path_buf(),
        Some(session) => {
            registry
                .resolve(Some(session))
                .map_err(|e| Error::HerdrUnavailable(format!("resolving run session: {e:#}")))?
                .socket
        }
    };
    let origin_socket = normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let target_socket = normalize_socket(&target_socket, "target")?;
    if origin_socket != target_socket {
        return Err(Error::InvalidState(
            "run pane belongs to a different Herdr session; cross-session jump is not supported"
                .into(),
        ));
    }

    let mut client = board_herdr::HerdrClient::connect(&target_socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
    client
        .pane_focus(&pane_id)
        .map_err(|e| Error::HerdrUnavailable(format!("pane.focus {pane_id}: {e}")))?;
    Ok(json!(RunFocusResult {
        run_id: run.id,
        pane_id,
    }))
}

fn normalize_socket(path: &Path, kind: &str) -> Result<PathBuf> {
    path.canonicalize().map_err(|e| {
        Error::HerdrUnavailable(format!(
            "{kind} Herdr socket '{}' is unavailable: {e}",
            path.display()
        ))
    })
}

fn run_retry(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    let card = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        sched.ensure_card_not_finalizing(p.card_id)?;
        if card.archived_at.is_some() {
            return Err(Error::InvalidState(
                "archived card must be restored before retrying".into(),
            ));
        }
        if db.open_run_for_card(p.card_id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; complete or cancel it before retrying".into(),
            ));
        }
        // Human action: reset the auto-chain counter and fork the session.
        sched.chain_hops.remove(&p.card_id);
        card
    };
    let run = enqueue_run(d, p.card_id, card.column_id, true)?;
    d.wake_dispatch();
    d.emit_changed(BoardChangedReason::CardUpdated, Some(p.card_id), None);
    let card = require_card(d, p.card_id)?;
    Ok(json!(RunActionResult { run, card }))
}

// -- harness / space --------------------------------------------------------

fn harness_capabilities(d: &Arc<Daemon>, p: HarnessCapabilitiesParams) -> Result<Value> {
    match capabilities_for(&p.harness, &d.config) {
        Some(mut caps) => {
            // Pi's static catalog is free-form (models: []); overlay the live
            // catalog read from the pi agent dir when one is configured. Tests
            // leave `pi_agent_dir` unset, so this stays the static catalog.
            if p.harness == "pi" {
                let models = pi_catalog::live_models(d.config.pi_agent_dir.as_deref(), "pi");
                if !models.is_empty() {
                    caps.models = models;
                }
            }
            Ok(json!(caps))
        }
        None => {
            let known = available_harnesses(&d.config);
            Err(Error::NotFound(format!(
                "unknown harness '{}'; known: {}",
                p.harness,
                known.join(", ")
            )))
        }
    }
}

fn harness_list(d: &Arc<Daemon>) -> Result<Value> {
    Ok(json!(HarnessListResult {
        harnesses: available_harnesses(&d.config)
    }))
}

fn space_list(d: &Arc<Daemon>, p: SpaceListParams) -> Result<Value> {
    let reg = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    // Resolve the requested session (None = default) to its socket; an
    // unknown/stopped session errors listing the known ones.
    let resolved = reg
        .resolve(p.session.as_deref())
        .map_err(|e| Error::HerdrUnavailable(format!("session '{:?}': {e:#}", p.session)))?;
    let mut client = board_herdr::HerdrClient::connect(&resolved.socket)
        .map_err(|e| Error::HerdrUnavailable(format!("herdr unavailable: {e}")))?;
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

fn session_list(d: &Arc<Daemon>) -> Result<Value> {
    let reg = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    let sessions = reg
        .session_infos()
        .map_err(|e| Error::HerdrUnavailable(format!("session.list: {e:#}")))?;
    Ok(json!(SessionListResult { sessions }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionRegistry;
    use crate::settings::DaemonSettings;
    use crate::spawner::LocalSpawner;
    use crate::store::Store;
    use board_core::config::{Config, HarnessDef};
    use board_core::db::Db;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;
    use tokio::sync::{broadcast, mpsc, watch};

    fn test_daemon(config: Config) -> Arc<Daemon> {
        test_daemon_with_registry(config, None)
    }

    fn test_daemon_with_registry(
        config: Config,
        session_registry: Option<SessionRegistry>,
    ) -> Arc<Daemon> {
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
            session_registry,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ))
    }

    #[test]
    fn merged_invalid_updates_are_atomic_and_emit_no_event() {
        let d = test_daemon(Config::default());
        let mut events = d.events_tx.subscribe();
        let created = handle_request(
            &d,
            "card.create",
            json!({
                "title": "valid settings",
                "harness": "claude",
                "model": "sonnet",
                "effort": "high",
                "permission_mode": "manual",
                "space_kind": "new_workspace",
                "space_ref": "feature",
                "space_cwd": "/repo"
            }),
        )
        .unwrap();
        let card_id = created["id"].as_i64().unwrap();
        let _ = events.try_recv().expect("create event");

        let err = handle_request(
            &d,
            "card.update",
            json!({
                "id": card_id,
                "space_kind": "new_workspace",
                "space_cwd": null
            }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let unchanged = d.store.lock().get_card(card_id).unwrap().unwrap();
        assert_eq!(unchanged.space_ref.as_deref(), Some("feature"));
        assert_eq!(unchanged.space_cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn invalid_column_update_keeps_dependents_and_emits_no_event() {
        let d = test_daemon(Config::default());
        let mut events = d.events_tx.subscribe();
        let created = handle_request(
            &d,
            "column.create",
            json!({
                "name": "validated",
                "harness_override": "claude",
                "model_override": "sonnet",
                "effort_override": "high",
                "permission_override": "manual"
            }),
        )
        .unwrap();
        let id = created["id"].as_i64().unwrap();
        let _ = events.try_recv().expect("create event");
        let err = handle_request(
            &d,
            "column.update",
            json!({"id": id, "harness_override": null}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let unchanged = d.store.lock().get_column(id).unwrap().unwrap();
        assert_eq!(unchanged.harness_override.as_deref(), Some("claude"));
        assert_eq!(unchanged.effort_override.as_deref(), Some("high"));
    }

    #[test]
    fn daemon_stop_triggers_shutdown_and_reports_stopping() {
        let d = test_daemon(Config::default());
        assert!(!d.is_shutdown());
        let res = handle_request(&d, "daemon.stop", json!({})).unwrap();
        assert_eq!(res["stopping"], true);
        assert!(d.is_shutdown());
    }

    #[test]
    fn board_open_list_get_and_legacy_default_are_scoped() {
        let d = test_daemon(Config::default());
        let alpha = handle_request(&d, "board.open", json!({"scope_path":"/alpha"})).unwrap();
        let beta = handle_request(&d, "board.open", json!({"scope_path":"/beta"})).unwrap();
        let alpha_id = alpha["board"]["id"].as_i64().unwrap();
        let beta_id = beta["board"]["id"].as_i64().unwrap();
        assert_ne!(alpha_id, beta_id);
        assert_eq!(alpha["columns"].as_array().unwrap().len(), 1);

        handle_request(
            &d,
            "card.create",
            json!({"board_id":alpha_id,"title":"alpha"}),
        )
        .unwrap();
        assert_eq!(
            handle_request(&d, "board.get", json!({"board_id":alpha_id})).unwrap()["cards"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            handle_request(&d, "board.get", json!({"board_id":beta_id})).unwrap()["cards"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let legacy = handle_request(&d, "board.get", json!({})).unwrap();
        assert_eq!(legacy["board"]["name"], "Global");
        let omitted = handle_request(&d, "board.get", Value::Null).unwrap();
        assert_eq!(omitted["board"]["name"], "Global");
        let list = handle_request(&d, "board.list", json!({})).unwrap();
        assert_eq!(list["boards"][0]["name"], "Global");
    }

    #[test]
    fn template_and_scheduler_operate_on_scoped_board() {
        let d = test_daemon(Config::default());
        let opened = handle_request(&d, "board.open", json!({"scope_path":"/scoped"})).unwrap();
        let board_id = opened["board"]["id"].as_i64().unwrap();
        handle_request(
            &d,
            "template.apply",
            json!({"name":"pipeline","board_id":board_id}),
        )
        .unwrap();
        let snapshot = handle_request(&d, "board.get", json!({"board_id":board_id})).unwrap();
        assert_eq!(snapshot["columns"].as_array().unwrap().len(), 6);
        let execute = snapshot["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|column| column["name"] == "Execute")
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let card = handle_request(
            &d,
            "card.create",
            json!({"board_id":board_id,"column_id":execute,"title":"queued","harness":"pi"}),
        )
        .unwrap();
        assert_eq!(card["board_id"], board_id);
        assert!(d
            .store
            .queued_runs()
            .unwrap()
            .iter()
            .any(|(_, queued)| queued.id == card["id"].as_i64().unwrap()));
    }

    #[test]
    fn run_done_ok_without_target_column_marks_card_done() {
        let d = test_daemon(Config::default());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "confirm me".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                .unwrap();
            db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
            // Simulate the pre-confirmation state: awaiting human review.
            db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                .unwrap();
            (card.id, run.id)
        };

        let res =
            handle_request(&d, "run.done", json!({"card_id": card_id, "outcome": "ok"})).unwrap();

        // The seed Todo column has no on_success target: confirmed completion
        // lands on `done` (not `idle`), clearing the awaiting reason.
        assert_eq!(res["run"]["id"], run_id);
        assert_eq!(res["run"]["outcome"], "ok");
        assert_eq!(res["card"]["status"], "done");
        assert!(res["card"]["awaiting_reason"].is_null());
    }

    #[test]
    fn run_done_accepts_matching_queued_configured_run_before_pane_registration() {
        let mut config = Config::default();
        config.harness.insert(
            "custom".into(),
            HarnessDef {
                argv: vec!["custom-agent".into()],
                ..Default::default()
            },
        );
        let d = test_daemon(config);
        let (card_id, run_id, target_id) = {
            let db = d.store.lock();
            let target = db
                .create_column(&ColumnCreateParams {
                    name: "pre-registration target".into(),
                    ..Default::default()
                })
                .unwrap();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "pre-registration source".into(),
                    trigger: Some(Trigger::Auto),
                    on_success_column_id: Some(target.id),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "done before pane registration".into(),
                    column_id: Some(source.id),
                    harness: Some("custom".into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "custom", "[]", "p", None, None)
                .unwrap();
            // The configured runner can report board done before the daemon
            // registers the spawned pane, so this is an open queued run.
            db.set_card_status(card.id, CardStatus::Queued).unwrap();
            (card.id, run.id, target.id)
        };

        let result = handle_request(
            &d,
            "run.done",
            json!({
                "card_id": card_id,
                "run_id": run_id,
                "outcome": "ok",
                "summary": "completed before pane registration"
            }),
        )
        .unwrap();

        assert_eq!(result["run"]["id"], run_id);
        assert_eq!(result["run"]["outcome"], "ok");
        assert_eq!(
            result["run"]["result_summary"],
            "completed before pane registration"
        );
        assert_eq!(result["card"]["column_id"], target_id);
        assert_eq!(result["card"]["status"], "idle");

        // A late pane-exit callback must not turn the already-successful run
        // into a configured-harness failure.
        let pane_exit = handle_request(
            &d,
            "run.pane_exited",
            json!({"card_id": card_id, "run_id": run_id}),
        )
        .unwrap_err();
        assert!(pane_exit.to_string().contains("no open run"), "{pane_exit}");
        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert_eq!(run.outcome, Some(RunOutcome::Ok));
        assert!(run.ended_at.is_some());
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.column_id, target_id);
        assert_eq!(card.status, CardStatus::Idle);
    }

    #[test]
    fn run_done_rejects_queued_configured_runs_without_or_with_stale_run_id() {
        for stale in [false, true] {
            let mut config = Config::default();
            config.harness.insert(
                "custom".into(),
                HarnessDef {
                    argv: vec!["custom-agent".into()],
                    ..Default::default()
                },
            );
            let d = test_daemon(config);
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: "queued callback identity".into(),
                        harness: Some("custom".into()),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, "custom", "[]", "p", None, None)
                    .unwrap();
                db.set_card_status(card.id, CardStatus::Queued).unwrap();
                (card.id, run.id)
            };

            let params = if stale {
                json!({"card_id": card_id, "run_id": run_id + 1, "outcome": "ok"})
            } else {
                json!({"card_id": card_id, "outcome": "ok"})
            };
            let err = handle_request(&d, "run.done", params).unwrap_err();
            assert!(err.to_string().contains("no active run"), "{err}");

            let db = d.store.lock();
            let run = db.get_run(run_id).unwrap();
            assert!(run.ended_at.is_none());
            assert!(run.outcome.is_none());
            assert_eq!(
                db.get_card(card_id).unwrap().unwrap().status,
                CardStatus::Queued
            );
        }
    }

    #[test]
    fn run_done_rejects_mismatching_run_id_for_a_different_active_replacement() {
        let mut config = Config::default();
        config.harness.insert(
            "custom".into(),
            HarnessDef {
                argv: vec!["custom-agent".into()],
                ..Default::default()
            },
        );
        let d = test_daemon(config);
        let (card_id, stale_run_id, active_run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "replacement callback identity".into(),
                    harness: Some("custom".into()),
                    ..Default::default()
                })
                .unwrap();
            let stale = db
                .create_run(card.id, card.column_id, "custom", "[]", "old", None, None)
                .unwrap();
            db.start_run(stale.id, Some("w1"), Some("old-pane"))
                .unwrap();
            db.finish_run(stale.id, RunOutcome::Fail, Some("replaced"))
                .unwrap();

            let active = db
                .create_run(card.id, card.column_id, "custom", "[]", "new", None, None)
                .unwrap();
            db.start_run(active.id, Some("w1"), Some("new-pane"))
                .unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
            (card.id, stale.id, active.id)
        };

        let err = handle_request(
            &d,
            "run.done",
            json!({
                "card_id": card_id,
                "run_id": stale_run_id,
                "outcome": "ok"
            }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("run"), "{err}");

        let db = d.store.lock();
        assert_eq!(
            db.get_run(stale_run_id).unwrap().outcome,
            Some(RunOutcome::Fail)
        );
        assert!(db.get_run(active_run_id).unwrap().ended_at.is_none());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
    }

    #[test]
    fn run_done_rejects_queued_builtin_runs_before_pane_registration() {
        for harness in ["pi", "claude"] {
            let d = test_daemon(Config::default());
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("queued builtin {harness}"),
                        harness: Some(harness.into()),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, harness, "[]", "p", None, None)
                    .unwrap();
                db.set_card_status(card.id, CardStatus::Queued).unwrap();
                (card.id, run.id)
            };

            let err = handle_request(
                &d,
                "run.done",
                json!({"card_id": card_id, "run_id": run_id, "outcome": "ok"}),
            )
            .unwrap_err();
            assert!(err.to_string().contains("no active run"), "{err}");

            let db = d.store.lock();
            let run = db.get_run(run_id).unwrap();
            assert!(run.ended_at.is_none());
            assert!(run.outcome.is_none());
            assert_eq!(
                db.get_card(card_id).unwrap().unwrap().status,
                CardStatus::Queued
            );
        }
    }

    #[test]
    fn pane_exited_finalizes_matching_run_without_on_fail_transition() {
        let d = test_daemon(Config::default());
        let (card_id, run_id, source_id) = {
            let db = d.store.lock();
            let target = db
                .create_column(&ColumnCreateParams {
                    name: "pane-exit target".into(),
                    ..Default::default()
                })
                .unwrap();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "pane-exit source".into(),
                    trigger: Some(Trigger::Auto),
                    on_fail_column_id: Some(target.id),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "silent configured harness".into(),
                    column_id: Some(source.id),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "fake", "[]", "silent", None, None)
                .unwrap();
            db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
            (card.id, run.id, source.id)
        };

        let stale = handle_request(
            &d,
            "run.pane_exited",
            json!({"card_id": card_id, "run_id": run_id + 1}),
        )
        .unwrap_err();
        assert!(stale.to_string().contains("run"));
        {
            let db = d.store.lock();
            assert!(db.get_run(run_id).unwrap().ended_at.is_none());
            let card = db.get_card(card_id).unwrap().unwrap();
            assert_eq!(card.status, CardStatus::Running);
            assert_eq!(card.column_id, source_id);
        }

        let res = handle_request(
            &d,
            "run.pane_exited",
            json!({"card_id": card_id, "run_id": run_id}),
        )
        .unwrap();
        assert_eq!(res["run"]["outcome"], "fail");
        assert_eq!(
            res["run"]["result_summary"],
            "configured harness exited without calling board done"
        );
        assert_eq!(res["card"]["status"], "failed");
        assert_eq!(res["card"]["column_id"], source_id);
        let detail = handle_request(&d, "card.get", json!({"id": card_id})).unwrap();
        assert!(detail["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|comment| comment["body"] == "pane exited without board done"));
    }

    #[test]
    fn pane_exited_accepts_matching_queued_configured_run() {
        let d = test_daemon(Config::default());
        let (card_id, run_id, column_id) = {
            let db = d.store.lock();
            let column = db
                .create_column(&ColumnCreateParams {
                    name: "queued configured".into(),
                    on_fail_column_id: Some(db.default_column_id(BOARD_ID).unwrap()),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "callback before registration".into(),
                    column_id: Some(column.id),
                    harness: Some("custom".into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, column.id, "custom", "[]", "p", None, None)
                .unwrap();
            db.set_card_status(card.id, CardStatus::Queued).unwrap();
            (card.id, run.id, column.id)
        };

        let result = handle_request(
            &d,
            "run.pane_exited",
            json!({"card_id": card_id, "run_id": run_id}),
        )
        .unwrap();

        assert_eq!(result["run"]["id"], run_id);
        assert_eq!(result["run"]["outcome"], "fail");
        assert_eq!(result["card"]["status"], "failed");
        assert_eq!(result["card"]["column_id"], column_id);
        let detail = handle_request(&d, "card.get", json!({"id": card_id})).unwrap();
        assert!(detail["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|comment| { comment["body"] == "pane exited without board done" }));
    }

    #[test]
    fn pane_exited_rejects_builtin_runs_without_mutating_them() {
        for harness in ["pi", "claude"] {
            let d = test_daemon(Config::default());
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("builtin {harness}"),
                        harness: Some(harness.into()),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, harness, "[]", "p", None, None)
                    .unwrap();
                db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
                db.set_card_status(card.id, CardStatus::Running).unwrap();
                (card.id, run.id)
            };

            let err = handle_request(
                &d,
                "run.pane_exited",
                json!({"card_id": card_id, "run_id": run_id}),
            )
            .unwrap_err();
            assert!(err.to_string().contains("configured harness"), "{err}");

            let db = d.store.lock();
            let run = db.get_run(run_id).unwrap();
            assert!(run.ended_at.is_none());
            assert!(run.outcome.is_none());
            assert_eq!(
                db.get_card(card_id).unwrap().unwrap().status,
                CardStatus::Running
            );
            assert!(db.list_comments(card_id).unwrap().is_empty());
        }
    }

    #[test]
    fn run_retry_rejects_every_kind_of_open_run_from_db_truth() {
        for (status, started) in [
            (CardStatus::Queued, false),
            (CardStatus::Blocked, true),
            (CardStatus::Awaiting, true),
        ] {
            let d = test_daemon(Config::default());
            let card_id = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("{status:?}"),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                    .unwrap();
                if started {
                    db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
                }
                if status == CardStatus::Awaiting {
                    db.set_card_awaiting(card.id, AwaitingReason::IdleExpired)
                        .unwrap();
                } else {
                    db.set_card_status(card.id, status).unwrap();
                }
                card.id
            };
            let err = handle_request(&d, "run.retry", json!({"card_id": card_id})).unwrap_err();
            assert_eq!(err.code(), 3);
            assert!(err.to_string().contains("open run"));
        }
    }

    #[test]
    fn finalizing_card_rejects_retry_and_conflicting_mutations() {
        let d = test_daemon(Config::default());
        let (card_id, source_id, target_id) = {
            let db = d.store.lock();
            let target_id = db.default_column_id(BOARD_ID).unwrap();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "Finalizing source".into(),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "finalizing".into(),
                    column_id: Some(source.id),
                    ..Default::default()
                })
                .unwrap();
            (card.id, source.id, target_id)
        };
        d.sched.lock().unwrap().finalizing_cards.insert(card_id, 77);

        for (method, params) in [
            ("run.done", json!({"card_id": card_id, "outcome": "ok"})),
            ("run.retry", json!({"card_id": card_id})),
            ("run.cancel", json!({"card_id": card_id})),
            ("card.delete", json!({"id": card_id})),
            ("card.archive", json!({"id": card_id, "archived": true})),
            (
                "card.update",
                json!({"id": card_id, "model": "locked-model"}),
            ),
            ("card.move", json!({"id": card_id, "column_id": target_id})),
            ("column.update", json!({"id": source_id, "trigger": "auto"})),
            (
                "column.delete",
                json!({"id": source_id, "move_cards_to": target_id}),
            ),
        ] {
            let err = handle_request(&d, method, params).unwrap_err();
            assert_eq!(err.code(), 3, "{method}: {err}");
            assert!(err.to_string().contains("finalization"), "{method}: {err}");
        }

        let card = d.store.lock().get_card(card_id).unwrap().unwrap();
        assert_eq!(card.column_id, source_id);
        assert!(card.archived_at.is_none());
        assert_eq!(card.model, None);
        assert_eq!(
            d.store
                .lock()
                .get_column(source_id)
                .unwrap()
                .unwrap()
                .trigger,
            Trigger::Manual
        );
        assert!(d.store.lock().list_runs(card_id).unwrap().is_empty());
        assert_eq!(
            d.sched.lock().unwrap().finalizing_cards.get(&card_id),
            Some(&77)
        );
    }

    #[test]
    fn card_delete_rejects_and_preserves_queued_blocked_and_awaiting_open_runs() {
        for (status, started) in [
            (CardStatus::Queued, false),
            (CardStatus::Blocked, true),
            (CardStatus::Awaiting, true),
        ] {
            let d = test_daemon(Config::default());
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("{status:?}"),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                    .unwrap();
                if started {
                    db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
                }
                if status == CardStatus::Awaiting {
                    db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                        .unwrap();
                } else {
                    db.set_card_status(card.id, status).unwrap();
                }
                (card.id, run.id)
            };

            let err = handle_request(&d, "card.delete", json!({"id": card_id})).unwrap_err();
            assert_eq!(err.code(), 3);
            assert!(err.to_string().contains("open run"));
            let db = d.store.lock();
            assert!(db.get_card(card_id).unwrap().is_some());
            assert!(db.get_run(run_id).unwrap().ended_at.is_none());
        }
    }

    #[test]
    fn card_locked_field_update_rejects_queued_blocked_and_awaiting_open_runs() {
        for (status, started) in [
            (CardStatus::Queued, false),
            (CardStatus::Blocked, true),
            (CardStatus::Awaiting, true),
        ] {
            let d = test_daemon(Config::default());
            let card_id = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("{status:?}"),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                    .unwrap();
                if started {
                    db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
                }
                if status == CardStatus::Awaiting {
                    db.set_card_awaiting(card.id, AwaitingReason::IdleExpired)
                        .unwrap();
                } else {
                    db.set_card_status(card.id, status).unwrap();
                }
                card.id
            };

            let err = handle_request(
                &d,
                "card.update",
                json!({"id": card_id, "model": "locked-model"}),
            )
            .unwrap_err();
            assert_eq!(err.code(), 3);
            assert!(err.to_string().contains("open run"));

            // Unlocked metadata remains editable while a run is open.
            let updated = handle_request(
                &d,
                "card.update",
                json!({"id": card_id, "title": "new title"}),
            )
            .unwrap();
            assert_eq!(updated["title"], "new title");
        }
    }

    #[test]
    fn card_open_run_db_guard_wins_over_stale_nonbusy_status() {
        let d = test_daemon(Config::default());
        let card_id = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "stale status".into(),
                    ..Default::default()
                })
                .unwrap();
            db.create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                .unwrap();
            db.set_card_status(card.id, CardStatus::Done).unwrap();
            card.id
        };

        let edit_err = handle_request(
            &d,
            "card.update",
            json!({"id": card_id, "model": "locked-model"}),
        )
        .unwrap_err();
        assert_eq!(edit_err.code(), 3);
        assert!(edit_err.to_string().contains("open run"));

        let archive_err =
            handle_request(&d, "card.archive", json!({"id": card_id, "archived": true}))
                .unwrap_err();
        assert_eq!(archive_err.code(), 3);
        assert!(archive_err.to_string().contains("open run"));
    }

    #[test]
    fn column_delete_rejects_queued_blocked_and_awaiting_open_runs() {
        for (status, started) in [
            (CardStatus::Queued, false),
            (CardStatus::Blocked, true),
            (CardStatus::Awaiting, true),
        ] {
            let d = test_daemon(Config::default());
            let (source_id, target_id) = {
                let db = d.store.lock();
                let target_id = db.default_column_id(BOARD_ID).unwrap();
                let source = db
                    .create_column(&ColumnCreateParams {
                        name: "Source".into(),
                        ..Default::default()
                    })
                    .unwrap();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("{status:?}"),
                        column_id: Some(source.id),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .create_run(card.id, source.id, "pi", "[]", "p", None, None)
                    .unwrap();
                if started {
                    db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
                }
                if status == CardStatus::Awaiting {
                    db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                        .unwrap();
                } else {
                    db.set_card_status(card.id, status).unwrap();
                }
                (source.id, target_id)
            };

            let err = handle_request(
                &d,
                "column.delete",
                json!({"id": source_id, "move_cards_to": target_id}),
            )
            .unwrap_err();
            assert_eq!(err.code(), 3);
            assert!(err.to_string().contains("open run"));
        }
    }

    fn add_run_with_pane(d: &Arc<Daemon>, pane: Option<&str>) -> i64 {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "focus target".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
            .unwrap();
        db.start_run(run.id, Some("w1"), pane).unwrap();
        card.id
    }

    fn fake_herdr(reply: &'static str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let stream = incoming.unwrap();
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(request["method"], "pane.focus");
                let id = request["id"].as_str().unwrap();
                writeln!(writer, "{{\"id\":\"{id}\",{reply}}}").unwrap();
                break;
            }
        });
        (dir, path)
    }

    #[test]
    fn run_focus_rejects_missing_pane_and_cross_session_socket() {
        let d = test_daemon(Config::default());
        let card_id = add_run_with_pane(&d, None);
        let err = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"origin_socket":"/tmp/origin.sock"}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 2);
        assert!(err.to_string().contains("pane"));

        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("target.sock");
        let _listener = UnixListener::bind(&target).unwrap();
        let origin_dir = tempfile::tempdir().unwrap();
        let origin = origin_dir.path().join("origin.sock");
        let _origin_listener = UnixListener::bind(&origin).unwrap();
        let d = test_daemon_with_registry(
            Config::default(),
            Some(SessionRegistry::new(target.clone())),
        );
        let card_id = add_run_with_pane(&d, Some("w1:p2"));
        let err = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"origin_socket":origin}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("different Herdr session"));
    }

    #[test]
    fn run_focus_propagates_herdr_error_and_returns_success_ids() {
        let (_dir, socket) =
            fake_herdr("\"error\":{\"code\":\"pane_not_found\",\"message\":\"gone\"}");
        let d = test_daemon_with_registry(
            Config::default(),
            Some(SessionRegistry::new(socket.clone())),
        );
        let card_id = add_run_with_pane(&d, Some("w1:p9"));
        let err = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"origin_socket":socket}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 4);
        assert!(err.to_string().contains("gone"));

        let (_dir, socket) = fake_herdr(
            "\"result\":{\"type\":\"pane_info\",\"pane\":{\"pane_id\":\"w1:p9\",\"terminal_id\":\"term\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\"focused\":true,\"revision\":0,\"agent_status\":\"idle\"}}",
        );
        let d = test_daemon_with_registry(
            Config::default(),
            Some(SessionRegistry::new(socket.clone())),
        );
        let card_id = add_run_with_pane(&d, Some("w1:p9"));
        let result = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"origin_socket":socket}),
        )
        .unwrap();
        assert_eq!(result["pane_id"], "w1:p9");
        assert!(result["run_id"].as_i64().unwrap() > 0);
    }

    #[test]
    fn harness_list_builtin_only() {
        let d = test_daemon(Config::default());
        let v = handle_request(&d, "harness.list", json!({})).unwrap();
        let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
        assert_eq!(names, vec!["pi".to_string(), "claude".to_string()]);
    }

    #[test]
    fn harness_list_includes_config_defined() {
        let mut config = Config::default();
        config.harness.insert(
            "fake".to_string(),
            HarnessDef {
                argv: vec!["bash".into(), "fake.sh".into()],
                ..Default::default()
            },
        );
        let d = test_daemon(config);
        let v = handle_request(&d, "harness.list", json!({})).unwrap();
        let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
        assert_eq!(names, vec!["pi", "claude", "fake"]);
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
    fn harness_capabilities_pi_ok() {
        let d = test_daemon(Config::default());
        let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
        assert_eq!(v["harness"], "pi");
        assert_eq!(v["model_freeform"], true);
        assert!(v["models"].as_array().unwrap().is_empty());
        assert!(v["permission_modes"].as_array().unwrap().is_empty());
        assert!(v["default_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effort| effort == "low"));
    }

    #[test]
    fn harness_capabilities_pi_overlays_live_catalog() {
        // A pi agent dir with auth.json + models-store.json → the daemon
        // overlays real models (per-model efforts) onto the pi catalog.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            r#"{"zai": {"type": "api_key"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models-store.json"),
            r#"{"zai": {"models": [{"id": "glm-5.2", "reasoning": true,
                 "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh"}}]}}"#,
        )
        .unwrap();
        let config = Config {
            pi_agent_dir: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let d = test_daemon(config);

        let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
        let models = v["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "zai/glm-5.2");
        // Per-model efforts come from thinkingLevelMap, in canonical order.
        let efforts: Vec<&str> = models[0]["efforts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(efforts, vec!["minimal", "xhigh"]);
        // model_freeform stays true: arbitrary model strings are still accepted.
        assert_eq!(v["model_freeform"], true);
    }

    #[test]
    fn harness_capabilities_pi_falls_back_to_static_without_agent_dir() {
        // No pi_agent_dir (tests) → static free-form catalog (models: []).
        let d = test_daemon(Config::default());
        let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
        assert!(v["models"].as_array().unwrap().is_empty());
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
        assert!(msg.contains("pi"), "message lists Pi: {msg}");
        assert!(msg.contains("claude"), "message lists Claude: {msg}");
    }

    #[test]
    fn card_create_rejects_pi_permission_mode() {
        let d = test_daemon(Config::default());
        let err = handle_request(
            &d,
            "card.create",
            json!({ "title": "bad", "harness": "pi", "permission_mode": "acceptEdits" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1);
        assert!(err.to_string().contains("permission mode"));
    }

    #[test]
    fn switching_card_to_pi_rejects_incompatible_permission() {
        let d = test_daemon(Config::default());
        let created = handle_request(
            &d,
            "card.create",
            json!({
                "title": "switch",
                "harness": "claude",
                "permission_mode": "acceptEdits"
            }),
        )
        .unwrap();
        let err = handle_request(
            &d,
            "card.update",
            json!({ "id": created["id"], "harness": "pi" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1);
        let unchanged = d
            .store
            .lock()
            .get_card(created["id"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.harness, "claude");
        assert_eq!(unchanged.permission_mode.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn switching_card_from_pi_to_claude_rejects_incompatible_effort() {
        let d = test_daemon(Config::default());
        let created = handle_request(
            &d,
            "card.create",
            json!({ "title": "switch", "harness": "pi", "effort": "off" }),
        )
        .unwrap();
        let err = handle_request(
            &d,
            "card.update",
            json!({ "id": created["id"], "harness": "claude" }),
        )
        .unwrap_err();
        assert_eq!(err.code(), 1);
        let unchanged = d
            .store
            .lock()
            .get_card(created["id"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.harness, "pi");
        assert_eq!(unchanged.effort, Some(Effort::Off));
    }

    #[test]
    fn card_archive_roundtrip_and_busy_rejection() {
        let d = test_daemon(Config::default());
        let created = handle_request(&d, "card.create", json!({ "title": "archive me" })).unwrap();
        let id = created["id"].as_i64().unwrap();

        let archived =
            handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap();
        assert!(archived["archived_at"].is_string());

        let restored =
            handle_request(&d, "card.archive", json!({ "id": id, "archived": false })).unwrap();
        assert!(restored["archived_at"].is_null());

        d.store
            .lock()
            .set_card_status(id, CardStatus::Running)
            .unwrap();
        let err =
            handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("cancel it before archiving"));
    }

    #[test]
    fn archived_card_cannot_move_until_restored() {
        let d = test_daemon(Config::default());
        let created = handle_request(&d, "card.create", json!({ "title": "inert" })).unwrap();
        let id = created["id"].as_i64().unwrap();
        handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap();
        let err = handle_request(&d, "card.move", json!({ "id": id, "column_id": 1 })).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("restored before moving"));
    }

    #[test]
    fn space_list_without_herdr_is_herdr_unavailable() {
        let d = test_daemon(Config::default());
        let err = handle_request(&d, "space.list", json!({})).unwrap_err();
        assert_eq!(err.code(), 4);
    }
}
