//! Startup recovery: what happens to runs that were started but never ended
//! when the daemon comes back up.
//!
//! Both branches are conservative. Under the Herdr spawner the always-on
//! supervisor owns reconciliation (a Herdr server may appear *after* boardd, so
//! this must not depend on the startup best-effort client); under the local
//! spawner the runs are adopted or lost here.

use std::sync::Arc;

use crate::dispatch;
use crate::settings::SpawnerKind;
use crate::spawner::RuntimeHandle;
use crate::state::{ActiveRun, Daemon};
use crate::supervisor;

pub(crate) async fn startup_recovery(d: &Arc<Daemon>) {
    if matches!(d.settings.spawner, SpawnerKind::Herdr) {
        if let Some(registry) = &d.session_registry {
            startup_recovery_with(
                d,
                Arc::new(crate::session::SessionRegistry::new(
                    registry.default_socket().to_path_buf(),
                )),
                Arc::new(supervisor::HerdrRuntime),
                Arc::new(supervisor::SystemClock),
            )
            .await;
        }
    } else {
        adopt_runs(d).await;
    }
}

/// Injectable startup branch used to prove that Herdr reconciliation runs even
/// when the daemon's initial best-effort client connection failed.
pub(crate) async fn startup_recovery_with(
    d: &Arc<Daemon>,
    resolver: Arc<dyn supervisor::SessionResolver>,
    runtime: Arc<dyn supervisor::Runtime>,
    clock: Arc<dyn supervisor::ReconcileClock>,
) {
    if matches!(d.settings.spawner, SpawnerKind::Herdr) {
        supervisor::reconcile_once(d, resolver, runtime, clock).await;
    } else {
        adopt_runs(d).await;
    }
}

/// On startup, reconcile runs that were started but never ended.
async fn adopt_runs(d: &Arc<Daemon>) {
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(error_category = "database", "adoption: active_runs failed");
            return;
        }
    };
    for (run, card) in active {
        // Resolve the run's session socket so kill/liveness target the right
        // session after a restart (default session → None handle socket).
        let herdr_socket = d.session_registry.as_ref().and_then(|reg| {
            reg.resolve(run.session.as_deref())
                .ok()
                .filter(|r| Some(r.socket.as_path()) != Some(reg.default_socket()))
                .map(|r| r.socket)
        });
        let handle = RuntimeHandle {
            pane_id: run.herdr_pane_id.clone(),
            workspace_id: run.herdr_workspace_id.clone(),
            anchor_pane_id: run.herdr_anchor_pane_id.clone(),
            pid: None,
            herdr_socket,
        };
        let alive = if handle.pane_id.is_some() {
            let spawner = d.spawner.clone();
            let h = handle.clone();
            tokio::task::spawn_blocking(move || spawner.is_alive(&h))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(false)
        } else {
            false
        };

        if alive {
            tracing::info!("adopting live run {} (card {})", run.id, card.id);
            let adopted_at = std::time::Instant::now();
            let wall_now_ms = d.wall_now_ms();
            // v9 deadlines are authoritative: restart never grants a fresh budget.
            let deadline = ActiveRun::reconstruct_deadline(
                adopted_at,
                wall_now_ms,
                run.timeout_deadline_at_ms,
            );
            let mut sched = d.sched.lock().unwrap();
            sched.active.insert(
                run.id,
                ActiveRun {
                    card_id: card.id,
                    handle,
                    started: adopted_at,
                    timeout_deadline: deadline,
                    idle_since: None,
                    awaiting_since: ActiveRun::reconstruct_awaiting_since(
                        adopted_at,
                        wall_now_ms,
                        run.timeout_paused_at_ms,
                    ),
                    is_local: false,
                    pane_id: run.herdr_pane_id.clone(),
                },
            );
            drop(sched);
            d.refresh_watch();
        } else {
            tracing::info!("run {} (card {}) lost across restart", run.id, card.id);
            let msg = "daemon restart: run lost".to_string();
            if let Err(_error) = dispatch::finalize_run(
                d,
                run.id,
                board_core::protocol::RunOutcome::Fail,
                Some(msg.clone()),
                Some(msg),
                false,
                false,
            ) {
                tracing::error!(
                    run_id = run.id,
                    card_id = card.id,
                    error_category = "database",
                    "adoption could not finalize a lost run; it stays open"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
