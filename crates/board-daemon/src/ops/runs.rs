use super::*;
use std::path::{Path, PathBuf};

use board_core::capability;
use board_core::db::FinalizeRun;
use board_core::engine::{
    decide_lifecycle, LifecycleAction, LifecycleDecision, LifecycleFacts, LifecycleHarness,
    LifecycleRejection,
};
use board_core::harness::{self, is_builtin_harness};
use board_core::model::Run;

use crate::dispatch::{enqueue_run, finalize_run};
use crate::spawner::{rescue_run_pane, CardOwnership, RescueOutcome, RescuePlan};

fn lifecycle_facts(
    run: &Run,
    card: &board_core::model::Card,
    supplied_run_id: Option<i64>,
) -> LifecycleFacts {
    LifecycleFacts {
        open_run_id: Some(run.id),
        supplied_run_id,
        started: run.started_at.is_some(),
        harness: if is_builtin_harness(&run.harness) {
            LifecycleHarness::BuiltIn
        } else {
            LifecycleHarness::Configured
        },
        card_status: card.status,
    }
}

fn lifecycle_rejection(card_id: i64, rejection: LifecycleRejection) -> Error {
    match rejection {
        LifecycleRejection::NoOpenRun
        | LifecycleRejection::QueuedCompletionRequiresRunId
        | LifecycleRejection::QueuedBuiltinCompletion => {
            Error::NotFound(format!("no active run for card {card_id}"))
        }
        LifecycleRejection::SuppliedRunIdMismatch { expected, supplied } => Error::InvalidState(
            format!(
                "no active run for card {card_id}: run {supplied} does not match active run {expected}"
            ),
        ),
        LifecycleRejection::PaneExitRequiresRunId => Error::InvalidState(format!(
            "pane-exited callback for card {card_id} must supply a run id"
        )),
        LifecycleRejection::PaneExitBuiltin => Error::InvalidState(
            "pane-exited callback is only valid for configured harnesses".into(),
        ),
        LifecycleRejection::TimeoutBeforeStart => {
            Error::InvalidState(format!("run for card {card_id} has not started"))
        }
        LifecycleRejection::TimeoutPaused => {
            Error::InvalidState(format!("run for card {card_id} is awaiting review"))
        }
    }
}

pub(super) fn run_done(d: &Arc<Daemon>, p: RunDoneParams) -> Result<Value> {
    let (run, plan) = {
        // Keep the scheduler -> store lock order used by the normal active-run
        // path so eligibility and finalization serialize. The pure engine owns callback
        // eligibility, including the narrow queued configured-run exception.
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let run = db
            .open_run_for_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("no active run for card {}", p.card_id)))?;
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        let facts = lifecycle_facts(&run, &card, p.run_id);
        let decision = decide_lifecycle(&facts, LifecycleAction::Done { outcome: p.outcome });
        let LifecycleDecision::Finalize(plan) = decision else {
            let LifecycleDecision::Reject(rejection) = decision else {
                unreachable!("lifecycle decision matched twice")
            };
            return Err(lifecycle_rejection(p.card_id, rejection));
        };
        (run, plan)
    };
    let (run, card) = finalize_run(
        d,
        run.id,
        plan.outcome,
        p.summary,
        None,
        plan.kill,
        plan.transition,
    )?;
    Ok(json!(RunActionResult { run, card }))
}

pub(super) fn run_pane_exited(d: &Arc<Daemon>, p: RunPaneExitedParams) -> Result<Value> {
    let (run, plan) = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let open = db
            .open_run_for_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("no open run for card {}", p.card_id)))?;
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        let facts = lifecycle_facts(&open, &card, Some(p.run_id));
        let decision = decide_lifecycle(&facts, LifecycleAction::PaneExited);
        let LifecycleDecision::Finalize(plan) = decision else {
            let LifecycleDecision::Reject(rejection) = decision else {
                unreachable!("lifecycle decision matched twice")
            };
            return Err(lifecycle_rejection(p.card_id, rejection));
        };
        (open, plan)
    };

    let (run, card) = finalize_run(
        d,
        run.id,
        plan.outcome,
        Some("configured harness exited without calling board done".into()),
        Some("pane exited without board done".into()),
        plan.kill,
        plan.transition,
    )?;
    Ok(json!(RunActionResult { run, card }))
}

pub(super) fn run_cancel(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    // Prefer the active run; else cancel the latest queued run for the card.
    let active = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.active_run_for_card(p.card_id)?
    };
    if let Some(run) = active {
        let (run, card) = finalize_run(
            d,
            run.id,
            RunOutcome::Cancelled,
            Some("cancelled by user".into()),
            None,
            true,
            false,
        )?;
        return Ok(json!(RunActionResult { run, card }));
    }

    // Keep queued verification and finalization in one scheduler→store critical
    // section. Otherwise dispatch could promote the run between them, leaving
    // a newly spawned process alive when this no-kill cancellation wins.
    let effects = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let queued = db
            .open_run_for_card(p.card_id)?
            .filter(|run| run.started_at.is_none())
            .ok_or_else(|| {
                Error::NotFound(format!("no active or queued run for card {}", p.card_id))
            })?;
        let effects = db.finalize_run_uow(&FinalizeRun {
            run_id: queued.id,
            outcome: RunOutcome::Cancelled,
            summary: Some("cancelled by user"),
            comments: &[("system", "queued run cancelled by user")],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })?;
        sched.chain_hops.remove(&p.card_id);
        #[cfg(test)]
        d.record_effect("scheduler");
        effects
    };
    d.refresh_watch();
    d.emit_run_ended(p.card_id, effects.finished_run.id, RunOutcome::Cancelled);
    d.wake_dispatch();
    Ok(json!(RunActionResult {
        run: effects.finished_run,
        card: effects.card,
    }))
}

pub(super) fn run_focus(d: &Arc<Daemon>, p: RunFocusParams) -> Result<Value> {
    // The caller names the exact run; ownership is validated in the db layer.
    let run = d.store.lock().run_for_card(p.card_id, p.run_id)?;
    // Dead-end #1: no recorded pane at all (the run never reached a pane, or
    // predates pane recording). The rescue below covers it exactly like a pane
    // that has since disappeared — in both cases there is no live pane for this
    // run and the only non-destructive option is to resume its conversation.
    let recorded_pane_id = run.herdr_pane_id.clone();
    // When nothing is recorded, a rescue is the *only* possible outcome, so
    // validate it locally (run row + config only) before involving Herdr. That
    // keeps "this run can never be reopened" reportable even with Herdr down,
    // and preserves the pre-rescue error code for that dead end.
    let identity = match &recorded_pane_id {
        None => Some(rescue_identity(d, &run, None)?),
        Some(_) => None,
    };
    let registry = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("jump to pane requires Herdr".into()))?;
    let target_socket = match run.session.as_deref() {
        None => registry.default_socket().to_path_buf(),
        Some(session) => {
            registry
                .resolve(Some(session))
                .map_err(|e| Error::HerdrUnavailable(format!("resolving run session: {e:#}")))?
                .socket
        }
    };
    let origin_socket = normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let target_socket = normalize_socket(&target_socket, "target")?;
    if origin_socket != target_socket {
        return Err(Error::InvalidState(
            "run pane belongs to a different Herdr session; cross-session jump is not supported"
                .into(),
        ));
    }

    // Liveness before focus: one targeted `pane.get` on this exact pane id
    // (cheaper and more direct than pulling a whole `session.snapshot` to test
    // one membership) turns a stale `herdr_pane_id` into a rescue instead of an
    // opaque `pane.focus` failure. There is deliberately never a fallback to
    // another run's pane.
    let live_pane = match &recorded_pane_id {
        None => None,
        Some(pane_id) => {
            let mut client = board_herdr::HerdrClient::connect(&target_socket)
                .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
            client
                .pane_get(pane_id)
                .map_err(|e| Error::HerdrUnavailable(format!("pane.get {pane_id}: {e}")))?
                .map(|_| pane_id.clone())
        }
    };

    if let Some(pane_id) = live_pane {
        let mut client = board_herdr::HerdrClient::connect(&target_socket)
            .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
        client
            .pane_focus(&pane_id)
            .map_err(|e| Error::HerdrUnavailable(format!("pane.focus {pane_id}: {e}")))?;
        return Ok(json!(RunFocusResult {
            action: RunFocusAction::FocusedRecordedPane,
            recorded_pane_id,
            run_id: run.id,
            card_id: run.card_id,
            column_id: run.column_id,
            harness: run.harness,
            session: run.session,
            session_id: run.session_id,
            pane_id,
        }));
    }

    // Dead-end #2 (and #1): there is no live pane for this run. Rescue it by
    // resuming its harness conversation in a brand-new, ephemeral pane. Nothing
    // is written to the database — see `rescue_run`.
    let identity = match identity {
        Some(identity) => identity,
        None => rescue_identity(d, &run, recorded_pane_id.as_deref())?,
    };
    let (action, pane_id) = rescue_run(d, &run, identity, &target_socket)?;
    Ok(json!(RunFocusResult {
        action,
        recorded_pane_id,
        run_id: run.id,
        card_id: run.card_id,
        column_id: run.column_id,
        harness: run.harness,
        session: run.session,
        session_id: run.session_id,
        pane_id,
    }))
}

/// Everything a rescue needs that comes from the run row + config alone, with
/// no Herdr involvement. Resolving this first means a run that can never be
/// reopened says so even when Herdr is unreachable.
struct RescueIdentity {
    /// The resume launch derived from the run's persisted execution spec.
    execution: board_core::launch::ExecutionSpec,
    /// The Herdr workspace the run actually ran in.
    workspace_id: String,
}

/// Validate that this run *can* be reopened, and build its resume launch.
///
/// Ordered cheapest-first, and every branch fails closed: there is deliberately
/// no fallback that launches a *fresh* conversation, because that would silently
/// re-run the card's task.
fn rescue_identity(
    d: &Arc<Daemon>,
    run: &board_core::model::Run,
    recorded_pane_id: Option<&str>,
) -> Result<RescueIdentity> {
    // How to describe the dead end in every message below.
    let pane_state = match recorded_pane_id {
        Some(pane_id) => format!("its pane {pane_id} no longer exists"),
        None => "it has no pane recorded".to_string(),
    };

    // 1. Identity from the run row. `session_id` is the *harness conversation*
    //    id (`--resume`), never `run.session` (the herdr session name).
    //    Nothing recorded to focus and nothing recorded to resume ⇒ NotFound,
    //    the same code this dead end reported before the rescue existed.
    let session_id = run
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            Error::NotFound(format!(
                "run {} of card {}: {pane_state}, and it recorded no harness conversation id, \
                 so there is nothing to reopen; retry the card to start a new run instead",
                run.id, run.card_id
            ))
        })?
        .to_string();

    // 2. Explicit per-harness resume capability. Naming the harness makes this
    //    actionable; there is no fallback launch.
    let support = capability::resume_support_for(&run.harness, &d.config);
    if !support.is_supported() {
        return Err(Error::InvalidState(format!(
            "run {} of card {}: {pane_state}, and harness '{}' does not support resuming a \
             recorded conversation, so it cannot be reopened; retry the card to start a new \
             run instead",
            run.id, run.card_id, run.harness
        )));
    }

    // 3. The launch spec persisted at enqueue time is the authority. Deriving
    //    the rescue from it (rather than rebuilding from current config)
    //    continues the same work in the same execution environment, so a
    //    rescue can never silently switch model/effort/env after a config edit.
    let launch_spec = run.launch_spec.as_ref().ok_or_else(|| {
        Error::NotFound(format!(
            "run {} of card {}: {pane_state}, and it predates durable launch specs, so there \
             is no recorded execution to resume; retry the card to start a new run instead",
            run.id, run.card_id
        ))
    })?;
    let mut execution =
        harness::resume_invocation(&run.harness, support, launch_spec.execution(), &session_id)
            .map_err(crate::dispatch::map_harness_err)?;
    rescue_board_env(d, run, &mut execution)?;

    // 4. The workspace the run actually ran in. A rescue never picks another.
    let workspace_id = run.herdr_workspace_id.clone().ok_or_else(|| {
        Error::HerdrUnavailable(format!(
            "run {} of card {}: {pane_state}, and it recorded no Herdr workspace, so a \
             reopened pane would have nowhere to go",
            run.id, run.card_id
        ))
    })?;

    Ok(RescueIdentity {
        execution,
        workspace_id,
    })
}

/// Install the board environment a rescued pane needs, and deliberately withhold
/// the one it must not have.
///
/// Dispatch injects `BOARD_CARD_ID`/`BOARD_RUN_ID`/`BOARD_SOCKET`/`BOARD_BIN` at
/// spawn time (`dispatch::pass`); none of them live in the persisted launch spec
/// (a built-in's persisted `env` is empty), and protocol-17 `agent.start` carries
/// no environment of its own. Without this the rescued pane would receive only
/// the two rescue variables, and any harness that reads the board env — the
/// checked-in fixtures do, under `set -u` — would exit immediately.
///
/// **`BOARD_RUN_ID` is deliberately absent.** It is not documentation, it is the
/// *actor credential*: `board comment` authenticates as `agent:$BOARD_RUN_ID`,
/// `board done` forwards it as the run to finalize, and the configured-harness
/// wrapper passes it to `run.pane_exited`. A rescued pane belongs to no run, and
/// the historical run row must stay immutable, so handing it the closed run's id
/// would be wrong in both directions:
/// - for an already-ended run, `require_agent_run` rejects it anyway
///   ("agent run N is no longer open"), so `board comment` would simply *fail*;
/// - for the narrow case of a still-open run whose pane died, it would let an
///   unwatched, unowned pane finalize that run — racing the liveness watcher for
///   the right to write the run's outcome.
///
/// Omitting it fails closed instead: `board comment` degrades to an ordinary
/// human comment on the card (still useful, and the card is the durable place for
/// a rescued conversation to report), `board done` answers "no active run" or is
/// rejected on run-id mismatch, and the configured wrapper's `__pane-exited` call
/// fails argument parsing and is swallowed by its own `|| :`. The run id is still
/// carried, for humans and fixtures, as `BOARD_RESCUED_RUN_ID` — a plain label
/// that no board command consumes.
fn rescue_board_env(
    d: &Arc<Daemon>,
    run: &board_core::model::Run,
    execution: &mut board_core::launch::ExecutionSpec,
) -> Result<()> {
    let board_bin = std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned();
    // The resume spec already carries BOARD_RESCUE + BOARD_RESUME_SESSION_ID.
    let injected = [
        ("BOARD_CARD_ID", run.card_id.to_string()),
        ("BOARD_SOCKET", d.socket_path.to_string_lossy().into_owned()),
        ("BOARD_BIN", board_bin),
        ("BOARD_RESCUED_RUN_ID", run.id.to_string()),
    ];
    for (key, value) in injected {
        // The persisted spec never sets these, but never shadow it silently.
        execution.env.retain(|(existing, _)| existing != key);
        execution.env.push((key.to_string(), value));
    }
    // Belt and braces: the actor credential must not survive from any source.
    execution.env.retain(|(key, _)| key != "BOARD_RUN_ID");
    Ok(())
}

/// Reopen a run whose pane is gone by resuming its harness conversation in a
/// fresh pane, or by focusing a pane an earlier rescue already created.
///
/// **This function performs no database writes and must never start doing so.**
/// It only *reads* the run rows it needs; the historical `runs` row for the
/// rescued run stays byte-for-byte untouched (no new row, no `herdr_pane_id`
/// update, no cleared `ended_at`/`outcome`). Consequently the rescued pane is
/// unmanaged: no ownership row, no watcher, no timeout. That is a deliberate,
/// documented limitation (`docs/design.md`).
fn rescue_run(
    d: &Arc<Daemon>,
    run: &board_core::model::Run,
    identity: RescueIdentity,
    target_socket: &Path,
) -> Result<(RunFocusAction, String)> {
    let RescueIdentity {
        execution,
        workspace_id,
    } = identity;

    // 5. Ownership evidence (read-only) plus the dedup marker.
    let prior_runs = d.store.lock().list_runs(run.card_id)?;
    // The ONLY correlator available for a previous rescue: no DB row may be
    // written, so the pane's Herdr-side name has to carry the identity. It must
    // therefore depend on nothing but **stable** identity — card id and run id.
    // Deliberately NOT `run_pane_name_unique`, whose `card-<id>-<column-slug>`
    // form is a function of the column's *current* name: renaming the column (or
    // tripping the 24-char slug cap) would change the marker, the scan would miss
    // the pane it created moments earlier, and `o` would resume the same
    // conversation a second time. The `-rescue` suffix keeps it from ever
    // colliding with a live original run pane. Treat it as a diagnostic hint, not
    // a record — see `spawner::rescue::find_rescued_pane`.
    let marker_name = format!("card-{}-r{}-rescue", run.card_id, run.id);
    let tab_label = format!("card-{}", run.card_id);
    let durable_pane_ids =
        crate::dispatch::durable_owned_pane_ids(&prior_runs, run.session.as_deref(), &workspace_id);
    let durable_anchor_pane_ids = crate::dispatch::durable_owned_anchor_pane_ids(
        &prior_runs,
        run.session.as_deref(),
        &workspace_id,
    );

    // 6. Reconstruct the exact card tab from durable pane identity (labels are
    //    never ownership), then place + launch. Everything Herdr-side lives in
    //    `spawner::rescue`, which shares the placement/launch helpers with
    //    dispatch instead of duplicating them — and never calls a UoW.
    let owned_tab_id = {
        let mut ownership_proof = durable_anchor_pane_ids.clone();
        ownership_proof.extend(durable_pane_ids.iter().cloned());
        if ownership_proof.is_empty() {
            None
        } else {
            let mut client = board_herdr::HerdrClient::connect(target_socket)
                .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
            let snapshot = client.session_snapshot().map_err(|e| {
                Error::HerdrUnavailable(format!("session.snapshot before rescue: {e}"))
            })?;
            crate::dispatch::reconstruct_owned_tab_id(&snapshot, &workspace_id, &ownership_proof)
        }
    };

    let plan = RescuePlan {
        marker_name: &marker_name,
        tab_label: &tab_label,
        workspace_id: &workspace_id,
        socket: target_socket,
        ownership: CardOwnership {
            owned_tab_id: owned_tab_id.as_deref(),
            durable_pane_ids: &durable_pane_ids,
            reclaimable_pane_ids: &[],
            durable_anchor_pane_ids: &durable_anchor_pane_ids,
            remembered_anchor_id: None,
        },
        execution,
        // Serialize against dispatch and against another concurrent rescue of
        // this same card tab, and register whatever gets allocated.
        card_tabs: d.spawner.card_tabs(),
    };
    match rescue_run_pane(&plan) {
        Ok(RescueOutcome::AlreadyLive(pane_id)) => {
            Ok((RunFocusAction::FocusedRescuedPane, pane_id))
        }
        Ok(RescueOutcome::Created(pane_id)) => Ok((RunFocusAction::Rescued, pane_id)),
        Err(error) => Err(Error::HerdrUnavailable(format!(
            "reopening run {} of card {} failed: {error:#}",
            run.id, run.card_id
        ))),
    }
}

fn normalize_socket(path: &Path, kind: &str) -> Result<PathBuf> {
    path.canonicalize().map_err(|e| {
        Error::HerdrUnavailable(format!(
            "{kind} Herdr socket '{}' is unavailable: {e}",
            path.display()
        ))
    })
}

pub(super) fn run_retry(d: &Arc<Daemon>, p: RunCardParams) -> Result<Value> {
    let card = {
        let mut sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        if card.archived_at.is_some() {
            return Err(Error::InvalidState(
                "archived card must be restored before retrying".into(),
            ));
        }
        if db.open_run_for_card(p.card_id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; complete or cancel it before retrying".into(),
            ));
        }
        // Human action: reset the auto-chain counter and fork the session.
        sched.chain_hops.remove(&p.card_id);
        card
    };
    let run = enqueue_run(d, p.card_id, card.column_id, true)?;
    d.wake_dispatch();
    d.emit_changed(BoardChangedReason::CardUpdated, Some(p.card_id), None);
    let card = require_card(d, p.card_id)?;
    Ok(json!(RunActionResult { run, card }))
}

// -- harness / space --------------------------------------------------------
