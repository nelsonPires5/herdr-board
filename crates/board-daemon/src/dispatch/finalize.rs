use std::sync::Arc;
use std::time::Instant;

use board_core::db::FinalizeRun;
use board_core::engine::{decide_auto_hop, decide_transition, AutoHopDecision};
use board_core::model::{Card, Run};
use board_core::protocol::{CardStatus, RunOutcome};
use board_core::{Error, Result};
use board_herdr::NotificationSound;

use crate::dispatch::enqueue::prepare_enqueue_values;
use crate::dispatch::PreparedEnqueue;
use crate::state::Daemon;

/// Finalize an active (started) run.
///
/// - `summary`: stored on the run as `result_summary`.
/// - `extra_comment`: an optional `system` comment posted before the transition
///   (e.g. the pane-exit / timeout reason). Distinct from the transition comment.
/// - `kill`: kill the underlying pane/process first (cancel/timeout).
/// - `transition`: apply the column's `on_success`/`on_fail` transition
///   (per [`decide_transition`]); `false` leaves the card put (pane-exit rule).
///
/// Returns the finished run and the card in its post-finalize state.
pub(crate) fn finalize_run(
    d: &Arc<Daemon>,
    run_id: i64,
    outcome: RunOutcome,
    summary: Option<String>,
    extra_comment: Option<String>,
    kill: bool,
    transition: bool,
) -> Result<(Run, Card)> {
    finalize_run_inner(
        d,
        run_id,
        outcome,
        summary,
        extra_comment,
        kill,
        transition,
        None,
    )?
    .ok_or_else(|| Error::InvalidState(format!("run {run_id} could not be claimed")))
}

/// Finalize a run selected by the timeout ticker, but only if its current DB
/// card is still non-awaiting at the atomic scheduler/DB claim point. A stale
/// timeout candidate returns `None` and leaves the run open.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_run_timeout(
    d: &Arc<Daemon>,
    run_id: i64,
    timeout_at: Instant,
    outcome: RunOutcome,
    summary: Option<String>,
    extra_comment: Option<String>,
    kill: bool,
    transition: bool,
) -> Result<Option<(Run, Card)>> {
    finalize_run_inner(
        d,
        run_id,
        outcome,
        summary,
        extra_comment,
        kill,
        transition,
        Some(timeout_at),
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize_run_inner(
    d: &Arc<Daemon>,
    run_id: i64,
    outcome: RunOutcome,
    summary: Option<String>,
    extra_comment: Option<String>,
    kill: bool,
    transition: bool,
    timeout_at: Option<Instant>,
) -> Result<Option<(Run, Card)>> {
    // Scheduler -> store is the sole lock order. The complete durable outcome
    // is committed while both locks are held; all external effects follow it.
    let (removed, effects, notify) = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let existing = db.get_run(run_id)?;
        if existing.ended_at.is_some() {
            let card = db
                .get_card(existing.card_id)?
                .ok_or_else(|| Error::NotFound(format!("card {}", existing.card_id)))?;
            return Ok(Some((existing, card)));
        }
        if let Some(classified_at) = timeout_at {
            let active_still_due = sched.active.get(&run_id).is_some_and(|active| {
                active.card_id == existing.card_id
                    && active
                        .timeout_deadline
                        .is_some_and(|deadline| classified_at >= deadline)
            });
            let card_is_awaiting = db
                .get_card(existing.card_id)?
                .is_some_and(|card| card.status == CardStatus::Awaiting);
            if !active_still_due || existing.started_at.is_none() || card_is_awaiting {
                return Ok(None);
            }
        }
        let elapsed = sched
            .active
            .get(&run_id)
            .map(|active| active.started.elapsed().as_secs() as i64);
        let mut card = db
            .get_card(existing.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", existing.card_id)))?;
        let mut comments = Vec::<String>::new();
        if let Some(comment) = extra_comment.as_ref() {
            comments.push(comment.clone());
        }
        let mut target_column_id = None;
        let mut final_status = match outcome {
            RunOutcome::Ok => CardStatus::Idle,
            _ => CardStatus::Failed,
        };
        let mut next = None;
        let mut next_hops = None;
        let mut notify = None;
        if transition {
            let current = db
                .get_column(existing.column_id)?
                .ok_or_else(|| Error::NotFound(format!("column {}", existing.column_id)))?;
            let cols = db.list_columns(card.board_id)?;
            let dec = decide_transition(&current, &cols, outcome, elapsed);
            comments.push(dec.system_comment.clone());
            target_column_id = dec.target_column_id;
            final_status = dec.new_status;
            if let Some(target_id) = dec.target_column_id {
                card.column_id = target_id;
                if dec.enqueue {
                    let current_hops = sched.chain_hops.get(&card.id).copied().unwrap_or(0);
                    match decide_auto_hop(current_hops, &dec) {
                        AutoHopDecision::Continue { hop } => {
                            next_hops = Some(hop);
                            next = Some(prepare_enqueue_values(d, &db, &card, target_id, false)?);
                        }
                        AutoHopDecision::Stop { message } => {
                            comments.push(message);
                            final_status = CardStatus::Failed;
                        }
                        AutoHopDecision::Reset => unreachable!(),
                    }
                } else if cols
                    .iter()
                    .find(|c| c.id == target_id)
                    .is_some_and(|c| c.trigger == board_core::protocol::Trigger::Manual)
                {
                    let target = cols.iter().find(|c| c.id == target_id).unwrap();
                    notify = Some((
                        format!("Card #{} ready for review", card.id),
                        format!("Entered {}", target.name),
                    ));
                }
            }
        }
        let comment_refs: Vec<(&str, &str)> = comments
            .iter()
            .map(|body| ("system", body.as_str()))
            .collect();
        let next_ref = next.as_ref().map(PreparedEnqueue::borrowed);
        let effects = db.finalize_run_uow(&FinalizeRun {
            run_id,
            outcome,
            summary: summary.as_deref(),
            comments: &comment_refs,
            target_column_id,
            final_status,
            final_awaiting_reason: None,
            next: next_ref,
        })?;
        let removed = sched.active.remove(&run_id);
        if let Some(hops) = next_hops {
            sched.chain_hops.insert(card.id, hops);
        } else {
            sched.chain_hops.remove(&card.id);
        }
        #[cfg(test)]
        d.record_effect("scheduler");
        (removed, effects, notify)
    };

    // Post-commit effects are deliberately ordered and contain no DB writes.
    d.refresh_watch();
    if kill {
        if let Some(active) = &removed {
            if let Err(e) = d.spawner.kill(&active.handle) {
                tracing::warn!("kill run {run_id} failed: {e}");
            }
        }
    }
    if let Some((title, body)) = notify {
        d.notify(title, Some(body), NotificationSound::Request);
    }
    d.emit_run_ended(effects.card.id, run_id, outcome);
    d.wake_dispatch();
    Ok(Some((effects.finished_run, effects.card)))
}
