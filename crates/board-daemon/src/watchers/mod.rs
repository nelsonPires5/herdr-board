//! Background watchers: timeout/idle, local liveness, and Herdr status events.
//!
//! The parent module keeps shared orchestration and the crate-private entrypoints;
//! each child owns one observation responsibility.

use std::sync::Arc;

use crate::state::Daemon;

mod herdr;
mod local;
mod signals;
mod timeout;

pub(crate) use signals::apply_signal;

/// Crate-private entrypoint for the timeout ticker.
pub async fn timeout_ticker(d: Arc<Daemon>) {
    timeout::timeout_ticker(d).await;
}

/// Crate-private entrypoint for local process liveness polling.
pub async fn local_liveness_poller(d: Arc<Daemon>) {
    local::local_liveness_poller(d).await;
}

/// Crate-private entrypoint for the Herdr socket supervisor.
pub fn herdr_event_thread(d: Arc<Daemon>) {
    herdr::herdr_event_thread(d);
}

/// Is the run still open (started, not ended) in the DB?
pub(super) fn run_open(d: &Arc<Daemon>, run_id: i64) -> bool {
    match d.store.lock().get_run(run_id) {
        Ok(r) => r.started_at.is_some() && r.ended_at.is_none(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests;
