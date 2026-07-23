//! Synchronous request handlers for every protocol method (except
//! `events.subscribe`, handled by the connection layer). DB work is quick and
//! serialized; spawning is deferred to the dispatcher via `wake_dispatch`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use board_core::capability::{available_harnesses, capabilities_for};
use board_core::db::{FinalizeRun, BOARD_ID};
use board_core::engine::{
    decide_entry, decide_lifecycle, merge_card_update, merge_column_update, validate_card_archive,
    validate_card_edit, validate_card_settings, validate_card_values, validate_column_delete,
    validate_column_update, validate_column_values, LifecycleAction, LifecycleDecision,
    LifecycleFacts, LifecycleHarness, LifecycleRejection, PermissionContext,
};
use board_core::harness::DEFAULT_HARNESS;
use board_core::model::Run;
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

fn lifecycle_facts(
    run: &Run,
    card: &board_core::model::Card,
    supplied_run_id: Option<i64>,
) -> LifecycleFacts {
    LifecycleFacts {
        open_run_id: Some(run.id),
        supplied_run_id,
        started: run.started_at.is_some(),
        harness: if board_core::harness::is_builtin_harness(&run.harness) {
            LifecycleHarness::BuiltIn
        } else {
            LifecycleHarness::Configured
        },
        card_status: card.status,
    }
}

fn lifecycle_rejection(card_id: i64, rejection: LifecycleRejection) -> Error {
    match rejection {
        LifecycleRejection::NoOpenRun
        | LifecycleRejection::QueuedCompletionRequiresRunId
        | LifecycleRejection::QueuedBuiltinCompletion => {
            Error::NotFound(format!("no active run for card {card_id}"))
        }
        LifecycleRejection::SuppliedRunIdMismatch { expected, supplied } => Error::InvalidState(
            format!(
                "no active run for card {card_id}: run {supplied} does not match active run {expected}"
            ),
        ),
        LifecycleRejection::PaneExitRequiresRunId => Error::InvalidState(format!(
            "pane-exited callback for card {card_id} must supply a run id"
        )),
        LifecycleRejection::PaneExitBuiltin => Error::InvalidState(
            "pane-exited callback is only valid for configured harnesses".into(),
        ),
        LifecycleRejection::TimeoutBeforeStart => {
            Error::InvalidState(format!("run for card {card_id} has not started"))
        }
        LifecycleRejection::TimeoutPaused => {
            Error::InvalidState(format!("run for card {card_id} is awaiting review"))
        }
    }
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
        active_runs: db.active_run_summaries(board_id)?,
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
        let _sched = d.sched.lock().unwrap();
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
        // Match finalization's scheduler→store order so the delete and its
        // open-run check cannot interleave with the final transaction.
        let _sched = d.sched.lock().unwrap();
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
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        // The scheduler→store critical section serializes this validation and
        // update with an entire finalization transaction.
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
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
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
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
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
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let current = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
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
    let (run, plan) = {
        // Keep the scheduler -> store lock order used by the normal active-run
        // path so eligibility and finalization serialize. The pure engine owns callback
        // eligibility, including the narrow queued configured-run exception.
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let run = db
            .open_run_for_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("no active run for card {}", p.card_id)))?;
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        let facts = lifecycle_facts(&run, &card, p.run_id);
        let decision = decide_lifecycle(&facts, LifecycleAction::Done { outcome: p.outcome });
        let LifecycleDecision::Finalize(plan) = decision else {
            let LifecycleDecision::Reject(rejection) = decision else {
                unreachable!("lifecycle decision matched twice")
            };
            return Err(lifecycle_rejection(p.card_id, rejection));
        };
        (run, plan)
    };
    let (run, card) = finalize_run(
        d,
        run.id,
        plan.outcome,
        p.summary,
        None,
        plan.kill,
        plan.transition,
    )?;
    Ok(json!(RunActionResult { run, card }))
}

fn run_pane_exited(d: &Arc<Daemon>, p: RunPaneExitedParams) -> Result<Value> {
    let (run, plan) = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let open = db
            .open_run_for_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("no open run for card {}", p.card_id)))?;
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        let facts = lifecycle_facts(&open, &card, Some(p.run_id));
        let decision = decide_lifecycle(&facts, LifecycleAction::PaneExited);
        let LifecycleDecision::Finalize(plan) = decision else {
            let LifecycleDecision::Reject(rejection) = decision else {
                unreachable!("lifecycle decision matched twice")
            };
            return Err(lifecycle_rejection(p.card_id, rejection));
        };
        (open, plan)
    };

    let (run, card) = finalize_run(
        d,
        run.id,
        plan.outcome,
        Some("configured harness exited without calling board done".into()),
        Some("pane exited without board done".into()),
        plan.kill,
        plan.transition,
    )?;
    Ok(json!(RunActionResult { run, card }))
}

fn run_cancel(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    // Prefer the active run; else cancel the latest queued run for the card.
    let active = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
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

    // Keep queued verification and finalization in one scheduler→store critical
    // section. Otherwise dispatch could promote the run between them, leaving
    // a newly spawned process alive when this no-kill cancellation wins.
    let effects = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let queued = db
            .open_run_for_card(p.card_id)?
            .filter(|run| run.started_at.is_none())
            .ok_or_else(|| {
                Error::NotFound(format!("no active or queued run for card {}", p.card_id))
            })?;
        let effects = db.finalize_run_uow(&FinalizeRun {
            run_id: queued.id,
            outcome: RunOutcome::Cancelled,
            summary: Some("cancelled by user"),
            comments: &[("system", "queued run cancelled by user")],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })?;
        sched.chain_hops.remove(&p.card_id);
        #[cfg(test)]
        d.record_effect("scheduler");
        effects
    };
    d.refresh_watch();
    d.emit_run_ended(p.card_id, effects.finished_run.id, RunOutcome::Cancelled);
    d.wake_dispatch();
    Ok(json!(RunActionResult {
        run: effects.finished_run,
        card: effects.card,
    }))
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
mod tests;
