//! `run.focus`: focus a run's live pane, or reopen a run whose pane is gone.
//!
//! Everything here is the *orchestration* half of the rescue — deciding that a
//! run can be reopened, and with what. The Herdr-side placement and launch live
//! in `spawner::rescue`, which shares the placement helpers with dispatch.
//!
//! **Nothing in this module writes to the database.** The historical `runs` row
//! of a rescued run stays byte-for-byte untouched; see [`rescue_run`].

use std::path::Path;
use std::sync::Arc;

use board_core::capability;
use board_core::harness;
use board_core::model::Run;
use board_core::protocol::{RunFocusAction, RunFocusParams, RunFocusResult};
use board_core::{Error, Result};
use serde_json::{json, Value};

use crate::dispatch::{owned_pane_ids, reconstruct_owned_tab_id, OwnedPanes};
use crate::herdr_conn::normalize_socket;
use crate::spawner::{rescue_run_pane, CardOwnership, RescueOutcome, RescuePlan};
use crate::state::Daemon;

pub(crate) fn focus_run(d: &Arc<Daemon>, p: RunFocusParams) -> Result<Value> {
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
            let mut client = crate::herdr_conn::connect_checked(&target_socket)
                .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
            client
                .pane_get(pane_id)
                .map_err(|e| Error::HerdrUnavailable(format!("pane.get {pane_id}: {e}")))?
                .map(|_| pane_id.clone())
        }
    };

    if let Some(pane_id) = live_pane {
        let mut client = crate::herdr_conn::connect_checked(&target_socket)
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
    run: &Run,
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
/// spawn time (`dispatch::launch_plan`); none of them live in the persisted launch
/// spec (a built-in's persisted `env` is empty), and protocol-17 `agent.start` carries
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
    run: &Run,
    execution: &mut board_core::launch::ExecutionSpec,
) -> Result<()> {
    // The resume spec already carries BOARD_RESCUE + BOARD_RESUME_SESSION_ID.
    // `None` is what withholds `BOARD_RUN_ID` from the shared dispatch helper.
    let mut injected = crate::dispatch::board_env(run.card_id, None, &d.socket_path)?;
    injected.push(("BOARD_RESCUED_RUN_ID".to_string(), run.id.to_string()));
    for (key, value) in injected {
        // The persisted spec never sets these, but never shadow it silently.
        execution.env.retain(|(existing, _)| existing != &key);
        execution.env.push((key, value));
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
    run: &Run,
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
    let durable_pane_ids = owned_pane_ids(
        &prior_runs,
        run.session.as_deref(),
        &workspace_id,
        OwnedPanes::DurableChildren,
    );
    let durable_anchor_pane_ids = owned_pane_ids(
        &prior_runs,
        run.session.as_deref(),
        &workspace_id,
        OwnedPanes::Anchors,
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
            let mut client = crate::herdr_conn::connect_checked(target_socket)
                .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
            let snapshot = client.session_snapshot().map_err(|e| {
                Error::HerdrUnavailable(format!("session.snapshot before rescue: {e}"))
            })?;
            reconstruct_owned_tab_id(&snapshot, &workspace_id, &ownership_proof)
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
