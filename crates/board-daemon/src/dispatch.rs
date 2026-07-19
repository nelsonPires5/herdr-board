//! Run lifecycle: enqueue, promote (spawn), and finalize (done / fail / timeout
//! / lost / cancel), plus the transition + auto-chain logic. All effects the
//! pure engine only *decides* are executed here.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::capability::{run_pane_name, run_pane_name_unique};
use board_core::engine::{decide_transition, TransitionDecision};
use board_core::harness::{
    build_invocation, is_builtin_harness, plan_session, HarnessError, SessionPlan,
};
use board_core::model::{Card, Run};
use board_core::prompt::{assemble_prompt, effective_settings};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome, SpaceKind};
use board_core::spawn::SpawnReq;
use board_core::{Error, Result};
use board_herdr::{HerdrClient, NotificationSound, WorkspaceCreateParams, WorkspaceInfo};
use uuid::Uuid;

use crate::state::{ActiveRun, Daemon, MAX_AUTO_HOPS};
use crate::store::space_key_str;

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
    enqueue_run_inner(d, card_id, column_id, is_retry, EnqueueMode::Public)
}

#[derive(Clone, Copy)]
enum EnqueueMode {
    Public,
    /// Only the finalizer owning this private run-id token may enqueue while
    /// its card remains claimed.
    Finalization(i64),
}

fn enqueue_run_inner(
    d: &Arc<Daemon>,
    card_id: i64,
    column_id: i64,
    is_retry: bool,
    mode: EnqueueMode,
) -> Result<Run> {
    let (card, column, comments, session_used) = {
        let db = d.store.lock();
        let card = db
            .get_card(card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
        let column = db
            .get_column(column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
        let comments = db.list_comments(card_id)?;
        // Resume/fork only sessions PROVEN to exist on the harness side —
        // claude exits with "no conversation found" otherwise. A spawned pane
        // is not proof (claude may crash before creating the session); an
        // `agent:<run_id>` comment is: it can only be posted from inside a
        // live run (the skill mandates comment-before-done).
        let session_used = match &card.session_id {
            Some(sid) => db.list_runs(card_id)?.iter().any(|r| {
                r.started_at.is_some()
                    && r.session_id.as_deref() == Some(sid)
                    && comments
                        .iter()
                        .any(|c| c.author == format!("agent:{}", r.id))
            }),
            None => false,
        };
        (card, column, comments, session_used)
    };

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

    // Invocation preparation above deliberately runs without either lock.
    // Revalidate under scheduler -> store and keep both through the writes:
    // this is the authoritative open-run and finalization guard.
    let sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    let current_card = db
        .get_card(card_id)?
        .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
    if current_card.archived_at.is_some() {
        return Err(Error::InvalidState(
            "archived card must be restored before starting a run".into(),
        ));
    }
    match (mode, sched.finalizing_cards.get(&card_id)) {
        (EnqueueMode::Public, None) => {}
        (EnqueueMode::Finalization(run_id), Some(owner)) if run_id == *owner => {}
        (EnqueueMode::Public, Some(_)) => {
            return Err(Error::InvalidState(
                "card finalization is in progress; retry after it completes".into(),
            ));
        }
        (EnqueueMode::Finalization(_), _) => {
            return Err(Error::InvalidState(
                "internal enqueue lost its card finalization claim".into(),
            ));
        }
    }
    if db.open_run_for_card(card_id)?.is_some() {
        return Err(Error::InvalidState(
            "card has an open run; complete or cancel it before starting another".into(),
        ));
    }
    if current_card.column_id != column_id {
        return Err(Error::InvalidState(
            "card moved to another column while its run was being prepared".into(),
        ));
    }
    if let Some(session_id) = &session_for_run {
        if current_card.session_id.as_deref() != Some(session_id) {
            db.set_card_session(card_id, session_id)?;
        }
    }
    let run = db.create_run(
        card_id,
        column_id,
        &settings.harness,
        &argv_json,
        &prompt,
        session_for_run.as_deref(),
        card.session.as_deref(),
    )?;
    db.set_card_status(card_id, CardStatus::Queued)?;
    Ok(run)
}

/// Evaluate the queue and promote as many queued runs as the per-space FIFO and
/// the global concurrency cap allow.
pub async fn dispatch_pass(d: &Arc<Daemon>) {
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: active_runs failed: {e}");
            return;
        }
    };
    let mut busy: HashSet<String> = active.iter().map(|(_, c)| space_key_str(c)).collect();
    let mut active_count = active.len();
    let max = d.config.max_concurrent.max(1);

    let queued = match d.store.queued_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: queued_runs failed: {e}");
            return;
        }
    };

    for (run, card) in queued {
        if active_count >= max {
            break;
        }
        // The finalizer wakes dispatch after releasing its claim. Until then,
        // keep an internally enqueued next hop queued so its final status is
        // stable and no new agent starts inside the finalization window.
        if d.sched
            .lock()
            .unwrap()
            .finalizing_cards
            .contains_key(&card.id)
        {
            continue;
        }
        let key = space_key_str(&card);
        if busy.contains(&key) {
            continue;
        }
        match spawn_one(d, &run, &card).await {
            Ok(true) => {
                busy.insert(key);
                active_count += 1;
            }
            Ok(false) => {} // spawn failed; run finished, slot not taken
            Err(e) => tracing::error!("dispatch: spawn_one run {} failed: {e}", run.id),
        }
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

    // Reconstruct the harness env (BOARD_PROMPT/BOARD_SYSTEM_PROMPT for custom
    // harnesses) plus the daemon-injected BOARD_* vars.
    let mut env = harness_prompt_env(
        &run.harness,
        &run.prompt_snapshot,
        column.system_prompt.as_deref(),
    );
    env.push(("BOARD_CARD_ID".into(), card.id.to_string()));
    env.push(("BOARD_RUN_ID".into(), run.id.to_string()));
    env.push((
        "BOARD_SOCKET".into(),
        d.socket_path.to_string_lossy().into_owned(),
    ));

    let argv: Vec<String> = serde_json::from_str(&run.argv_json)?;
    let mut req = SpawnReq {
        // Stable, human-readable pane name `card-<id>-<column-slug>`. herdr
        // agent names are exclusive while a pane using one is open (and finished
        // panes stay open, visible, by design), so on collision the spawner
        // retries once with the run-scoped `name_fallback`.
        name: run_pane_name(card.id, &column.name),
        name_fallback: Some(run_pane_name_unique(card.id, &column.name, run.id)),
        // Both space kinds land in a `kanban` tab (find-or-create + grid layout).
        tab_label: Some("kanban".to_string()),
        cwd: None,
        workspace_ref: None,
        herdr_socket: None,
        env,
        argv,
    };

    // Resolve the card's herdr session to a concrete socket. `None` session →
    // the daemon's default socket. An unknown/stopped session fails the run
    // with a clear error listing the known sessions.
    if let Some(reg) = &d.session_registry {
        match reg.resolve(card.session.as_deref()) {
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
        let resolved =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(String, Option<String>)> {
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
                req.cwd = cwd.map(PathBuf::from);
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
    let deadline = column
        .timeout_minutes
        .map(|m| started + Duration::from_secs(m.max(0) as u64 * d.settings.timeout_unit_secs));
    if !register_spawned_run(d, run.id, handle, started, deadline)? {
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
    handle: board_core::spawn::SpawnHandle,
    started: Instant,
    timeout_deadline: Option<Instant>,
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
        db.start_run(
            run_id,
            spawned.workspace_id.as_deref(),
            spawned.pane_id.as_deref(),
        )?;
        db.set_card_status(card.id, CardStatus::Running)?;
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
/// their prompt/system instructions in argv and must not receive reconstruction.
/// The board-protocol trailer is unconditional: every custom-harness run gets
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

struct FinalizationClaim<'a> {
    d: &'a Daemon,
    card_id: i64,
    run_id: i64,
}

impl Drop for FinalizationClaim<'_> {
    fn drop(&mut self) {
        let mut sched = self.d.sched.lock().unwrap();
        if sched.finalizing_cards.get(&self.card_id) == Some(&self.run_id) {
            sched.finalizing_cards.remove(&self.card_id);
        }
    }
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
    // One claim order everywhere: scheduler, then store. Ending the DB row
    // while both are held means a removed run is already terminal before any
    // competing finalizer or signal can inspect it.
    let (removed, elapsed, finished) = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let existing = db.get_run(run_id)?;
        // An ended row can still be mid-transition. A duplicate finalizer must
        // neither return that intermediate card nor disturb the owner's claim.
        if sched.finalizing_cards.contains_key(&existing.card_id) {
            return Ok(None);
        }
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
        let finished = db.finish_run(run_id, outcome, summary.as_deref())?;
        let removed = sched.active.remove(&run_id);
        sched.finalizing_cards.insert(existing.card_id, run_id);
        (removed, elapsed, finished)
    };
    let claim = FinalizationClaim {
        d,
        card_id: finished.card_id,
        run_id,
    };

    if kill {
        if let Some(active) = &removed {
            if let Err(e) = d.spawner.kill(&active.handle) {
                tracing::warn!("kill run {run_id} failed: {e}");
            }
        }
    }
    d.refresh_watch();

    let card_id = finished.card_id;
    let completion = complete_post_close(
        d,
        card_id,
        finished.column_id,
        run_id,
        outcome,
        elapsed,
        extra_comment.as_deref(),
        transition,
    );

    match completion {
        Ok(card) => {
            drop(claim);
            d.emit_run_ended(card_id, run_id, outcome);
            d.wake_dispatch();
            Ok(Some((finished, card)))
        }
        Err(original) => {
            let recovery = recover_post_close_failure(d, card_id, run_id, &original);
            // The claim remains authoritative through the recovery writes.
            drop(claim);
            d.emit_run_ended(card_id, run_id, outcome);
            d.wake_dispatch();
            match recovery {
                Ok(()) => Err(original),
                Err(recovery_error) => Err(Error::InvalidState(format!(
                    "run {run_id} finalization failed after close: {original}; recovery incomplete: {recovery_error}"
                ))),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_post_close(
    d: &Arc<Daemon>,
    card_id: i64,
    column_id: i64,
    run_id: i64,
    outcome: RunOutcome,
    elapsed: Option<i64>,
    extra_comment: Option<&str>,
    transition: bool,
) -> Result<Card> {
    if let Some(comment) = extra_comment {
        d.store.lock().add_comment(card_id, "system", comment)?;
    }

    if transition {
        let (current_col, cols) = {
            let db = d.store.lock();
            let card = db
                .get_card(card_id)?
                .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
            let col = db
                .get_column(column_id)?
                .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
            let cols = db.list_columns(card.board_id)?;
            (col, cols)
        };
        let dec = decide_transition(&current_col, &cols, outcome, elapsed);
        d.store
            .lock()
            .add_comment(card_id, "system", &dec.system_comment)?;
        apply_transition(d, card_id, run_id, &dec, &cols)
    } else {
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        let status = match outcome {
            RunOutcome::Ok => CardStatus::Idle,
            _ => CardStatus::Failed,
        };
        d.store.lock().set_card_status(card_id, status)
    }
}

/// Best-effort repair for any failure after the run row was closed. Both writes
/// are attempted so the diagnostic survives even if the status write fails.
fn recover_post_close_failure(
    d: &Arc<Daemon>,
    card_id: i64,
    run_id: i64,
    failure: &Error,
) -> std::result::Result<(), String> {
    let mut sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    sched.chain_hops.remove(&card_id);
    let status = db.set_card_status(card_id, CardStatus::Failed);
    let message = format!(
        "run {run_id} finalization failed after the run was closed: {failure}; card recovered to failed"
    );
    let comment = db.add_comment(card_id, "system", &message);

    let mut errors = Vec::new();
    if let Err(e) = status {
        errors.push(format!("setting card failed: {e}"));
    }
    if let Err(e) = comment {
        errors.push(format!("adding recovery comment failed: {e}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Apply a transition decision: move the card, enqueue the next auto run (with
/// cycle protection), or notify on a manual landing.
fn apply_transition(
    d: &Arc<Daemon>,
    card_id: i64,
    finalizing_run_id: i64,
    dec: &TransitionDecision,
    cols: &[board_core::model::Column],
) -> Result<Card> {
    let Some(tid) = dec.target_column_id else {
        // Stay put; chain ends.
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        return d.store.lock().set_card_status(card_id, dec.new_status);
    };

    d.store.lock().set_card_column(card_id, tid)?;
    let target = cols.iter().find(|c| c.id == tid);

    if dec.enqueue {
        let hops = {
            let mut s = d.sched.lock().unwrap();
            let h = s.chain_hops.entry(card_id).or_insert(0);
            *h += 1;
            *h
        };
        if hops > MAX_AUTO_HOPS {
            d.sched.lock().unwrap().chain_hops.remove(&card_id);
            let msg = format!(
                "auto-chain limit ({MAX_AUTO_HOPS}) reached without human action; stopping"
            );
            d.store.lock().add_comment(card_id, "system", &msg)?;
            return d.store.lock().set_card_status(card_id, CardStatus::Failed);
        }
        enqueue_run_inner(
            d,
            card_id,
            tid,
            false,
            EnqueueMode::Finalization(finalizing_run_id),
        )?;
        d.store
            .lock()
            .get_card(card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {card_id}")))
    } else {
        // Manual (or non-auto) landing: chain ends; notify (auto-transition).
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        let card = d.store.lock().set_card_status(card_id, dec.new_status)?;
        if target
            .map(|t| t.trigger == board_core::protocol::Trigger::Manual)
            .unwrap_or(false)
        {
            d.notify(
                format!("Card #{card_id} ready for review"),
                target.map(|t| format!("Entered {}", t.name)),
                NotificationSound::Request,
            );
        }
        Ok(card)
    }
}

// ---------------------------------------------------------------------------
// Space resolution (per-session)
// ---------------------------------------------------------------------------

/// Resolve a card's space within its session to `(workspace_id, cwd)`.
///
/// - [`SpaceKind::Workspace`]: `space_ref` is an existing workspace id or a
///   case-insensitive label; cwd comes from the workspace's pane snapshot.
/// - [`SpaceKind::NewWorkspace`]: reuse an open workspace whose label matches
///   `space_ref`, else `workspace.create {label, cwd}`; cwd is `space_cwd`.
fn resolve_space(
    client: &mut HerdrClient,
    kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
) -> anyhow::Result<(String, Option<String>)> {
    let workspaces = client.workspace_list()?;
    match kind {
        SpaceKind::Workspace => {
            let ws_ref =
                space_ref.ok_or_else(|| anyhow::anyhow!("workspace space requires a space_ref"))?;
            let id = resolve_workspace_ref(&workspaces, ws_ref).map_err(|m| anyhow::anyhow!(m))?;
            let cwd = workspace_cwd(client, &id);
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
                // Reuse: prefer the workspace's live cwd, fall back to the card's.
                Some(id) => {
                    let live = workspace_cwd(client, &id);
                    Ok((id, live.or_else(|| Some(cwd.to_string()))))
                }
                None => {
                    let created = client.workspace_create(&WorkspaceCreateParams {
                        label: Some(label.to_string()),
                        cwd: Some(cwd.to_string()),
                        focus: false,
                        ..Default::default()
                    })?;
                    Ok((created.workspace_id().to_string(), Some(cwd.to_string())))
                }
            }
        }
    }
}

/// Look up a workspace's cwd via its first pane in the session snapshot.
fn workspace_cwd(client: &mut HerdrClient, workspace_id: &str) -> Option<String> {
    client.session_snapshot().ok().and_then(|s| {
        s.panes
            .iter()
            .find(|p| p.workspace_id == workspace_id)
            .and_then(|p| p.cwd.clone())
    })
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use super::{
        dispatch_pass, enqueue_run, finalize_run, find_workspace_by_label, harness_prompt_env,
        register_spawned_run, resolve_workspace_ref,
    };
    use crate::settings::DaemonSettings;
    use crate::state::Daemon;
    use crate::store::Store;
    use board_core::config::Config;
    use board_core::db::Db;
    use board_core::protocol::{
        AwaitingReason, BoardChangedReason, CardCreateParams, CardStatus, ColumnCreateParams,
        ColumnUpdateParams, Effort, Event, RunOutcome, Trigger,
    };
    use board_core::spawn::{SpawnHandle, SpawnReq, Spawner};
    use board_herdr::{AgentStatus, WorkspaceInfo};
    use tokio::sync::{broadcast, mpsc, watch};

    struct MissingPiSpawner;

    impl Spawner for MissingPiSpawner {
        fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle> {
            assert_eq!(req.argv.first().map(String::as_str), Some("pi"));
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "pi not found").into())
        }

        fn kill(&self, _h: &SpawnHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_alive(&self, _h: &SpawnHandle) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    #[derive(Default)]
    struct RecordingSpawner {
        kills: AtomicUsize,
    }

    impl Spawner for RecordingSpawner {
        fn spawn(&self, _req: &SpawnReq) -> anyhow::Result<SpawnHandle> {
            unreachable!("registration tests provide the spawned handle")
        }

        fn kill(&self, _h: &SpawnHandle) -> anyhow::Result<()> {
            self.kills.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn is_alive(&self, _h: &SpawnHandle) -> anyhow::Result<bool> {
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
        let (events_tx, events_rx) = broadcast::channel(16);
        let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let daemon = Arc::new(Daemon::new(
            Store::new(Db::open_in_memory().unwrap()),
            Config::default(),
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

    #[test]
    fn public_enqueue_rejects_a_card_claimed_for_finalization() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, column_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "finishing".into(),
                    ..Default::default()
                })
                .unwrap();
            (card.id, card.column_id)
        };
        d.sched.lock().unwrap().finalizing_cards.insert(card_id, 99);

        let err = enqueue_run(&d, card_id, column_id, true).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("finalization"));
        assert!(d.store.lock().list_runs(card_id).unwrap().is_empty());
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
            SpawnHandle {
                pid: Some(41),
                ..Default::default()
            },
            started,
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
            SpawnHandle {
                pid: Some(42),
                ..Default::default()
            },
            Instant::now(),
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
    fn auto_transition_enqueues_once_inside_claim_and_releases_it() {
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
                on_success_column_id: Some(target.id),
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
        assert!(!d
            .sched
            .lock()
            .unwrap()
            .finalizing_cards
            .contains_key(&card_id));
    }

    #[test]
    fn finalization_error_recovers_failed_card_and_emits_completion() {
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
                on_success_column_id: Some(target.id),
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
        assert!(run.ended_at.is_some());
        assert_eq!(run.outcome, Some(RunOutcome::Ok));
        assert_eq!(card.column_id, target_id, "partial move is retained");
        assert_eq!(card.status, CardStatus::Failed);
        assert_eq!(card.awaiting_reason, None);
        assert_eq!(db.list_runs(card_id).unwrap().len(), 1);
        assert!(db.list_comments(card_id).unwrap().iter().any(|comment| {
            comment.author == "system"
                && comment.body.contains("finalization failed")
                && comment.body.contains("unknown harness")
        }));
        drop(db);

        assert!(!d
            .sched
            .lock()
            .unwrap()
            .finalizing_cards
            .contains_key(&card_id));
        assert_eq!(
            events.try_recv().unwrap(),
            Event::RunEnded {
                card_id,
                run_id,
                outcome: RunOutcome::Ok,
            }
        );
        assert_eq!(
            events.try_recv().unwrap(),
            Event::BoardChanged {
                reason: BoardChangedReason::RunEnded,
                card_id: Some(card_id),
                column_id: None,
            }
        );

        let (duplicate_run, duplicate_card) =
            finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap();
        assert!(duplicate_run.ended_at.is_some());
        assert_eq!(duplicate_card.status, CardStatus::Failed);
        assert_eq!(duplicate_card.awaiting_reason, None);
        assert!(
            events.try_recv().is_err(),
            "a duplicate finalizer must not emit RunEnded again"
        );
        dispatch.try_recv().unwrap();
        assert!(dispatch.try_recv().is_err());
    }

    #[test]
    fn duplicate_finalizer_does_not_clear_an_existing_claim() {
        let d = test_daemon(Arc::new(MissingPiSpawner));
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "already claimed".into(),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
                .unwrap();
            db.start_run(run.id, None, None).unwrap();
            db.finish_run(run.id, RunOutcome::Ok, None).unwrap();
            (card.id, run.id)
        };
        d.sched
            .lock()
            .unwrap()
            .finalizing_cards
            .insert(card_id, run_id);

        assert!(finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).is_err());
        assert_eq!(
            d.sched.lock().unwrap().finalizing_cards.get(&card_id),
            Some(&run_id)
        );
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
                on_success_column_id: Some(done.id),
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
}
