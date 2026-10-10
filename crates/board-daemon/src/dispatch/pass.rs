use std::sync::Arc;

use board_core::model::{Card, Run};
use tracing::Instrument;

use crate::dispatch::launch_plan::spawn_one;
use crate::state::Daemon;

/// Admit queued runs in global FIFO order up to the global concurrency cap.
///
/// One span per pass, so the launches it fans out are attributable to the pass
/// that decided them. Passes are serialized, so this is not a hot loop.
#[tracing::instrument(name = "dispatch_pass", skip_all)]
pub(crate) async fn dispatch_pass(d: &Arc<Daemon>) {
    // A claim lives in this pass until spawn registration/failure is durable.
    // Serializing passes prevents another caller from observing those claimed
    // rows as queued and independently claiming the same run or capacity.
    let _pass = d.dispatch_pass.lock().await;
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(error_category = "database", "dispatch: active_runs failed");
            return;
        }
    };
    let active_count = active.len();
    let max = d.config.max_concurrent.max(1);

    let queued = match d.store.queued_runs() {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(error_category = "database", "dispatch: queued_runs failed");
            return;
        }
    };

    // The store orders by run id across every board and workspace. Reserve the
    // oldest available slots before launching; placement/completion may finish
    // out of order. A workspace is a container for distinct card tabs, not a
    // worker-lifetime exclusion key. The DB still enforces one open run/card.
    let claimed: Vec<_> = queued
        .into_iter()
        .take(max.saturating_sub(active_count))
        .collect();

    if claimed.is_empty() {
        return;
    }
    tracing::debug!(claimed = claimed.len(), active_count, max, "dispatch pass");

    let mut launches = tokio::task::JoinSet::new();
    for (run, card) in claimed {
        let daemon = Arc::clone(d);
        // Carry this pass's span into the spawned task; a bare `spawn` would
        // otherwise root each launch span on its own.
        let pass_span = tracing::Span::current();
        launches.spawn(
            async move {
                let run_id = run.id;
                (run_id, spawn_one(&daemon, &run, &card).await)
            }
            .instrument(pass_span),
        );
    }
    while let Some(result) = launches.join_next().await {
        match result {
            Ok((_, Ok(true) | Ok(false))) => {}
            Ok((run_id, Err(_))) => {
                tracing::error!(
                    run_id,
                    error_category = "launch",
                    "dispatch: spawn_one failed"
                );
            }
            Err(_) => tracing::error!(error_category = "task", "dispatch: launch task failed"),
        }
    }
}

/// Select placement for dispatch. v11 rows use the enqueue-time run snapshot;
/// pre-v11 rows explicitly retain the historical current-card behavior.
pub(crate) fn launch_session<'a>(run: &'a Run, card: &'a Card) -> Option<&'a str> {
    if run.launch_spec.is_some() {
        run.session.as_deref()
    } else {
        card.session.as_deref()
    }
}
