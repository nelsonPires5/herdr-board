//! Run lifecycle: enqueue, promote (spawn), and finalize (done / fail / timeout
//! / lost / cancel), plus the transition + auto-chain logic. All effects the
//! pure engine only *decides* are executed here.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::capability::{run_pane_name, run_pane_name_unique};
use board_core::db::{Db, EnqueueRun, FinalizeRun};
use board_core::engine::{
    decide_auto_hop, decide_resumability, decide_transition, validate_effective_settings,
    AutoHopDecision, ResumabilityDecision,
};
use board_core::harness::{
    build_invocation, is_builtin_harness, plan_session, HarnessError, SessionPlan,
};
use board_core::model::{Card, Run};
use board_core::prompt::{assemble_prompt, effective_settings};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome, SpaceKind};

struct PreparedEnqueue {
    card_id: i64,
    column_id: i64,
    harness: String,
    argv_json: String,
    prompt: String,
    system_prompt: String,
    launch_spec_json: String,
    session_id: Option<String>,
    session: Option<String>,
}

impl PreparedEnqueue {
    fn borrowed(&self) -> EnqueueRun<'_> {
        EnqueueRun {
            card_id: self.card_id,
            column_id: self.column_id,
            harness: &self.harness,
            argv_json: &self.argv_json,
            prompt_snapshot: &self.prompt,
            system_prompt_snapshot: Some(&self.system_prompt),
            launch_spec_json: Some(&self.launch_spec_json),
            session_id: self.session_id.as_deref(),
            session: self.session.as_deref(),
        }
    }
}
use crate::spawner::HerdrLaunchPlan;
use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use board_core::{Error, Result};
use board_herdr::{HerdrClient, NotificationSound, WorkspaceCreateParams, WorkspaceInfo};
use uuid::Uuid;

use crate::state::{ActiveRun, Daemon};
use board_core::model::SpaceKey;

const HERDR_PROTOCOL: u32 = 17;

fn map_harness_err(e: HarnessError) -> Error {
    match e {
        HarnessError::UnknownHarness(h) => Error::BadRequest(format!("unknown harness: {h}")),
        HarnessError::MissingMintedSession => {
            Error::BadRequest("mint session requested without a uuid".into())
        }
        HarnessError::MissingForkTargetSession => {
            Error::BadRequest("Pi fork requested without a new session uuid".into())
        }
        HarnessError::PiPermissionModeUnsupported => {
            Error::BadRequest("pi does not support permission modes".into())
        }
    }
}

/// Create a queued run row for `card` in `column`, minting/resuming/forking the
/// session per policy. Sets the card to `queued`. Does not spawn.
pub fn enqueue_run(d: &Arc<Daemon>, card_id: i64, column_id: i64, is_retry: bool) -> Result<Run> {
    enqueue_run_inner(d, card_id, column_id, is_retry)
}

fn enqueue_run_inner(d: &Arc<Daemon>, card_id: i64, column_id: i64, is_retry: bool) -> Result<Run> {
    // Scheduler state and every enqueue input share one critical section.
    // In particular, do not prepare an invocation from a card snapshot before
    // this lock: a concurrent edit could otherwise update `card.session` (or
    // its settings/prompt) before this run persists the stale value.
    let _sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    let card = db
        .get_card(card_id)?
        .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
    if card.archived_at.is_some() {
        return Err(Error::InvalidState(
            "archived card must be restored before starting a run".into(),
        ));
    }
    if db.open_run_for_card(card_id)?.is_some() {
        return Err(Error::InvalidState(
            "card has an open run; complete or cancel it before starting another".into(),
        ));
    }
    if card.column_id != column_id {
        return Err(Error::InvalidState(
            "card moved to another column while its run was being prepared".into(),
        ));
    }

    let column = db
        .get_column(column_id)?
        .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
    let comments = db.list_comments(card_id)?;
    // Resume/fork only sessions proven to exist on the harness side. The pure
    // engine owns the evidence rule; the daemon only supplies DB facts.
    let session_used = matches!(
        decide_resumability(
            card.session_id.as_deref(),
            &db.list_runs(card_id)?,
            &comments
        ),
        ResumabilityDecision::Resumable
    );
    // Revalidate the merged effective state at the enqueue boundary. This
    // protects dispatch from legacy rows and from a column/card changing after
    // an earlier client-side capability lookup.
    validate_effective_settings(&card, &column, &d.config)?;
    let settings = effective_settings(&card, &column)?;
    let prompt = assemble_prompt(&card.description, &comments);
    let existing_session = card.session_id.as_deref().filter(|_| session_used);
    let plan = plan_session(existing_session, settings.fresh_session, is_retry);
    // Pi needs an explicit new target id for both mint and fork. Claude ignores
    // the target on fork and keeps its existing resume+fork semantics.
    let target_session = matches!(plan, SessionPlan::Mint | SessionPlan::Fork(_))
        .then(|| Uuid::new_v4().to_string());
    let invocation = build_invocation(
        &settings.harness,
        &d.config,
        &settings,
        &plan,
        target_session.as_deref(),
        &prompt,
    )
    .map_err(map_harness_err)?;

    // Built-ins state their persisted session explicitly. Preserve legacy
    // custom-harness bookkeeping by falling back to the generic plan.
    let session_for_run = invocation
        .resulting_session_id
        .clone()
        .or_else(|| match &plan {
            SessionPlan::Mint => target_session.clone(),
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => Some(id.clone()),
        });
    let argv_json = serde_json::to_string(&invocation.argv)?;
    // Persist one authoritative, trailer-inclusive value for every new run.
    // Managed adapters already resolved this channel; configured harnesses use
    // the same protocol composition they historically received through env.
    let system_prompt_snapshot = invocation.system_prompt.clone().unwrap_or_else(|| {
        board_core::harness::protocol_system_prompt(settings.system_prompt.as_deref())
    });

    let run = db.enqueue_run_uow(&EnqueueRun {
        card_id,
        column_id,
        harness: &settings.harness,
        argv_json: &argv_json,
        prompt_snapshot: &prompt,
        system_prompt_snapshot: Some(&system_prompt_snapshot),
        launch_spec_json: Some(&serde_json::to_string(&RunLaunchSpec::v1(ExecutionSpec {
            argv: invocation.argv.clone(),
            env: invocation.env.clone(),
            agent_kind: invocation.agent_kind.clone(),
            initial_prompt: invocation.initial_prompt.clone(),
            system_prompt: invocation.system_prompt.clone(),
        }))?),
        session_id: session_for_run.as_deref(),
        session: card.session.as_deref(),
    })?;
    Ok(run)
}

fn prepare_enqueue_values(
    d: &Daemon,
    db: &Db,
    card: &Card,
    column_id: i64,
    is_retry: bool,
) -> Result<PreparedEnqueue> {
    let column = db
        .get_column(column_id)?
        .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
    let comments = db.list_comments(card.id)?;
    let session_used = matches!(
        decide_resumability(
            card.session_id.as_deref(),
            &db.list_runs(card.id)?,
            &comments
        ),
        ResumabilityDecision::Resumable
    );
    validate_effective_settings(card, &column, &d.config)?;
    let settings = effective_settings(card, &column)?;
    let prompt = assemble_prompt(&card.description, &comments);
    let existing_session = card.session_id.as_deref().filter(|_| session_used);
    let plan = plan_session(existing_session, settings.fresh_session, is_retry);
    let target_session = matches!(plan, SessionPlan::Mint | SessionPlan::Fork(_))
        .then(|| Uuid::new_v4().to_string());
    let invocation = build_invocation(
        &settings.harness,
        &d.config,
        &settings,
        &plan,
        target_session.as_deref(),
        &prompt,
    )
    .map_err(map_harness_err)?;
    let session_id = invocation
        .resulting_session_id
        .clone()
        .or_else(|| match &plan {
            SessionPlan::Mint => target_session.clone(),
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => Some(id.clone()),
        });
    Ok(PreparedEnqueue {
        card_id: card.id,
        column_id,
        harness: settings.harness.clone(),
        argv_json: serde_json::to_string(&invocation.argv)?,
        prompt,
        system_prompt: invocation.system_prompt.clone().unwrap_or_else(|| {
            board_core::harness::protocol_system_prompt(settings.system_prompt.as_deref())
        }),
        launch_spec_json: serde_json::to_string(&RunLaunchSpec::v1(ExecutionSpec {
            argv: invocation.argv.clone(),
            env: invocation.env.clone(),
            agent_kind: invocation.agent_kind.clone(),
            initial_prompt: invocation.initial_prompt.clone(),
            system_prompt: invocation.system_prompt.clone(),
        }))?,
        session_id,
        session: card.session.clone(),
    })
}

/// Evaluate the queue and promote as many queued runs as the per-space FIFO and
/// the global concurrency cap allow.
pub async fn dispatch_pass(d: &Arc<Daemon>) {
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
fn launch_session<'a>(run: &'a Run, card: &'a Card) -> Option<&'a str> {
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
                fail_queued_run(d, run, card, &format!("session resolve: {e:#}"))?;
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
                fail_queued_run(d, run, card, &format!("{e:#}"))?;
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
            fail_queued_run(d, run, card, &format!("spawn failed: {e:#}"))?;
            return Ok(false);
        }
        Err(e) => {
            fail_queued_run(d, run, card, &format!("spawn task panicked: {e}"))?;
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
fn register_spawned_run(
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
fn harness_prompt_env(
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
fn fail_queued_run(d: &Arc<Daemon>, run: &Run, card: &Card, reason: &str) -> Result<()> {
    let db = d.store.lock();
    db.finish_run(run.id, RunOutcome::Fail, Some(reason))?;
    db.add_comment(card.id, "system", reason)?;
    db.set_card_status(card.id, CardStatus::Failed)?;
    drop(db);
    d.emit_run_ended(card.id, run.id, RunOutcome::Fail);
    Ok(())
}

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
pub fn finalize_run(
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
pub fn finalize_run_timeout(
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

// ---------------------------------------------------------------------------
// Space resolution (per-session)
// ---------------------------------------------------------------------------

/// Resolve a card's space within its session to `(workspace_id, cwd)`.
///
/// - [`SpaceKind::Workspace`]: `space_ref` is an existing workspace id or a
///   case-insensitive label; cwd comes from the workspace's pane snapshot.
/// - [`SpaceKind::NewWorkspace`]: reuse an open workspace whose label matches
///   `space_ref`, else `workspace.create {label, cwd}`; in either case cwd is
///   verified from the resulting workspace's live pane snapshot.
fn resolve_space(
    client: &mut HerdrClient,
    kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
) -> anyhow::Result<(String, String)> {
    // Dispatch performs workspace discovery before handing off to the spawner,
    // so the selected socket must be gated here as well as in HerdrSpawner.
    client.require_protocol(HERDR_PROTOCOL).map_err(|error| {
        let message = error.to_string();
        anyhow::Error::new(error).context(format!(
            "checking Herdr protocol before workspace resolution: {message}"
        ))
    })?;
    let workspaces = client.workspace_list()?;
    match kind {
        SpaceKind::Workspace => {
            let ws_ref =
                space_ref.ok_or_else(|| anyhow::anyhow!("workspace space requires a space_ref"))?;
            let id = resolve_workspace_ref(&workspaces, ws_ref).map_err(|m| anyhow::anyhow!(m))?;
            let cwd = workspace_cwd(client, &id)?;
            Ok((id, cwd))
        }
        SpaceKind::NewWorkspace => {
            let label = space_ref.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                anyhow::anyhow!("new_workspace space requires a label (space_ref)")
            })?;
            let cwd = space_cwd
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("new_workspace space requires space_cwd"))?;
            match find_workspace_by_label(&workspaces, label) {
                // A reused workspace must use a cwd from one of its live
                // panes. Protocol 17 does not inherit workspace cwd, so the
                // card's original create cwd is not a safe fallback here.
                Some(id) => {
                    let live = workspace_cwd(client, &id)?;
                    Ok((id, live))
                }
                None => {
                    let created = client.workspace_create(&WorkspaceCreateParams {
                        label: Some(label.to_string()),
                        cwd: Some(cwd.to_string()),
                        focus: false,
                        ..Default::default()
                    })?;
                    let id = created.workspace_id().to_string();
                    let live = workspace_cwd(client, &id)?;
                    Ok((id, live))
                }
            }
        }
    }
}

/// Look up a workspace's cwd via one of its live panes in the session snapshot.
///
/// Protocol 17 placement is pane-first and never inherits a workspace cwd, so
/// failure to read this value must stop dispatch rather than launch from an
/// implicit daemon/Herdr fallback directory.
fn workspace_cwd(client: &mut HerdrClient, workspace_id: &str) -> anyhow::Result<String> {
    let snapshot = client.session_snapshot().map_err(|error| {
        // `anyhow::Error`'s Display shows only the outermost context. Include
        // the rendered cause in that context so a dispatch failure tells the
        // operator both which cwd lookup failed and why the snapshot failed,
        // while retaining the original error chain for callers using `{:#}`.
        let cause = format!("{error:#}");
        anyhow::Error::new(error).context(format!(
            "session snapshot unavailable while reading cwd for workspace '{workspace_id}': {cause}"
        ))
    })?;
    snapshot
        .panes
        .iter()
        .find(|pane| pane.workspace_id == workspace_id)
        .and_then(|pane| pane.cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("workspace '{workspace_id}' has no live pane cwd"))
}

/// Resolve a `workspace` space_ref (id, else case-insensitive label) to a
/// workspace id among the open `workspaces`. Err message lists the known ones.
fn resolve_workspace_ref(
    workspaces: &[WorkspaceInfo],
    ws_ref: &str,
) -> std::result::Result<String, String> {
    workspaces
        .iter()
        .find(|w| w.workspace_id == ws_ref)
        .or_else(|| {
            workspaces
                .iter()
                .find(|w| w.label.eq_ignore_ascii_case(ws_ref))
        })
        .map(|w| w.workspace_id.clone())
        .ok_or_else(|| {
            let known: Vec<String> = workspaces
                .iter()
                .map(|w| format!("{} ({})", w.workspace_id, w.label))
                .collect();
            format!(
                "herdr workspace '{ws_ref}' not found by id or label; known: {}",
                known.join(", ")
            )
        })
}

/// Find an open workspace whose label case-insensitively matches `label`.
fn find_workspace_by_label(workspaces: &[WorkspaceInfo], label: &str) -> Option<String> {
    workspaces
        .iter()
        .find(|w| w.label.eq_ignore_ascii_case(label))
        .map(|w| w.workspace_id.clone())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        dispatch_pass, enqueue_run, finalize_run, finalize_run_timeout, find_workspace_by_label,
        harness_prompt_env, launch_session, register_spawned_run, resolve_space,
        resolve_workspace_ref,
    };
    use crate::settings::DaemonSettings;
    use crate::spawner::{HerdrLaunchPlan, RuntimeHandle, Spawner};
    use crate::state::{ActiveRun, Daemon};
    use crate::store::Store;
    use board_core::config::Config;
    use board_core::db::{Db, EnqueueRun, LifecycleFaultPoint};
    use board_core::model::{Card, Run};
    use board_core::prompt::{assemble_prompt, effective_settings};
    use board_core::protocol::{
        AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, ColumnCreateParams,
        ColumnUpdateParams, Effort, Event, Patch, RunOutcome, SpaceKind, Trigger,
    };
    use board_core::{Error, Result};
    use board_herdr::{AgentStatus, HerdrClient, WorkspaceInfo};
    use serde_json::Value;
    use tokio::sync::{broadcast, mpsc, watch};

    struct MissingPiSpawner;

    impl Spawner for MissingPiSpawner {
        fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
            assert_eq!(req.argv.first().map(String::as_str), Some("pi"));
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "pi not found").into())
        }

        fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    #[derive(Default)]
    struct RecordingSpawner {
        kills: AtomicUsize,
        effects: Mutex<Option<Arc<Mutex<Vec<&'static str>>>>>,
    }

    #[derive(Default)]
    struct CapturingSpawner {
        requests: std::sync::Mutex<Vec<HerdrLaunchPlan>>,
    }

    #[derive(Default)]
    struct FaultPromotionSpawner {
        kills: AtomicUsize,
    }

    #[derive(Default)]
    struct PausedSpawner {
        state: Mutex<PausedSpawnerState>,
        changed: Condvar,
        started_notify: tokio::sync::Notify,
    }

    #[derive(Default)]
    struct PausedSpawnerState {
        started: Vec<String>,
        released: bool,
    }

    impl PausedSpawner {
        fn started(&self) -> Vec<String> {
            self.state.lock().unwrap().started.clone()
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.released = true;
            self.changed.notify_all();
        }
    }

    impl Spawner for PausedSpawner {
        fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
            let mut state = self.state.lock().unwrap();
            state.started.push(req.name.clone());
            self.started_notify.notify_one();
            self.changed.notify_all();
            while !state.released {
                state = self.changed.wait(state).unwrap();
            }
            Ok(RuntimeHandle {
                pid: Some(4242),
                ..Default::default()
            })
        }

        fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    impl Spawner for FaultPromotionSpawner {
        fn spawn(&self, _req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
            Ok(RuntimeHandle {
                pid: Some(4242),
                workspace_id: Some("spawned-workspace".into()),
                pane_id: Some("spawned-pane".into()),
                ..Default::default()
            })
        }

        fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    impl Spawner for CapturingSpawner {
        fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
            self.requests.lock().unwrap().push(req.clone());
            Ok(RuntimeHandle {
                pid: Some(4242),
                ..Default::default()
            })
        }

        fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    impl Spawner for RecordingSpawner {
        fn spawn(&self, _req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
            unreachable!("registration tests provide the spawned handle")
        }

        fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = self.effects.lock().unwrap().as_ref() {
                log.lock().unwrap().push("kill");
            }
            Ok(())
        }

        fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    fn test_daemon_with_receivers(
        spawner: Arc<dyn Spawner>,
    ) -> (
        Arc<Daemon>,
        broadcast::Receiver<Event>,
        mpsc::UnboundedReceiver<()>,
    ) {
        test_daemon_with_config(spawner, Config::default())
    }

    fn test_daemon_with_config(
        spawner: Arc<dyn Spawner>,
        config: Config,
    ) -> (
        Arc<Daemon>,
        broadcast::Receiver<Event>,
        mpsc::UnboundedReceiver<()>,
    ) {
        let (events_tx, events_rx) = broadcast::channel(16);
        let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let daemon = Arc::new(Daemon::new(
            Store::new(Db::open_in_memory().unwrap()),
            config,
            DaemonSettings::default(),
            PathBuf::from("/tmp/board-test.db"),
            PathBuf::from("/tmp/board-test.sock"),
            spawner,
            None,
            None,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ));
        (daemon, events_rx, dispatch_rx)
    }

    fn test_daemon(spawner: Arc<dyn Spawner>) -> Arc<Daemon> {
        test_daemon_with_receivers(spawner).0
    }

    fn ws(id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.to_string(),
            label: label.to_string(),
            number: 0,
            focused: false,
            active_tab_id: String::new(),
            agent_status: AgentStatus::Unknown,
        }
    }

    /// Serve exactly the three calls made by `resolve_space`: protocol gate,
    /// workspace discovery, and the live pane snapshot. Keeping the fixture
    /// single-purpose makes cwd failure tests deterministic and independent of
    /// a real Herdr process.
    fn workspace_resolution_server(snapshot: Option<Value>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("workspace-resolution.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming().take(3) {
                let Ok(stream) = incoming else { break };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                let response = match request["method"].as_str().unwrap() {
                    "ping" => serde_json::json!({
                        "id": request["id"],
                        "result": {
                            "type": "pong", "version": "0.7.5", "protocol": 17,
                            "capabilities": {}
                        }
                    }),
                    "workspace.list" => serde_json::json!({
                        "id": request["id"],
                        "result": {"workspaces": [{
                            "workspace_id": "w1", "label": "Feature", "number": 1,
                            "focused": false, "active_tab_id": "", "agent_status": "idle"
                        }]}
                    }),
                    "session.snapshot" => match &snapshot {
                        Some(snapshot) => serde_json::json!({
                            "id": request["id"],
                            "result": {"snapshot": snapshot}
                        }),
                        None => serde_json::json!({
                            "id": request["id"],
                            "error": {
                                "code": "snapshot_failed",
                                "message": "session snapshot unavailable"
                            }
                        }),
                    },
                    method => panic!("unexpected workspace resolution method: {method}"),
                };
                writeln!(writer, "{response}").unwrap();
                writer.flush().unwrap();
            }
        });
        (dir, socket)
    }

    /// Serve the four calls made while creating a missing `new_workspace`:
    /// protocol gate, workspace discovery, create, and live pane snapshot.
    fn new_workspace_resolution_server(snapshot: Option<Value>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("new-workspace-resolution.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming().take(4) {
                let Ok(stream) = incoming else { break };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                let response = match request["method"].as_str().unwrap() {
                    "ping" => serde_json::json!({
                        "id": request["id"],
                        "result": {
                            "type": "pong", "version": "0.7.5", "protocol": 17,
                            "capabilities": {}
                        }
                    }),
                    "workspace.list" => serde_json::json!({
                        "id": request["id"], "result": {"workspaces": []}
                    }),
                    "workspace.create" => serde_json::json!({
                        "id": request["id"],
                        "result": {
                            "type": "workspace_created",
                            "workspace": {
                                "workspace_id": "created-ws", "label": "Created", "number": 1,
                                "focused": false, "active_tab_id": "created-ws:t1",
                                "agent_status": "unknown"
                            },
                            "tab": {
                                "tab_id": "created-ws:t1", "workspace_id": "created-ws",
                                "label": "tab", "focused": false, "number": 1,
                                "pane_count": 1, "agent_status": "unknown"
                            },
                            "root_pane": {
                                "pane_id": "created-ws:p1", "terminal_id": "term-1",
                                "workspace_id": "created-ws", "tab_id": "created-ws:t1",
                                "focused": true, "revision": 0, "agent_status": "unknown"
                            }
                        }
                    }),
                    "session.snapshot" => match &snapshot {
                        Some(snapshot) => serde_json::json!({
                            "id": request["id"], "result": {"snapshot": snapshot}
                        }),
                        None => serde_json::json!({
                            "id": request["id"],
                            "error": {
                                "code": "snapshot_failed",
                                "message": "created workspace snapshot unavailable"
                            }
                        }),
                    },
                    method => panic!("unexpected new-workspace resolution method: {method}"),
                };
                writeln!(writer, "{response}").unwrap();
                writer.flush().unwrap();
            }
        });
        (dir, socket)
    }

    #[test]
    fn pi_is_builtin_and_does_not_receive_custom_prompt_env() {
        assert!(harness_prompt_env("pi", "prompt", Some("system")).is_empty());
        assert!(harness_prompt_env("claude", "prompt", Some("system")).is_empty());
        assert_eq!(
            harness_prompt_env("fake", "prompt", Some("system")),
            vec![
                ("BOARD_PROMPT".into(), "prompt".into()),
                (
                    "BOARD_SYSTEM_PROMPT".into(),
                    board_core::harness::protocol_system_prompt(Some("system")),
                ),
            ]
        );
        // No column prompt → the trailer alone, never a missing env var.
        assert_eq!(
            harness_prompt_env("fake", "prompt", None),
            vec![
                ("BOARD_PROMPT".into(), "prompt".into()),
                (
                    "BOARD_SYSTEM_PROMPT".into(),
                    board_core::harness::protocol_system_prompt(None),
                ),
            ]
        );
    }

    #[test]
    fn pi_fork_persists_the_new_target_session_id() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, column_id, old_session) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "retry".into(),
                    harness: Some("pi".into()),
                    effort: Some(Effort::Low),
                    ..Default::default()
                })
                .unwrap();
            let old_session = "11111111-1111-4111-8111-111111111111";
            db.set_card_session(card.id, old_session).unwrap();
            let prior = db
                .create_run(
                    card.id,
                    card.column_id,
                    "pi",
                    "[]",
                    "prior",
                    Some(old_session),
                    None,
                )
                .unwrap();
            db.start_run(prior.id, None, None).unwrap();
            db.add_comment(card.id, &format!("agent:{}", prior.id), "done")
                .unwrap();
            db.finish_run(prior.id, RunOutcome::Ok, None).unwrap();
            (card.id, card.column_id, old_session.to_string())
        };

        let run = enqueue_run(&d, card_id, column_id, true).unwrap();
        let card = d.store.lock().get_card(card_id).unwrap().unwrap();
        let new_session = card.session_id.unwrap();
        assert_ne!(new_session, old_session);
        assert_eq!(run.session_id.as_deref(), Some(new_session.as_str()));
        assert!(run.launch_spec.is_some());
        assert_eq!(
            run.launch_spec.as_ref().unwrap().execution().argv,
            serde_json::from_str::<Vec<String>>(&run.argv_json).unwrap()
        );
        let argv: Vec<String> = serde_json::from_str(&run.argv_json).unwrap();
        assert!(argv
            .windows(2)
            .any(|w| w == ["--fork", old_session.as_str()]));
        assert!(argv
            .windows(2)
            .any(|w| w == ["--session-id", new_session.as_str()]));
    }

    #[test]
    fn enqueue_run_final_guard_prevents_duplicate_open_runs() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, column_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "single open run".into(),
                    ..Default::default()
                })
                .unwrap();
            (card.id, card.column_id)
        };

        let first = enqueue_run(&d, card_id, column_id, true).unwrap();
        let err = enqueue_run(&d, card_id, column_id, true).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("open run"));
        let open_runs: Vec<_> = d
            .store
            .lock()
            .list_runs(card_id)
            .unwrap()
            .into_iter()
            .filter(|run| run.ended_at.is_none())
            .collect();
        assert_eq!(open_runs.len(), 1);
        assert_eq!(open_runs[0].id, first.id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_claims_a1_and_b1_before_launch_and_serializes_competing_passes() {
        let spawner = Arc::new(PausedSpawner::default());
        let config = Config {
            max_concurrent: 2,
            ..Default::default()
        };
        let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
        let (a1, a2, b1) = {
            let db = d.store.lock();
            let make = |title: &str, space_ref: &str| {
                db.create_card(&CardCreateParams {
                    title: title.into(),
                    space_kind: Some(SpaceKind::Workspace),
                    space_ref: Some(space_ref.into()),
                    ..Default::default()
                })
                .unwrap()
            };
            let a1 = make("A1", "space-a");
            let a2 = make("A2", "space-a");
            let b1 = make("B1", "space-b");
            for card in [&a1, &a2, &b1] {
                db.create_run(
                    card.id,
                    card.column_id,
                    "pi",
                    "[]",
                    card.title.as_str(),
                    None,
                    None,
                )
                .unwrap();
            }
            (a1, a2, b1)
        };

        // Deliberately race two callers. The per-daemon pass lock must keep the
        // second caller behind the first pass's pre-launch claims.
        let first = tokio::spawn({
            let d = d.clone();
            async move { dispatch_pass(&d).await }
        });
        let second = tokio::spawn({
            let d = d.clone();
            async move { dispatch_pass(&d).await }
        });

        while spawner.started().len() < 2 {
            spawner.started_notify.notified().await;
        }
        let started = spawner.started();
        assert_eq!(started.len(), 2, "global cap was exceeded: {started:?}");
        assert!(started
            .iter()
            .any(|name| name.starts_with(&format!("card-{}-", a1.id))));
        assert!(started
            .iter()
            .any(|name| name.starts_with(&format!("card-{}-", b1.id))));
        assert!(!started
            .iter()
            .any(|name| name.starts_with(&format!("card-{}-", a2.id))));

        spawner.release();
        first.await.unwrap();
        second.await.unwrap();

        let db = d.store.lock();
        let active_ids: Vec<_> = db
            .active_runs_with_cards()
            .unwrap()
            .into_iter()
            .map(|(_, card)| card.id)
            .collect();
        let queued_ids: Vec<_> = db
            .queued_runs_with_cards()
            .unwrap()
            .into_iter()
            .map(|(_, card)| card.id)
            .collect();
        assert_eq!(active_ids, vec![a1.id, b1.id]);
        assert_eq!(queued_ids, vec![a2.id]);
        assert_eq!(spawner.started().len(), 2);
    }

    #[tokio::test]
    async fn promotion_fault_reopens_queued_state_without_started_effects_and_kills_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promotion-fault.db");
        let armed = Arc::new(AtomicBool::new(false));
        let fault_armed = armed.clone();
        let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
            if fault_armed.load(Ordering::SeqCst)
                && point == LifecycleFaultPoint::PromoteAfterRunUpdate
            {
                return Err(Error::InvalidState("injected promotion fault".into()));
            }
            Ok(())
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "promotion fault".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: Some("system"),
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        let card_id = card.id;
        let run_id = run.id;
        let spawner = Arc::new(FaultPromotionSpawner::default());
        let (events_tx, mut events_rx) = broadcast::channel(16);
        let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let d = Arc::new(Daemon::new(
            Store::new(db),
            Config::default(),
            DaemonSettings::default(),
            path.clone(),
            dir.path().join("board.sock"),
            spawner.clone(),
            None,
            None,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ));
        armed.store(true, Ordering::SeqCst);

        dispatch_pass(&d).await;

        assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
        assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
        let watch = d.watch.lock().unwrap();
        assert!(watch.panes_by_socket.is_empty());
        assert_eq!(watch.generation, 0);
        drop(watch);
        assert!(matches!(
            events_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            dispatch_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        drop(d);
        let reopened = Db::open(&path).unwrap();
        let card = reopened.get_card(card_id).unwrap().unwrap();
        let run = reopened.get_run(run_id).unwrap();
        assert_eq!(card.status, CardStatus::Queued);
        assert!(run.started_at.is_none());
        assert!(run.herdr_workspace_id.is_none());
        assert!(run.herdr_pane_id.is_none());
    }

    #[test]
    fn spawned_run_registration_starts_row_card_and_active_bookkeeping_together() {
        let spawner = Arc::new(RecordingSpawner::default());
        let d = test_daemon(spawner.clone());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "register atomically".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                .unwrap();
            db.set_card_status(card.id, CardStatus::Queued).unwrap();
            (card.id, run.id)
        };
        let started = Instant::now();

        assert!(register_spawned_run(
            &d,
            run_id,
            RuntimeHandle {
                pid: Some(41),
                ..Default::default()
            },
            started,
            None,
            None,
        )
        .unwrap());

        let sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        assert!(db.get_run(run_id).unwrap().started_at.is_some());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
        assert_eq!(sched.active.get(&run_id).unwrap().handle.pid, Some(41));
        assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn spawned_run_registration_kills_handle_when_row_was_cancelled() {
        let spawner = Arc::new(RecordingSpawner::default());
        let d = test_daemon(spawner.clone());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "cancelled during spawn".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                .unwrap();
            db.finish_run(run.id, RunOutcome::Cancelled, Some("cancelled"))
                .unwrap();
            db.set_card_status(card.id, CardStatus::Failed).unwrap();
            (card.id, run.id)
        };

        assert!(!register_spawned_run(
            &d,
            run_id,
            RuntimeHandle {
                pid: Some(42),
                ..Default::default()
            },
            Instant::now(),
            None,
            None,
        )
        .unwrap());

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.started_at.is_none());
        assert_eq!(run.outcome, Some(RunOutcome::Cancelled));
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Failed
        );
        drop(db);
        assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
        assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn auto_transition_enqueues_once_inside_finalization_transaction() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, run_id, target_id) = {
            let db = d.store.lock();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "Source".into(),
                    trigger: Some(Trigger::Auto),
                    ..Default::default()
                })
                .unwrap();
            let target = db
                .create_column(&ColumnCreateParams {
                    name: "Target".into(),
                    trigger: Some(Trigger::Auto),
                    ..Default::default()
                })
                .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: source.id,
                on_success_column_id: Patch::Set(target.id),
                ..Default::default()
            })
            .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    column_id: Some(source.id),
                    title: "chain".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "pi", "[]", "p", None, None)
                .unwrap();
            db.start_run(run.id, None, None).unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
            (card.id, run.id, target.id)
        };

        let (_, card) = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap();

        assert_eq!(card.column_id, target_id);
        assert_eq!(card.status, CardStatus::Queued);
        let runs = d.store.lock().list_runs(card_id).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs.iter().filter(|run| run.ended_at.is_none()).count(), 1);
        let next = runs.iter().find(|run| run.ended_at.is_none()).unwrap();
        assert!(
            next.launch_spec.is_some(),
            "auto-hop must materialize exactly one v11 spec"
        );
        assert_eq!(next.session, card.session);
    }

    #[test]
    fn finalization_planning_error_preserves_exact_prior_state_and_emits_nothing() {
        let (d, mut events, mut dispatch) = test_daemon_with_receivers(Arc::new(MissingPiSpawner));
        let (card_id, run_id, target_id) = {
            let db = d.store.lock();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "Source".into(),
                    ..Default::default()
                })
                .unwrap();
            let target = db
                .create_column(&ColumnCreateParams {
                    name: "Target".into(),
                    trigger: Some(Trigger::Auto),
                    ..Default::default()
                })
                .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: source.id,
                on_success_column_id: Patch::Set(target.id),
                ..Default::default()
            })
            .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    column_id: Some(source.id),
                    title: "bad next harness".into(),
                    harness: Some("missing".into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "pi", "[]", "p", None, None)
                .unwrap();
            db.start_run(run.id, None, None).unwrap();
            db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                .unwrap();
            (card.id, run.id, target.id)
        };

        let err = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap_err();
        assert!(err.to_string().contains("unknown harness"));

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        let card = db.get_card(card_id).unwrap().unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.outcome, None);
        assert_ne!(card.column_id, target_id);
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
        assert_eq!(db.list_runs(card_id).unwrap().len(), 1);
        assert!(db.list_comments(card_id).unwrap().is_empty());
        drop(db);
        assert!(events.try_recv().is_err());
        assert!(dispatch.try_recv().is_err());
    }

    fn file_daemon(
        db: Db,
        path: PathBuf,
        spawner: Arc<dyn Spawner>,
    ) -> (
        Arc<Daemon>,
        broadcast::Receiver<Event>,
        mpsc::UnboundedReceiver<()>,
    ) {
        let (events_tx, events_rx) = broadcast::channel(32);
        let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let daemon = Arc::new(Daemon::new(
            Store::new(db),
            Config::default(),
            DaemonSettings::default(),
            path,
            PathBuf::from("/tmp/board-finalize-test.sock"),
            spawner,
            None,
            None,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ));
        (daemon, events_rx, dispatch_rx)
    }

    fn assert_no_effects(
        d: &Arc<Daemon>,
        events: &mut broadcast::Receiver<Event>,
        dispatch: &mut mpsc::UnboundedReceiver<()>,
        spawner: &RecordingSpawner,
        run_id: i64,
    ) {
        assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
        assert!(d.sched.lock().unwrap().active.contains_key(&run_id));
        assert!(
            events.try_recv().is_err(),
            "terminal event escaped rollback"
        );
        assert!(
            dispatch.try_recv().is_err(),
            "dispatch wake escaped rollback"
        );
    }

    #[test]
    fn daemon_comment_insert_fault_reopens_exact_prior_state_without_precommit_effects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comment-fault.db");
        let db = Db::open(&path).unwrap();
        let (card_id, run_id, column_id) = {
            let card = db
                .create_card(&CardCreateParams {
                    title: "comment rollback".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, card.column_id, "pi", "[]", "prompt", None, None)
                .unwrap();
            db.start_run(run.id, Some("workspace"), Some("pane"))
                .unwrap();
            db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                .unwrap();
            db.add_comment(card.id, "user", "durable before").unwrap();
            (card.id, run.id, card.column_id)
        };
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER abort_daemon_comment BEFORE INSERT ON comments
                 BEGIN SELECT RAISE(ABORT, 'injected daemon comment failure'); END;",
            )
            .unwrap();
        let spawner = Arc::new(RecordingSpawner::default());
        let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
        let effects = Arc::new(Mutex::new(Vec::new()));
        *d.effect_log.lock().unwrap() = Some(effects.clone());
        d.sched.lock().unwrap().active.insert(
            run_id,
            ActiveRun {
                card_id,
                handle: RuntimeHandle {
                    pane_id: Some("pane".into()),
                    ..Default::default()
                },
                started: Instant::now(),
                timeout_deadline: None,
                idle_since: None,
                awaiting_since: Some(Instant::now()),
                is_local: false,
                pane_id: Some("pane".into()),
            },
        );

        let err = finalize_run(
            &d,
            run_id,
            RunOutcome::Cancelled,
            Some("must roll back".into()),
            Some("must not persist".into()),
            true,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("injected daemon comment failure"));
        assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
        assert!(effects.lock().unwrap().is_empty());
        drop(d);

        let reopened = Db::open(&path).unwrap();
        let run = reopened.get_run(run_id).unwrap();
        let card = reopened.get_card(card_id).unwrap().unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.outcome, None);
        assert_eq!(run.result_summary, None);
        assert_eq!(card.column_id, column_id);
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
        let comments = reopened.list_comments(card_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "user");
        assert_eq!(comments[0].body, "durable before");
        assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
    }

    #[test]
    fn daemon_auto_hop_enqueue_fault_reopens_exact_prior_state_without_precommit_effects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto-hop-fault.db");
        let db = Db::open(&path).unwrap();
        let (card_id, run_id, source_id) = {
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "Fault source".into(),
                    ..Default::default()
                })
                .unwrap();
            let target = db
                .create_column(&ColumnCreateParams {
                    name: "Fault auto target".into(),
                    trigger: Some(Trigger::Auto),
                    ..Default::default()
                })
                .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: source.id,
                on_success_column_id: Patch::Set(target.id),
                ..Default::default()
            })
            .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "auto hop rollback".into(),
                    column_id: Some(source.id),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "pi", "[]", "prompt", None, None)
                .unwrap();
            db.start_run(run.id, Some("workspace"), Some("pane"))
                .unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
            db.add_comment(card.id, "user", "durable before").unwrap();
            (card.id, run.id, source.id)
        };
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER abort_daemon_next BEFORE INSERT ON runs
                 WHEN NEW.card_id={card_id}
                 BEGIN SELECT RAISE(ABORT, 'injected daemon next enqueue failure'); END;"
            ))
            .unwrap();
        let spawner = Arc::new(RecordingSpawner::default());
        let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
        let effects = Arc::new(Mutex::new(Vec::new()));
        *d.effect_log.lock().unwrap() = Some(effects.clone());
        d.sched.lock().unwrap().active.insert(
            run_id,
            ActiveRun {
                card_id,
                handle: RuntimeHandle {
                    pane_id: Some("pane".into()),
                    ..Default::default()
                },
                started: Instant::now(),
                timeout_deadline: None,
                idle_since: None,
                awaiting_since: None,
                is_local: false,
                pane_id: Some("pane".into()),
            },
        );

        let err = finalize_run(
            &d,
            run_id,
            RunOutcome::Ok,
            Some("must roll back".into()),
            Some("must not persist".into()),
            true,
            true,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("injected daemon next enqueue failure"));
        assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
        assert!(effects.lock().unwrap().is_empty());
        assert_eq!(d.sched.lock().unwrap().chain_hops.get(&card_id), None);
        drop(d);

        let reopened = Db::open(&path).unwrap();
        let run = reopened.get_run(run_id).unwrap();
        let card = reopened.get_card(card_id).unwrap().unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.outcome, None);
        assert_eq!(run.result_summary, None);
        assert_eq!(card.column_id, source_id);
        assert_eq!(card.status, CardStatus::Running);
        assert_eq!(card.awaiting_reason, None);
        let comments = reopened.list_comments(card_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "durable before");
        assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
    }

    #[derive(Clone, Copy, Debug)]
    enum TerminalPath {
        BoardDone,
        Cancel,
        Timeout,
        PaneExit,
    }

    fn invoke_terminal_path(
        d: &Arc<Daemon>,
        run_id: i64,
        path: TerminalPath,
    ) -> Result<(Run, Card)> {
        match path {
            TerminalPath::BoardDone => finalize_run(
                d,
                run_id,
                RunOutcome::Ok,
                Some("board done".into()),
                None,
                false,
                true,
            ),
            TerminalPath::Cancel => finalize_run(
                d,
                run_id,
                RunOutcome::Cancelled,
                Some("cancel".into()),
                None,
                true,
                false,
            ),
            TerminalPath::Timeout => finalize_run_timeout(
                d,
                run_id,
                Instant::now(),
                RunOutcome::Fail,
                Some("timeout".into()),
                Some("timeout".into()),
                true,
                true,
            )?
            .ok_or_else(|| Error::InvalidState("timeout lost".into())),
            TerminalPath::PaneExit => finalize_run(
                d,
                run_id,
                RunOutcome::Fail,
                Some("pane exit".into()),
                Some("pane exit".into()),
                false,
                false,
            ),
        }
    }

    #[test]
    fn terminal_winner_duplicate_and_stale_matrix_is_idempotent() {
        let paths = [
            TerminalPath::BoardDone,
            TerminalPath::Cancel,
            TerminalPath::Timeout,
            TerminalPath::PaneExit,
        ];
        for winner in paths {
            for loser in paths {
                let spawner = Arc::new(RecordingSpawner::default());
                let (d, mut events, mut dispatch) = test_daemon_with_receivers(spawner.clone());
                let (card_id, run_id) = {
                    let db = d.store.lock();
                    let card = db
                        .create_card(&CardCreateParams {
                            title: format!("winner {winner:?}, loser {loser:?}"),
                            ..Default::default()
                        })
                        .unwrap();
                    let run = db
                        .create_run(card.id, card.column_id, "pi", "[]", "prompt", None, None)
                        .unwrap();
                    db.start_run(run.id, Some("workspace"), Some("pane"))
                        .unwrap();
                    db.set_card_status(card.id, CardStatus::Running).unwrap();
                    (card.id, run.id)
                };
                d.sched.lock().unwrap().active.insert(
                    run_id,
                    ActiveRun {
                        card_id,
                        handle: RuntimeHandle {
                            pane_id: Some("pane".into()),
                            ..Default::default()
                        },
                        started: Instant::now(),
                        timeout_deadline: Some(Instant::now() - Duration::from_secs(1)),
                        idle_since: None,
                        awaiting_since: None,
                        is_local: false,
                        pane_id: Some("pane".into()),
                    },
                );

                let (won_run, won_card) = invoke_terminal_path(&d, run_id, winner).unwrap();
                let won_outcome = won_run.outcome;
                let won_status = won_card.status;
                let won_column = won_card.column_id;
                let won_comments = d.store.lock().list_comments(card_id).unwrap();
                while events.try_recv().is_ok() {}
                while dispatch.try_recv().is_ok() {}
                let kills = spawner.kills.load(Ordering::SeqCst);

                let duplicate = invoke_terminal_path(&d, run_id, loser).unwrap();
                assert_eq!(duplicate.0.outcome, won_outcome, "{winner:?} vs {loser:?}");
                assert_eq!(duplicate.1.status, won_status, "{winner:?} vs {loser:?}");
                assert!(events.try_recv().is_err());
                assert!(dispatch.try_recv().is_err());
                assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
                assert_eq!(d.store.lock().list_comments(card_id).unwrap(), won_comments);

                let replacement = enqueue_run(&d, card_id, won_column, true).unwrap();
                while events.try_recv().is_ok() {}
                while dispatch.try_recv().is_ok() {}
                let stale = invoke_terminal_path(&d, run_id, loser).unwrap();
                assert_eq!(
                    stale.0.outcome, won_outcome,
                    "stale {winner:?} vs {loser:?}"
                );
                assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
                assert!(events.try_recv().is_err());
                assert!(dispatch.try_recv().is_err());
                let db = d.store.lock();
                let replacement = db.get_run(replacement.id).unwrap();
                assert!(replacement.ended_at.is_none());
                assert_eq!(
                    db.get_card(card_id).unwrap().unwrap().status,
                    CardStatus::Queued
                );
                assert_eq!(db.list_comments(card_id).unwrap(), won_comments);
            }
        }
    }

    #[test]
    fn successful_finalization_records_exact_postcommit_effect_order() {
        let spawner = Arc::new(RecordingSpawner::default());
        let (d, _events, _dispatch) = test_daemon_with_receivers(spawner.clone());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "effect source".into(),
                    ..Default::default()
                })
                .unwrap();
            let review = db
                .create_column(&ColumnCreateParams {
                    name: "Review".into(),
                    trigger: Some(Trigger::Manual),
                    ..Default::default()
                })
                .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: source.id,
                on_success_column_id: Patch::Set(review.id),
                ..Default::default()
            })
            .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "ordered effects".into(),
                    column_id: Some(source.id),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, source.id, "pi", "[]", "prompt", None, None)
                .unwrap();
            db.start_run(run.id, Some("workspace"), Some("pane"))
                .unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
            (card.id, run.id)
        };
        d.sched.lock().unwrap().active.insert(
            run_id,
            ActiveRun {
                card_id,
                handle: RuntimeHandle {
                    pane_id: Some("pane".into()),
                    ..Default::default()
                },
                started: Instant::now(),
                timeout_deadline: None,
                idle_since: None,
                awaiting_since: None,
                is_local: false,
                pane_id: Some("pane".into()),
            },
        );
        let effects = Arc::new(Mutex::new(Vec::new()));
        *d.effect_log.lock().unwrap() = Some(effects.clone());
        *spawner.effects.lock().unwrap() = Some(effects.clone());

        finalize_run(&d, run_id, RunOutcome::Ok, None, None, true, true).unwrap();

        assert_eq!(
            *effects.lock().unwrap(),
            [
                "scheduler",
                "watch",
                "kill",
                "notification",
                "run_ended",
                "board_changed",
                "dispatch_wake"
            ]
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EnqueueSnapshotSpec {
        harness: String,
        model: Option<String>,
        effort: Option<Effort>,
        permission_mode: Option<String>,
        system_prompt: Option<String>,
        fresh_session: bool,
        prompt: String,
        session: Option<String>,
    }

    // Test-only seam for the authoritative-lock contract: production enqueue
    // must call the pure snapshot builders again from the locked state rather
    // than persist the values prepared before the lock.
    fn authoritative_enqueue_snapshot(
        card: &board_core::model::Card,
        column: &board_core::model::Column,
        comments: &[board_core::model::Comment],
    ) -> EnqueueSnapshotSpec {
        let settings = effective_settings(card, column).unwrap();
        EnqueueSnapshotSpec {
            harness: settings.harness,
            model: settings.model,
            effort: settings.effort,
            permission_mode: settings.permission_mode,
            system_prompt: settings.system_prompt,
            fresh_session: settings.fresh_session,
            prompt: assemble_prompt(&card.description, comments),
            session: card.session.clone(),
        }
    }

    #[test]
    fn enqueue_snapshot_spec_rebuilds_after_authoritative_card_changes() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, column_id) = {
            let db = d.store.lock();
            let column = db
                .create_column(&ColumnCreateParams {
                    name: "authoritative old".into(),
                    system_prompt: Some("old settings".into()),
                    model_override: Some("old-model".into()),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "authoritative snapshot".into(),
                    column_id: Some(column.id),
                    harness: Some("pi".into()),
                    description: Some("old prompt".into()),
                    session: Some("old-herdr-session".into()),
                    ..Default::default()
                })
                .unwrap();
            db.add_comment(card.id, "user", "old comment").unwrap();
            (card.id, column.id)
        };

        let prepared = {
            let db = d.store.lock();
            authoritative_enqueue_snapshot(
                &db.get_card(card_id).unwrap().unwrap(),
                &db.get_column(column_id).unwrap().unwrap(),
                &db.list_comments(card_id).unwrap(),
            )
        };

        {
            let db = d.store.lock();
            db.update_card(&CardUpdateParams {
                id: card_id,
                description: Some("new prompt".into()),
                model: Patch::Set("new-model".into()),
                session: Patch::Set("new-herdr-session".into()),
                ..Default::default()
            })
            .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: column_id,
                system_prompt: Patch::Set("new settings".into()),
                model_override: Patch::Set("new-column-model".into()),
                ..Default::default()
            })
            .unwrap();
            db.add_comment(card_id, "user", "new comment").unwrap();
        }

        let rebuilt = {
            let db = d.store.lock();
            authoritative_enqueue_snapshot(
                &db.get_card(card_id).unwrap().unwrap(),
                &db.get_column(column_id).unwrap().unwrap(),
                &db.list_comments(card_id).unwrap(),
            )
        };
        assert_ne!(prepared, rebuilt);
        assert_eq!(rebuilt.harness, "pi");
        assert_eq!(rebuilt.model.as_deref(), Some("new-column-model"));
        assert_eq!(rebuilt.system_prompt.as_deref(), Some("new settings"));
        assert_eq!(rebuilt.session.as_deref(), Some("new-herdr-session"));
        assert!(rebuilt.prompt.contains("new prompt"));
        assert!(rebuilt.prompt.contains("new comment"));
        assert!(!rebuilt.prompt.contains("old prompt"));
        // Existing comments remain part of the authoritative current list;
        // the new comment must not be dropped while rebuilding.
        assert!(rebuilt.prompt.contains("old comment"));
    }

    #[tokio::test]
    async fn queued_managed_pi_uses_enqueue_time_system_snapshot() {
        let spawner = Arc::new(CapturingSpawner::default());
        let d = test_daemon(spawner.clone());
        let (card_id, column_id) = {
            let db = d.store.lock();
            let column = db
                .create_column(&ColumnCreateParams {
                    name: "Execute".into(),
                    trigger: Some(Trigger::Auto),
                    system_prompt: Some("old column instructions".into()),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "snapshot dispatch".into(),
                    column_id: Some(column.id),
                    harness: Some("pi".into()),
                    description: Some("task body".into()),
                    ..Default::default()
                })
                .unwrap();
            (card.id, column.id)
        };
        let run = enqueue_run(&d, card_id, column_id, false).unwrap();
        let exact = run.launch_spec.as_ref().unwrap().execution().clone();
        let old = board_core::harness::protocol_system_prompt(Some("old column instructions"));
        d.store
            .lock()
            .update_card(&CardUpdateParams {
                id: card_id,
                description: Some("edited task must not launch".into()),
                model: Patch::Set("edited-model".into()),
                ..Default::default()
            })
            .unwrap();
        d.store
            .lock()
            .update_column(&ColumnUpdateParams {
                id: column_id,
                system_prompt: Patch::Set("new column instructions".into()),
                ..Default::default()
            })
            .unwrap();

        dispatch_pass(&d).await;

        let requests = spawner.requests.lock().unwrap();
        let req = &requests[0];
        assert_eq!(req.argv, exact.argv);
        assert_eq!(req.agent_kind, exact.agent_kind);
        assert_eq!(req.initial_prompt, exact.initial_prompt);
        assert_eq!(req.system_prompt, exact.system_prompt);
        assert_eq!(req.agent_kind.as_deref(), Some("pi"));
        assert_eq!(
            req.initial_prompt.as_deref(),
            Some(run.prompt_snapshot.as_str())
        );
        assert_eq!(req.system_prompt.as_deref(), Some(old.as_str()));
        assert!(req
            .argv
            .iter()
            .all(|arg| !arg.contains("old column instructions")));
        assert!(req.argv.iter().all(|arg| !arg.contains("task body")));
    }

    #[tokio::test]
    async fn queued_configured_harness_uses_enqueue_time_system_snapshot() {
        let spawner = Arc::new(CapturingSpawner::default());
        let mut d = test_daemon(spawner.clone());
        Arc::get_mut(&mut d).unwrap().config.harness.insert(
            "custom".into(),
            board_core::config::HarnessDef {
                argv: vec!["custom-agent".into()],
                ..Default::default()
            },
        );
        let (card_id, column_id) = {
            let db = d.store.lock();
            let column = db
                .create_column(&ColumnCreateParams {
                    name: "Configured".into(),
                    system_prompt: Some("configured old".into()),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: "configured snapshot".into(),
                    column_id: Some(column.id),
                    harness: Some("custom".into()),
                    description: Some("configured task".into()),
                    ..Default::default()
                })
                .unwrap();
            (card.id, column.id)
        };
        let run = enqueue_run(&d, card_id, column_id, false).unwrap();
        let exact = run.launch_spec.as_ref().unwrap().execution().clone();
        Arc::get_mut(&mut d)
            .unwrap()
            .config
            .harness
            .get_mut("custom")
            .unwrap()
            .argv = vec!["edited-agent-must-not-launch".into()];
        d.store
            .lock()
            .update_card(&CardUpdateParams {
                id: card_id,
                description: Some("edited configured task".into()),
                ..Default::default()
            })
            .unwrap();
        d.store
            .lock()
            .update_column(&ColumnUpdateParams {
                id: column_id,
                system_prompt: Patch::Set("configured new".into()),
                ..Default::default()
            })
            .unwrap();
        dispatch_pass(&d).await;
        let requests = spawner.requests.lock().unwrap();
        let req = &requests[0];
        assert_eq!(req.argv, exact.argv);
        assert_eq!(req.agent_kind, exact.agent_kind);
        assert_eq!(req.initial_prompt, exact.initial_prompt);
        assert_eq!(req.system_prompt, exact.system_prompt);
        assert_eq!(&req.env[..exact.env.len()], exact.env.as_slice());
        assert_eq!(req.env.len(), exact.env.len() + 4);
        let env = &req.env;
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "BOARD_SYSTEM_PROMPT")
                .unwrap()
                .1,
            board_core::harness::protocol_system_prompt(Some("configured old"))
        );
        assert_eq!(
            env.iter().find(|(k, _)| k == "BOARD_BIN").map(|(_, v)| v),
            Some(
                &std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    #[test]
    fn v11_placement_uses_run_session_while_legacy_uses_current_card_session() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let card = d
            .store
            .lock()
            .create_card(&CardCreateParams {
                title: "session snapshot".into(),
                session: Some("enqueue-session".into()),
                ..Default::default()
            })
            .unwrap();
        let mut run = enqueue_run(&d, card.id, card.column_id, false).unwrap();
        assert!(run.launch_spec.is_some());
        assert_eq!(run.session.as_deref(), Some("enqueue-session"));

        // Model a queued card edit in the dispatch snapshot: v11 ignores it.
        let mut edited_card = card;
        edited_card.session = Some("edited-session".into());
        assert_eq!(launch_session(&run, &edited_card), Some("enqueue-session"));

        // The same row shape without a v11 spec follows the documented legacy
        // adapter and therefore observes the current card session.
        run.launch_spec = None;
        assert_eq!(launch_session(&run, &edited_card), Some("edited-session"));
    }

    #[tokio::test]
    async fn v7_and_pre_v7_launch_adapters_remain_explicit() {
        let spawner = Arc::new(CapturingSpawner::default());
        let mut config = Config::default();
        config.harness.insert(
            "custom".into(),
            board_core::config::HarnessDef {
                argv: vec!["custom".into()],
                ..Default::default()
            },
        );
        let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
        let (v7_card, legacy_card, column_id) = {
            let db = d.store.lock();
            let column = db
                .create_column(&ColumnCreateParams {
                    name: "Adapters".into(),
                    system_prompt: Some("current".into()),
                    ..Default::default()
                })
                .unwrap();
            let v7 = db
                .create_card(&CardCreateParams {
                    title: "v7".into(),
                    column_id: Some(column.id),
                    harness: Some("custom".into()),
                    space_ref: Some("v7".into()),
                    ..Default::default()
                })
                .unwrap();
            let legacy = db
                .create_card(&CardCreateParams {
                    title: "legacy".into(),
                    column_id: Some(column.id),
                    harness: Some("custom".into()),
                    space_ref: Some("legacy".into()),
                    ..Default::default()
                })
                .unwrap();
            db.create_run_with_prompt_snapshots(
                v7.id,
                column.id,
                "custom",
                r#"["v7-command"]"#,
                "v7-prompt",
                Some("v7-system-exact"),
                None,
                None,
            )
            .unwrap();
            db.create_run(
                legacy.id,
                column.id,
                "custom",
                r#"["legacy-command"]"#,
                "legacy-prompt",
                None,
                None,
            )
            .unwrap();
            db.set_card_status(v7.id, CardStatus::Queued).unwrap();
            db.set_card_status(legacy.id, CardStatus::Queued).unwrap();
            (v7.id, legacy.id, column.id)
        };
        dispatch_pass(&d).await;
        let requests = spawner.requests.lock().unwrap();
        let v7 = requests.iter().find(|r| r.argv == ["v7-command"]).unwrap();
        assert!(v7
            .env
            .contains(&("BOARD_SYSTEM_PROMPT".into(), "v7-system-exact".into())));
        let legacy = requests
            .iter()
            .find(|r| r.argv == ["legacy-command"])
            .unwrap();
        assert!(legacy.env.contains(&(
            "BOARD_SYSTEM_PROMPT".into(),
            board_core::harness::protocol_system_prompt(Some("current"))
        )));
        assert_ne!(v7_card, legacy_card);
        assert!(column_id > 0);
    }

    #[tokio::test]
    async fn spawn_failure_for_missing_pi_marks_run_failed_with_system_comment() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, column_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "missing pi".into(),
                    ..Default::default()
                })
                .unwrap();
            (card.id, card.column_id)
        };
        let run = enqueue_run(&d, card_id, column_id, false).unwrap();

        dispatch_pass(&d).await;

        let db = d.store.lock();
        let finished = db.get_run(run.id).unwrap();
        assert_eq!(finished.outcome, Some(RunOutcome::Fail));
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Failed
        );
        assert!(db
            .list_comments(card_id)
            .unwrap()
            .iter()
            .any(|comment| comment.author == "system"
                && comment.body.contains("spawn failed")
                && comment.body.contains("pi not found")));
    }

    #[test]
    fn scoped_run_transition_uses_the_cards_board_columns() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card, run, target) = {
            let db = d.store.lock();
            let board = db.open_board("/scoped").unwrap();
            let auto = db
                .create_column(&ColumnCreateParams {
                    board_id: Some(board.id),
                    name: "Execute".into(),
                    trigger: Some(Trigger::Auto),
                    ..Default::default()
                })
                .unwrap();
            let done = db
                .create_column(&ColumnCreateParams {
                    board_id: Some(board.id),
                    name: "Done".into(),
                    ..Default::default()
                })
                .unwrap();
            db.update_column(&ColumnUpdateParams {
                id: auto.id,
                on_success_column_id: Patch::Set(done.id),
                ..Default::default()
            })
            .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    board_id: Some(board.id),
                    column_id: Some(auto.id),
                    title: "scoped transition".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, auto.id, "pi", "[]", "p", None, None)
                .unwrap();
            db.start_run(run.id, None, None).unwrap();
            (card, run, done)
        };

        let (_, moved) = finalize_run(&d, run.id, RunOutcome::Ok, None, None, false, true).unwrap();
        assert_eq!(moved.board_id, card.board_id);
        assert_eq!(moved.column_id, target.id);
    }

    #[test]
    fn resolve_ref_by_id_then_label() {
        let all = [ws("w1", "Alpha"), ws("w2", "Beta")];
        assert_eq!(resolve_workspace_ref(&all, "w2").unwrap(), "w2");
        // Case-insensitive label match.
        assert_eq!(resolve_workspace_ref(&all, "alpha").unwrap(), "w1");
    }

    #[test]
    fn resolve_ref_unknown_lists_known() {
        let all = [ws("w1", "Alpha")];
        let err = resolve_workspace_ref(&all, "ghost").unwrap_err();
        assert!(err.contains("ghost"));
        assert!(err.contains("w1"));
    }

    #[test]
    fn new_workspace_reuse_matches_label_case_insensitively() {
        let all = [ws("w1", "Alpha"), ws("w2", "MyFeature")];
        // Reuse: label already open → return its id (no create).
        assert_eq!(
            find_workspace_by_label(&all, "myfeature").as_deref(),
            Some("w2")
        );
    }

    #[test]
    fn new_workspace_create_when_absent() {
        let all = [ws("w1", "Alpha")];
        // Absent → None → dispatch will call workspace.create.
        assert!(find_workspace_by_label(&all, "brand-new").is_none());
    }

    #[test]
    fn existing_workspace_resolution_fails_when_snapshot_fails() {
        let (_dir, socket) = workspace_resolution_server(None);
        let mut client = HerdrClient::connect(&socket).unwrap();
        let err = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), None)
            .expect_err("a snapshot failure must prevent launch without a cwd");
        assert!(err.to_string().contains("session snapshot unavailable"));
    }

    #[test]
    fn workspace_resolution_fails_without_live_cwd_for_existing_and_reused_spaces() {
        let missing_cwd_snapshot = serde_json::json!({
            "panes": [{
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "focused": false,
                "revision": 1
            }]
        });

        for (kind, space_ref, space_cwd) in [
            (SpaceKind::Workspace, "w1", None),
            (SpaceKind::NewWorkspace, "Feature", Some("/fallback")),
        ] {
            let (_dir, socket) = workspace_resolution_server(Some(missing_cwd_snapshot.clone()));
            let mut client = HerdrClient::connect(&socket).unwrap();
            let err = resolve_space(&mut client, kind, Some(space_ref), space_cwd)
                .expect_err("a missing live pane cwd must not fall back or be omitted");
            assert!(err.to_string().contains("cwd"), "{err}");
        }
    }

    #[test]
    fn newly_created_workspace_requires_live_snapshot_cwd() {
        for snapshot in [
            None,
            Some(serde_json::json!({
                "panes": [{
                    "pane_id": "created-ws:p1",
                    "workspace_id": "created-ws",
                    "focused": false,
                    "revision": 1
                }]
            })),
        ] {
            let (_dir, socket) = new_workspace_resolution_server(snapshot);
            let mut client = HerdrClient::connect(&socket).unwrap();
            let err = resolve_space(
                &mut client,
                SpaceKind::NewWorkspace,
                Some("Created"),
                Some("/requested-but-unverified"),
            )
            .expect_err("a created workspace must prove its cwd from a live pane snapshot");
            assert!(err.to_string().contains("cwd") || err.to_string().contains("snapshot"));
        }
    }

    #[test]
    fn new_workspace_selected_socket_preflights_protocol_before_resolution() {
        // RED: dispatch must gate the selected socket before resolve_space. A
        // mismatched socket must receive exactly ping; workspace.list/create,
        // session.snapshot, and spawner placement must not be reached.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("selected-herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let methods = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&methods);
        thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let Ok(stream) = stream else { break };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                seen.lock()
                    .unwrap()
                    .push(request["method"].as_str().unwrap().into());
                let result = match request["method"].as_str().unwrap() {
                    "ping" => serde_json::json!({
                        "type": "pong", "version": "0.7.4", "protocol": 17,
                        "capabilities": {}
                    }),
                    "workspace.list" => serde_json::json!({
                        "workspaces": [{
                            "workspace_id": "w1", "label": "feature", "number": 1,
                            "focused": false, "active_tab_id": "", "agent_status": "idle"
                        }]
                    }),
                    "session.snapshot" => serde_json::json!({}),
                    other => panic!("unexpected mutating/placement method: {other}"),
                };
                writeln!(
                    writer,
                    "{}",
                    serde_json::json!({
                        "id": request["id"], "result": result
                    })
                )
                .unwrap();
                writer.flush().unwrap();
            }
        });

        let mut client = HerdrClient::connect(&socket).unwrap();
        let result = resolve_space(
            &mut client,
            SpaceKind::NewWorkspace,
            Some("feature"),
            Some("/tmp/feature"),
        );

        let actual_methods = methods.lock().unwrap().clone();
        assert_eq!(actual_methods, vec!["ping"]);
        let err = result.expect_err("protocol mismatch must stop workspace resolution");
        assert!(err
            .to_string()
            .contains("Herdr 0.7.5 with protocol 17 is required"));
    }
}
