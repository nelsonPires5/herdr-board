//! Local child-process liveness polling.

use std::sync::Arc;
use std::time::Duration;

use board_core::protocol::RunOutcome;

use crate::dispatch::finalize_run;
use crate::state::Daemon;

use super::run_open;

// -- LocalSpawner liveness poller -------------------------------------------

/// Every `local_poll_ms`: detect local child processes that exited without a
/// `board done` and finalize them per the pane-exit rule (fail, no transition).
pub(super) async fn local_liveness_poller(d: Arc<Daemon>) {
    let mut rx = d.shutdown_rx();
    let mut iv = tokio::time::interval(Duration::from_millis(d.settings.local_poll_ms));
    loop {
        tokio::select! {
            _ = iv.tick() => poll_once(&d).await,
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
}

async fn poll_once(d: &Arc<Daemon>) {
    let candidates: Vec<(i64, crate::spawner::RuntimeHandle)> = {
        let s = d.sched.lock().unwrap();
        s.active
            .iter()
            .filter(|(_, a)| a.is_local)
            .map(|(id, a)| (*id, a.handle.clone()))
            .collect()
    };
    for (run_id, handle) in candidates {
        let spawner = d.spawner.clone();
        let alive = tokio::task::spawn_blocking(move || spawner.is_alive(&handle))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
        if alive {
            continue;
        }
        if run_open(d, run_id) {
            let msg = "pane exited without board done".to_string();
            if let Err(e) = finalize_run(
                d,
                run_id,
                RunOutcome::Fail,
                Some(msg.clone()),
                Some(msg),
                false,
                false,
            ) {
                tracing::warn!("liveness finalize run {run_id}: {e}");
            }
        } else {
            // Already finalized elsewhere; just drop our bookkeeping.
            d.sched.lock().unwrap().active.remove(&run_id);
            d.refresh_watch();
        }
    }
}
