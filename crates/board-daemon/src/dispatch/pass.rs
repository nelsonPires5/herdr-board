use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::capability::{run_pane_name, run_pane_name_unique};
use board_core::db::FinalizeRun;
use board_core::harness::is_builtin_harness;
use board_core::model::{Card, Run, SpaceKey};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome};
use board_herdr::HerdrClient;

use crate::dispatch::space::resolve_space;
use crate::spawner::HerdrLaunchPlan;
use crate::state::{ActiveRun, Daemon};
use board_core::{Error, Result};

/// Evaluate the queue and promote as many queued runs as the per-space FIFO and
/// the global concurrency cap allow.
pub(crate) async fn dispatch_pass(d: &Arc<Daemon>) {
    // A claim lives in this pass until spawn registration/failure is durable.
    // Serializing passes prevents another caller from observing those claimed
    // rows as queued and independently claiming the same capacity or space.
    let _pass = d.dispatch_pass.lock().await;
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: active_runs failed: {e}");
            return;
        }
    };
    let mut busy: HashSet<SpaceKey> = active
        .iter()
        .map(|(_, card)| SpaceKey::from_card(card))
        .collect();
    let mut active_count = active.len();
    let max = d.config.max_concurrent.max(1);

    let queued = match d.store.queued_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: queued_runs failed: {e}");
            return;
        }
    };

    // Claim capacity and one FIFO head per space before any launch starts.
    // Independent spaces then launch concurrently; a second run for a claimed
    // space cannot slip in while its first launch is in flight.
    let mut claimed = Vec::new();
    for (run, card) in queued {
        if active_count >= max {
            break;
        }
        let key = SpaceKey::from_card(&card);
        if busy.insert(key) {
            active_count += 1;
            claimed.push((run, card));
        }
    }

    let mut launches = tokio::task::JoinSet::new();
    for (run, card) in claimed {
        let daemon = Arc::clone(d);
        launches.spawn(async move {
            let run_id = run.id;
            (run_id, spawn_one(&daemon, &run, &card).await)
        });
    }
    while let Some(result) = launches.join_next().await {
        match result {
            Ok((_, Ok(true) | Ok(false))) => {}
            Ok((run_id, Err(error))) => {
                tracing::error!("dispatch: spawn_one run {run_id} failed: {error}");
            }
            Err(error) => tracing::error!("dispatch: launch task failed: {error}"),
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

/// Promote one queued run to running. Returns `Ok(true)` if it started,
/// `Ok(false)` if the spawn failed (the run is finished `fail`).
async fn spawn_one(d: &Arc<Daemon>, run: &Run, card: &Card) -> Result<bool> {
    let column = {
        let db = d.store.lock();
        db.get_column(run.column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", run.column_id)))?
    };

    // A non-NULL snapshot is explicit protocol-17 launch metadata. Legacy v6
    // built-ins remain unmanaged so their persisted all-in-one argv executes
    // unchanged, without duplicate prompt delivery.
    let builtin = is_builtin_harness(&run.harness);
    let managed = builtin && run.system_prompt_snapshot.is_some();
    let agent_kind = managed.then(|| run.harness.clone());
    let initial_prompt = managed.then(|| run.prompt_snapshot.clone());
    let system_prompt = if managed {
        run.system_prompt_snapshot.clone()
    } else {
        None
    };
    let mut env = if builtin {
        Vec::new()
    } else if let Some(snapshot) = &run.system_prompt_snapshot {
        // New configured runs use the exact enqueue-time value. In particular,
        // do not append the protocol trailer a second time here.
        vec![
            ("BOARD_PROMPT".to_string(), run.prompt_snapshot.clone()),
            ("BOARD_SYSTEM_PROMPT".to_string(), snapshot.clone()),
        ]
    } else {
        // Pre-v7 configured rows never persisted this channel; retain their
        // historical spawn-time current-column fallback.
        harness_prompt_env(
            &run.harness,
            &run.prompt_snapshot,
            column.system_prompt.as_deref(),
        )
    };
    env.push(("BOARD_CARD_ID".into(), card.id.to_string()));
    env.push(("BOARD_RUN_ID".into(), run.id.to_string()));
    env.push((
        "BOARD_SOCKET".into(),
        d.socket_path.to_string_lossy().into_owned(),
    ));
    env.push((
        "BOARD_BIN".into(),
        std::env::current_exe()?.to_string_lossy().into_owned(),
    ));

    let mut argv: Vec<String> = serde_json::from_str(&run.argv_json)?;
    // v11 rows consume the single enqueue-time materialization. Older rows use
    // the v7+ snapshot adapter above, or the pre-v7 fallback.
    if let Some(spec) = &run.launch_spec {
        let execution = spec.execution();
        argv.clone_from(&execution.argv);
        env = execution.env.clone();
        env.push(("BOARD_CARD_ID".into(), card.id.to_string()));
        env.push(("BOARD_RUN_ID".into(), run.id.to_string()));
        env.push((
            "BOARD_SOCKET".into(),
            d.socket_path.to_string_lossy().into_owned(),
        ));
        env.push((
            "BOARD_BIN".into(),
            std::env::current_exe()?.to_string_lossy().into_owned(),
        ));
    }
    let (agent_kind, initial_prompt, system_prompt) = match run.launch_spec.as_ref() {
        Some(spec) => {
            let execution = spec.execution();
            (
                execution.agent_kind.clone(),
                execution.initial_prompt.clone(),
                execution.system_prompt.clone(),
            )
        }
        None => (agent_kind, initial_prompt, system_prompt),
    };
    let mut req = HerdrLaunchPlan {
        // Stable, human-readable pane name `card-<id>-<column-slug>`. herdr
        // agent names are exclusive while a pane using one is open (and finished
        // panes stay open, visible, by design), so on collision the spawner
        // retries once with the run-scoped `name_fallback`.
        name: run_pane_name(card.id, &column.name),
        agent_kind,
        initial_prompt,
        system_prompt,
        name_fallback: Some(run_pane_name_unique(card.id, &column.name, run.id)),
        // Both space kinds land in a `kanban` tab (find-or-create + grid layout).
        tab_label: Some("kanban".to_string()),
        cwd: None,
        workspace_ref: None,
        herdr_socket: None,
        env,
        argv,
    };

    // v11 launch placement is part of the enqueue-time run snapshot. Legacy
    // rows have no launch spec and retain their historical current-card lookup.
    let launch_session = launch_session(run, card);
    // Resolve that herdr session to a concrete socket. `None` session → the
    // daemon's default socket. An unknown/stopped session fails the run.
    if let Some(reg) = &d.session_registry {
        match reg.resolve(launch_session) {
            Ok(resolved) => {
                // Only stamp a non-default socket on the req (keeps the default
                // path implicit, matching the spawner's fallback).
                if resolved.socket.as_path() != reg.default_socket() {
                    req.herdr_socket = Some(resolved.socket);
                }
            }
            Err(e) => {
                fail_queued_run(d, run.id, &format!("session resolve: {e:#}"))?;
                return Ok(false);
            }
        }
    }

    // Resolve the workspace (existing or freshly created) within the card's
    // session, plus its cwd — agent.start does not inherit the latter (the
    // daemon is not a pane, so herdr's "follow" policy resolves to the daemon's
    // own context). Skipped entirely under the local spawner (no session_registry).
    if d.session_registry.is_some() {
        let socket = req
            .herdr_socket
            .clone()
            .unwrap_or_else(|| d.default_herdr_socket());
        let kind = card.space_kind;
        let space_ref = card.space_ref.clone();
        let space_cwd = card.space_cwd.clone();
        let resolved = tokio::task::spawn_blocking(move || -> anyhow::Result<(String, String)> {
            let mut client = HerdrClient::connect(&socket)
                .map_err(|e| anyhow::anyhow!("herdr unavailable: {e}"))?;
            resolve_space(
                &mut client,
                kind,
                space_ref.as_deref(),
                space_cwd.as_deref(),
            )
        })
        .await
        .map_err(|e| Error::BadRequest(format!("workspace resolve join: {e}")))?;
        match resolved {
            Ok((id, cwd)) => {
                req.workspace_ref = Some(id);
                req.cwd = Some(PathBuf::from(cwd));
            }
            Err(e) => {
                fail_queued_run(d, run.id, &format!("{e:#}"))?;
                return Ok(false);
            }
        }
    }

    let spawner = d.spawner.clone();
    let req2 = req.clone();
    let spawn_res = tokio::task::spawn_blocking(move || spawner.spawn(&req2)).await;
    let handle = match spawn_res {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            // `:#` prints the whole anyhow chain — the herdr protocol error
            // (e.g. "workspace not found") lives below the top context.
            fail_queued_run(d, run.id, &format!("spawn failed: {e:#}"))?;
            return Ok(false);
        }
        Err(e) => {
            fail_queued_run(d, run.id, &format!("spawn task panicked: {e}"))?;
            return Ok(false);
        }
    };

    let started = Instant::now();
    let timeout_ms = column.timeout_minutes.map(|m| {
        m.max(0)
            .saturating_mul(d.settings.timeout_unit_secs as i64)
            .saturating_mul(1000)
    });
    let deadline = timeout_ms.and_then(|ms| started.checked_add(Duration::from_millis(ms as u64)));
    let deadline_at_ms = timeout_ms.map(|ms| d.wall_now_ms().saturating_add(ms));
    if !register_spawned_run(d, run.id, handle, started, deadline, deadline_at_ms)? {
        return Ok(false);
    }

    d.refresh_watch();
    d.emit_changed(
        BoardChangedReason::RunStarted,
        Some(card.id),
        Some(run.column_id),
    );
    Ok(true)
}

/// Register a handle only while its queued row is still open. Cancellation can
/// close a run while the blocking spawn is in flight, so the DB promotion and
/// in-memory bookkeeping share the scheduler -> store critical section.
pub(crate) fn register_spawned_run(
    d: &Arc<Daemon>,
    run_id: i64,
    handle: crate::spawner::RuntimeHandle,
    started: Instant,
    timeout_deadline: Option<Instant>,
    timeout_deadline_at_ms: Option<i64>,
) -> Result<bool> {
    let mut handle = Some(handle);
    let registration = (|| {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let run = db.get_run(run_id)?;
        let card = db.get_card(run.card_id)?;
        if run.ended_at.is_some() || run.started_at.is_some() {
            return Ok(false);
        }
        let card = card.ok_or_else(|| Error::NotFound(format!("card {}", run.card_id)))?;
        let spawned = handle.as_ref().ok_or_else(|| {
            Error::InvalidState(format!(
                "run {run_id} registration lost its spawn handle before promotion"
            ))
        })?;
        let is_local = spawned.pid.is_some();
        let pane_id = spawned.pane_id.clone();
        db.promote_run_uow(
            run_id,
            spawned.workspace_id.as_deref(),
            spawned.pane_id.as_deref(),
            timeout_deadline_at_ms,
        )?;
        let registered_handle = handle.take().ok_or_else(|| {
            Error::InvalidState(format!(
                "run {run_id} registration lost its spawn handle before bookkeeping"
            ))
        })?;
        sched.active.insert(
            run_id,
            ActiveRun {
                card_id: card.id,
                handle: registered_handle,
                started,
                timeout_deadline,
                idle_since: None,
                awaiting_since: None,
                is_local,
                pane_id,
            },
        );
        Ok(true)
    })();

    match registration {
        Ok(true) => Ok(true),
        other => {
            if let Some(unregistered) = handle.as_ref() {
                if let Err(e) = d.spawner.kill(unregistered) {
                    tracing::warn!("kill unregistered spawned run {run_id} failed: {e}");
                }
            }
            other
        }
    }
}

/// Prompt env is only for config-defined harness templates. Built-ins carry
/// their prompt/system instructions in explicit managed launch fields and must
/// not receive reconstruction. The board-protocol trailer is unconditional:
/// every custom-harness run gets
/// BOARD_SYSTEM_PROMPT even when the column sets no system prompt.
pub(crate) fn harness_prompt_env(
    harness: &str,
    prompt: &str,
    system_prompt: Option<&str>,
) -> Vec<(String, String)> {
    if is_builtin_harness(harness) {
        return Vec::new();
    }
    vec![
        ("BOARD_PROMPT".to_string(), prompt.to_string()),
        (
            "BOARD_SYSTEM_PROMPT".to_string(),
            board_core::harness::protocol_system_prompt(system_prompt),
        ),
    ]
}

/// Finish a never-started (queued) run as `fail` after a spawn error.
///
/// Uses the canonical finalization UOW path: scheduler→store locking, atomic
/// [`Db::finalize_run_uow`] commit, and all effects deferred until after commit.
fn fail_queued_run(d: &Arc<Daemon>, run_id: i64, reason: &str) -> Result<()> {
    let effects = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.finalize_run_uow(&FinalizeRun {
            run_id,
            outcome: RunOutcome::Fail,
            summary: Some(reason),
            comments: &[("system", reason)],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })?
    };
    // Post-commit effects: wake watchers, emit events, and re-evaluate the
    // dispatch queue — all only after the atomic commit succeeds.
    d.refresh_watch();
    d.emit_run_ended(effects.card.id, run_id, RunOutcome::Fail);
    d.wake_dispatch();
    Ok(())
}
