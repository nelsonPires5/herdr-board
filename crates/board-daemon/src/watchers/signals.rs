//! Signal guards and the single DB/effects application point.

use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::engine::{decide_signal, AgentSignal};
use board_core::protocol::{BoardChangedReason, CardStatus};
use board_herdr::NotificationSound;

use crate::state::Daemon;

// -- signal application ------------------------------------------------------

#[derive(Clone, Copy)]
enum SignalGuard {
    None,
    IdleExpired {
        observed_idle_since: Instant,
        now: Instant,
        grace: Duration,
    },
}

/// The single application point for engine signal decisions: watchers/ticker
/// emit [`AgentSignal`]s, [`decide_signal`] decides, this writes the decision
/// to the DB, maintains timeout-pause bookkeeping, and emits the board event
/// plus any notification. No-op for stale/no-op signals (engine `None`).
pub(crate) fn apply_signal(d: &Arc<Daemon>, run_id: i64, card_id: i64, signal: AgentSignal) {
    apply_signal_guarded(d, run_id, card_id, signal, SignalGuard::None);
}

pub(super) fn apply_idle_expired(
    d: &Arc<Daemon>,
    run_id: i64,
    card_id: i64,
    observed_idle_since: Instant,
    now: Instant,
) {
    apply_signal_guarded(
        d,
        run_id,
        card_id,
        AgentSignal::IdleExpired,
        SignalGuard::IdleExpired {
            observed_idle_since,
            now,
            grace: Duration::from_secs(d.config.idle_grace_seconds),
        },
    );
}

fn apply_signal_guarded(
    d: &Arc<Daemon>,
    run_id: i64,
    card_id: i64,
    signal: AgentSignal,
    guard: SignalGuard,
) {
    let applied_at = Instant::now();
    let wall_now_ms = d.wall_now_ms();
    let dec = {
        // Signals and finalizers share one lock order. The exact active run and
        // its open DB row are revalidated while both locks are held, so a run
        // removed by finalization can never write the card afterward.
        let mut sched = d.sched.lock().unwrap();
        let Some(active) = sched.active.get_mut(&run_id) else {
            return;
        };
        if active.card_id != card_id {
            return;
        }
        let db = d.store.lock();
        if let SignalGuard::IdleExpired {
            observed_idle_since,
            now,
            grace,
        } = guard
        {
            if active.idle_since != Some(observed_idle_since)
                || now.saturating_duration_since(observed_idle_since) < grace
            {
                return;
            }
        }
        let run = match db.get_run(run_id) {
            Ok(run) => run,
            Err(_) => return,
        };
        if run.card_id != card_id || run.started_at.is_none() || run.ended_at.is_some() {
            return;
        }
        let card = match db.get_card(card_id) {
            Ok(Some(card)) => card,
            _ => return,
        };
        let Some(dec) = decide_signal(card.status, signal) else {
            return;
        };

        let written = match dec.awaiting_reason {
            Some(reason) if card.status != CardStatus::Awaiting => {
                db.pause_run_timeout_uow(card_id, reason, wall_now_ms)
            }
            Some(reason) => db.set_card_awaiting(card_id, reason),
            None if card.status == CardStatus::Awaiting => {
                db.resume_run_timeout_uow(card_id, dec.new_status, wall_now_ms)
            }
            None => db.set_card_status(card_id, dec.new_status),
        };
        if let Err(e) = written {
            tracing::warn!("apply signal to card {card_id}: {e}");
            return;
        }

        // Timeout-pause bookkeeping is committed under the same locks as the
        // status write. Entering awaiting disarms idle tracking; leaving it
        // shifts the deadline by exactly the review span.
        match (card.status, dec.new_status) {
            (before, CardStatus::Awaiting) if before != CardStatus::Awaiting => {
                active.enter_awaiting(applied_at);
            }
            (CardStatus::Awaiting, after) if after != CardStatus::Awaiting => {
                active.leave_awaiting(applied_at);
            }
            _ => {}
        }
        dec
    };

    // Effects are deliberately outside both locks.
    let reason = if dec.new_status == CardStatus::Blocked {
        BoardChangedReason::RunBlocked
    } else {
        BoardChangedReason::CardUpdated
    };
    d.emit_changed(reason, Some(card_id), None);
    if let Some(msg) = dec.emit_notification {
        d.notify(
            format!("Card #{card_id}: {msg}"),
            None,
            NotificationSound::Request,
        );
    }
}
