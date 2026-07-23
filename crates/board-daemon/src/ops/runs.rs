use super::*;
use std::path::{Path, PathBuf};

use board_core::db::FinalizeRun;
use board_core::engine::{
    decide_lifecycle, LifecycleAction, LifecycleDecision, LifecycleFacts, LifecycleHarness,
    LifecycleRejection,
};
use board_core::harness::is_builtin_harness;
use board_core::model::Run;

use crate::dispatch::{enqueue_run, finalize_run};

fn lifecycle_facts(
    run: &Run,
    card: &board_core::model::Card,
    supplied_run_id: Option<i64>,
) -> LifecycleFacts {
    LifecycleFacts {
        open_run_id: Some(run.id),
        supplied_run_id,
        started: run.started_at.is_some(),
        harness: if is_builtin_harness(&run.harness) {
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

pub(super) fn run_done(d: &Arc<Daemon>, p: RunDoneParams) -> Result<Value> {
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

pub(super) fn run_pane_exited(d: &Arc<Daemon>, p: RunPaneExitedParams) -> Result<Value> {
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

pub(super) fn run_cancel(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
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

pub(super) fn run_focus(d: &Arc<Daemon>, p: RunFocusParams) -> Result<Value> {
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

pub(super) fn run_retry(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
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
