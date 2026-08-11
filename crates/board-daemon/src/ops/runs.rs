use super::*;

use board_core::db::FinalizeRun;
use board_core::engine::{decide_lifecycle, LifecycleAction, LifecycleDecision};

use super::errors::{lifecycle_facts, lifecycle_rejection};
use crate::dispatch::{enqueue_run, finalize_run};

pub(super) fn run_done(d: &Arc<Daemon>, p: RunDoneParams) -> Result<Value> {
    let (run, plan) = {
        // Keep the scheduler -> store lock order used by the normal active-run
        // path so eligibility and finalization serialize. The pure engine owns callback
        // eligibility, including the narrow queued configured-run exception.
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db.require_card(p.card_id)?;
        let run = match db.open_run_for_card(p.card_id)? {
            Some(run) => run,
            None => {
                let manual_idle = card.status == CardStatus::Idle
                    && db.require_column(card.column_id)?.trigger == Trigger::Manual;
                let message = if manual_idle {
                    format!(
                        "no active run for card {}; manual column: close via board move {} <column>",
                        p.card_id,
                        p.card_id
                    )
                } else {
                    format!("no active run for card {}", p.card_id)
                };
                return Err(Error::NotFound(message));
            }
        };
        // A reused agent pane keeps its first stage's BOARD_RUN_ID (a running
        // process's env cannot change); when its HERDR_PANE_ID matches this
        // open run's pane, the caller is this run's pane, so the stale id is
        // treated as this run instead of being rejected as a mismatch.
        let actor_run_id = match p.actor_pane_id.as_deref() {
            Some(pane) if run.herdr_pane_id.as_deref() == Some(pane) => Some(run.id),
            _ => p.run_id,
        };
        let facts = lifecycle_facts(&run, &card, actor_run_id);
        let plan = match decide_lifecycle(&facts, LifecycleAction::Done { outcome: p.outcome }) {
            LifecycleDecision::Finalize(plan) => plan,
            LifecycleDecision::Reject(rejection) => {
                return Err(lifecycle_rejection(p.card_id, rejection))
            }
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
        let card = db.require_card(p.card_id)?;
        let facts = lifecycle_facts(&open, &card, Some(p.run_id));
        let plan = match decide_lifecycle(&facts, LifecycleAction::PaneExited) {
            LifecycleDecision::Finalize(plan) => plan,
            LifecycleDecision::Reject(rejection) => {
                return Err(lifecycle_rejection(p.card_id, rejection))
            }
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
    crate::rescue::focus_run(d, p)
}

pub(super) fn run_retry(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    let card = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db.require_card(p.card_id)?;
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
