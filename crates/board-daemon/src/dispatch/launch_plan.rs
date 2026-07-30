//! Materialize one queued run into a launch plan, spawn it, and register the
//! resulting handle. Everything between "the queue chose this run" and "the run
//! is durably promoted" lives here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::capability::{run_pane_name, run_pane_name_unique};
use board_core::db::FinalizeRun;
use board_core::harness::is_builtin_harness;
use board_core::model::{Card, Run};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome};
use board_core::{Error, Result};
use board_herdr::HerdrClient;

use crate::dispatch::ownership::{owned_pane_ids, reconstruct_owned_tab_id, OwnedPanes};
use crate::dispatch::pass::launch_session;
use crate::dispatch::space::resolve_space;
use crate::spawner::HerdrLaunchPlan;
use crate::state::{ActiveRun, Daemon};

/// The board variables a launched pane needs to talk back to the daemon, in the
/// order dispatch has always appended them.
///
/// `run_id` is `None` for a rescued pane: `BOARD_RUN_ID` is the actor
/// credential, and a rescue belongs to no run (see `rescue::rescue_board_env`
/// for the full reasoning). Every other variable is identical either way.
pub(crate) fn board_env(
    card_id: i64,
    run_id: Option<i64>,
    socket_path: &std::path::Path,
) -> Result<Vec<(String, String)>> {
    let mut env = vec![("BOARD_CARD_ID".to_string(), card_id.to_string())];
    if let Some(run_id) = run_id {
        env.push(("BOARD_RUN_ID".to_string(), run_id.to_string()));
    }
    env.push((
        "BOARD_SOCKET".to_string(),
        socket_path.to_string_lossy().into_owned(),
    ));
    env.push((
        "BOARD_BIN".to_string(),
        std::env::current_exe()?.to_string_lossy().into_owned(),
    ));
    Ok(env)
}

/// Promote one queued run to running. Returns `Ok(true)` if it started,
/// `Ok(false)` if the spawn failed (the run is finished `fail`).
///
/// The span makes a run's whole launch — session resolve, workspace resolve,
/// spawn, promotion — correlatable in the log. One span per launch, never in a
/// loop.
#[tracing::instrument(
    name = "launch",
    skip_all,
    fields(run_id = run.id, card_id = card.id, harness = %run.harness)
)]
pub(super) async fn spawn_one(d: &Arc<Daemon>, run: &Run, card: &Card) -> Result<bool> {
    let column = {
        let db = d.store.lock();
        db.require_column(run.column_id)?
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

    let mut argv: Vec<String> = serde_json::from_str(&run.argv_json)?;
    // v11 rows consume the single enqueue-time materialization. Older rows use
    // the v7+ snapshot adapter above, or the pre-v7 fallback.
    if let Some(spec) = &run.launch_spec {
        let execution = spec.execution();
        argv.clone_from(&execution.argv);
        env = execution.env.clone();
    }
    // Appended once, after the base env is final. The v11 branch above replaces
    // `env` wholesale, so pushing the board variables before it would only be
    // discarded again.
    env.extend(board_env(card.id, Some(run.id), &d.socket_path)?);
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
        // New durable runs get one exact tab per card. Legacy rows retain the
        // historical kanban placement and lookup behavior unchanged.
        tab_label: Some(if run.launch_spec.is_some() {
            format!("card-{}", card.id)
        } else {
            "kanban".to_string()
        }),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
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
        let prior_runs = if run.launch_spec.is_some() {
            d.store.lock().list_runs(card.id)?
        } else {
            Vec::new()
        };
        let card_tab = run.launch_spec.is_some();
        let run_session = launch_session.map(str::to_owned);
        let resolved = tokio::task::spawn_blocking(move || {
            let mut client = HerdrClient::connect(&socket)
                .map_err(|e| anyhow::anyhow!("herdr unavailable: {e}"))?;
            let (workspace_id, cwd) = resolve_space(
                &mut client,
                kind,
                space_ref.as_deref(),
                space_cwd.as_deref(),
            )?;
            let session = run_session.as_deref();
            let prior_pane_ids = owned_pane_ids(
                &prior_runs,
                session,
                &workspace_id,
                OwnedPanes::DurableChildren,
            );
            let reclaimable_pane_ids = owned_pane_ids(
                &prior_runs,
                session,
                &workspace_id,
                OwnedPanes::ReclaimableChildren,
            );
            let anchor_pane_ids =
                owned_pane_ids(&prior_runs, session, &workspace_id, OwnedPanes::Anchors);
            let mut ownership_proof = anchor_pane_ids.clone();
            ownership_proof.extend(prior_pane_ids.iter().cloned());
            let owned_tab_id = if card_tab && !ownership_proof.is_empty() {
                let snapshot = client
                    .session_snapshot()
                    .map_err(|e| anyhow::anyhow!("herdr session.snapshot: {e}"))?;
                reconstruct_owned_tab_id(&snapshot, &workspace_id, &ownership_proof)
            } else {
                None
            };
            Ok::<_, anyhow::Error>((
                workspace_id,
                cwd,
                owned_tab_id,
                prior_pane_ids,
                reclaimable_pane_ids,
                anchor_pane_ids,
            ))
        })
        .await
        .map_err(|e| Error::BadRequest(format!("workspace resolve join: {e}")))?;
        match resolved {
            Ok((
                id,
                cwd,
                owned_tab_id,
                durable_pane_ids,
                reclaimable_pane_ids,
                durable_anchor_pane_ids,
            )) => {
                req.workspace_ref = Some(id);
                req.cwd = Some(PathBuf::from(cwd));
                req.owned_tab_id = owned_tab_id;
                req.durable_pane_ids = durable_pane_ids;
                req.reclaimable_pane_ids = reclaimable_pane_ids;
                req.durable_anchor_pane_ids = durable_anchor_pane_ids;
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
            // The discriminant is observability only: a transport deadline and
            // a permanent "workspace not found" are logged apart, but both
            // still end the run here, exactly as before. Changing that is a
            // separate, e2e-pinned decision.
            tracing::warn!(
                run_id = run.id,
                card_id = card.id,
                failure = e.label(),
                retriable = e.retriable(),
                "spawn failed"
            );
            // `:#` prints the whole anyhow chain — the herdr protocol error
            // (e.g. "workspace not found") lives below the top context.
            fail_queued_run(d, run.id, &format!("spawn failed: {e:#}"))?;
            return Ok(false);
        }
        Err(e) => {
            tracing::error!(
                run_id = run.id,
                card_id = card.id,
                error_category = "task",
                "spawn task panicked"
            );
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
    tracing::info!(
        run_id = run.id,
        card_id = card.id,
        column_id = run.column_id,
        timeout_ms,
        "run started"
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
        db.promote_run_with_anchor_uow(
            run_id,
            spawned.workspace_id.as_deref(),
            spawned.pane_id.as_deref(),
            spawned.anchor_pane_id.as_deref(),
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
                if d.spawner.kill(unregistered).is_err() {
                    tracing::warn!(
                        run_id,
                        error_category = "runtime",
                        "kill unregistered spawned run failed"
                    );
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
