//! Timeout and idle-expiry classification and application.

use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::engine::format_duration;
use board_core::protocol::{CardStatus, RunOutcome};

use crate::dispatch::finalize_run_timeout;
use crate::state::Daemon;

// -- timeout / idle ticker ---------------------------------------------------

/// Every `tick_ms`: kill runs past their column timeout (→ fail + on_fail) and
/// move runs idle beyond `idle_grace_seconds` to `awaiting` (the run stays
/// OPEN for human review — it is never auto-failed). Runs whose card is
/// `awaiting` are skipped entirely: the column timeout is paused and the idle
/// check no longer applies.
pub(super) async fn timeout_ticker(d: Arc<Daemon>) {
    let mut rx = d.shutdown_rx();
    let mut iv = tokio::time::interval(Duration::from_millis(d.settings.tick_ms));
    loop {
        tokio::select! {
            _ = iv.tick() => check(&d),
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
}

fn check(d: &Arc<Daemon>) {
    check_at(d, Instant::now());
}

#[derive(Debug)]
pub(super) struct Candidate {
    run_id: i64,
    card_id: i64,
    elapsed: Duration,
    timed_out: bool,
    observed_idle_since: Option<Instant>,
}

/// Snapshot and classify active runs. Cards already `awaiting` are skipped:
/// their run stays open and the column timeout is paused.
pub(super) fn classify_candidates(d: &Arc<Daemon>, now: Instant) -> Vec<Candidate> {
    let idle_grace = Duration::from_secs(d.config.idle_grace_seconds);
    let mut candidates = Vec::new();
    let s = d.sched.lock().unwrap();
    let store = d.store.lock();
    for (run_id, a) in &s.active {
        let awaiting = store
            .get_card(a.card_id)
            .ok()
            .flatten()
            .map(|c| c.status == CardStatus::Awaiting)
            .unwrap_or(false);
        if awaiting {
            continue;
        }
        let timed_out = a.timeout_deadline.is_some_and(|dl| now >= dl);
        let observed_idle_since = (!timed_out)
            .then_some(a.idle_since)
            .flatten()
            .filter(|idle| now.saturating_duration_since(*idle) >= idle_grace);
        if timed_out || observed_idle_since.is_some() {
            candidates.push(Candidate {
                run_id: *run_id,
                card_id: a.card_id,
                elapsed: now.saturating_duration_since(a.started),
                timed_out,
                observed_idle_since,
            });
        }
    }
    candidates
}

pub(super) fn apply_candidate(d: &Arc<Daemon>, c: Candidate, now: Instant) {
    if c.timed_out {
        let msg = format!(
            "run timed out after {}; applying on_fail",
            format_duration(Some(c.elapsed.as_secs() as i64))
        );
        if let Err(e) = finalize_run_timeout(
            d,
            c.run_id,
            now,
            RunOutcome::Fail,
            Some(msg.clone()),
            Some(msg),
            true,
            true,
        ) {
            tracing::warn!("timeout finalize run {}: {e}", c.run_id);
        }
    } else if let Some(observed_idle_since) = c.observed_idle_since {
        // Idle past the grace period without `board done`: the agent may have
        // finished silently. Awaiting (run open) — never a failure. Revalidate
        // the exact idle observation because a newer Working event may have
        // cleared or replaced it since classification.
        super::signals::apply_idle_expired(d, c.run_id, c.card_id, observed_idle_since, now);
    }
}

/// Deterministic timeout/idle pass. Tests inject `now`; the ticker uses the
/// current monotonic instant.
pub(super) fn check_at(d: &Arc<Daemon>, now: Instant) {
    for candidate in classify_candidates(d, now) {
        apply_candidate(d, candidate, now);
    }
}
