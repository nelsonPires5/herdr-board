//! Ephemeral rescue of a run whose pane is gone.
//!
//! This is the placement + launch half of the `run.focus` rescue, factored out
//! of `Spawner::spawn` so it can be reused **without** the run-promotion path
//! that writes to the database. A rescue deliberately persists nothing: the
//! historical `runs` row stays immutable, so the pane created here has no run
//! row and is therefore not owned, watched, or timed out by the daemon.
//!
//! The dead pane id is never reused or revived; a brand-new pane is split into
//! the card's tab and the harness conversation is resumed in it.
//!
//! Concurrency: a rescue places panes into the very same `card-<id>` tabs as
//! dispatch, and board requests are served concurrently (one task per
//! connection), so it takes the shared per-card allocation lock from
//! [`CardTabRegistry`] and registers what it allocated there. Without that, two
//! simultaneous focus requests — or a focus racing a dispatch — would each
//! create a pane, or a whole second `card-<id>` tab.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use board_herdr::{AgentStatus, HerdrClient, PaneInfo, PaneRenameParams};

use super::card_tabs::{CardTabKey, CardTabRegistry};
use super::herdr::{
    launch_configured, launch_managed, HerdrCliPaneRunner, PaneRunner, DEFAULT_AGENT_START_DELAY,
};
use super::placement::{
    allocate_owned_pane, close_owned_after_error, close_owned_for_retry, is_pane_not_found,
    mark_retryable_placement_race, CardOwnership,
};
use super::HerdrLaunchPlan;
use crate::dispatch::workspace_cwd;
use crate::herdr_conn::connect_checked_for;

/// Everything a rescue needs. All of it is derived from the run row plus the
/// live Herdr session — nothing is written back.
pub(crate) struct RescuePlan<'a> {
    /// The rescued pane's `agent.start` name **and** its pane label. The *label*
    /// is the dedup correlator (the one field we set and can read back); the
    /// agent name additionally buys Herdr's `agent_name_taken` exclusivity as a
    /// backstop. Because the rescue may not write to the database, this name is
    /// the only trace a previous rescue can leave, so it must depend on nothing
    /// but stable identity (card id + run id). See [`find_rescued_pane`] for the
    /// honest limits of that.
    pub(crate) marker_name: &'a str,
    /// `card-<id>` — the durable card tab to place the pane in.
    pub(crate) tab_label: &'a str,
    pub(crate) workspace_id: &'a str,
    pub(crate) socket: &'a Path,
    /// Exact tab/pane ownership evidence from the card's run rows. Note that
    /// `reclaimable_pane_ids` is always empty for a rescue: reopening one run
    /// must never close another run's pane.
    pub(crate) ownership: CardOwnership<'a>,
    /// The resume launch, built by [`board_core::harness::resume_invocation`]
    /// from the run's persisted execution spec (so model/effort/env match the
    /// original run) with the initial prompt deliberately cleared.
    pub(crate) execution: board_core::launch::ExecutionSpec,
    /// Shared card-tab allocation state, so this rescue serializes against
    /// dispatch and against another concurrent rescue of the same card. `None`
    /// only when the daemon has no Herdr-placing spawner.
    pub(crate) card_tabs: Option<Arc<CardTabRegistry>>,
}

/// What the rescue found or did.
pub(crate) enum RescueOutcome {
    /// A pane from an earlier rescue of this exact run was still alive; it was
    /// focused and nothing new was created.
    AlreadyLive(String),
    /// A new pane was created and the conversation resumed in it.
    Created(String),
}

/// Is this pane a still-running earlier rescue of this run, or just a leftover
/// label on a shell whose harness already exited?
///
/// A Herdr pane label outlives the process that ran in it, so matching the label
/// alone would make `o` a permanent no-op once the resumed harness exits: every
/// later press would report "focused the rescued pane" and start nothing. The
/// extra evidence available depends on the harness kind:
///
/// - **managed** (`agent_kind: Some`): Herdr tracks a registered agent for the
///   pane, so require `PaneInfo::agent` to still be *present*. Deliberately a
///   presence test, not an equality test against our `agent.start` name: the
///   pinned protocol-17 schema gives `AgentInfo` **both** an `agent` and a
///   separate `name` field, and `e2e/16-managed-p17.sh` matches `pane.agent`
///   against the agent *kind* (`pi`/`claude`), so `agent` is not the exclusive
///   name we chose and must not be compared to it. Presence is the same
///   semantics `placement::alloc` already relies on for `usable_anchor`, and it is
///   correct either way: when the managed process goes, the registration goes.
/// - **configured** (`agent_kind: None`): intentionally unmanaged, so Herdr
///   registers no agent for it at all (see `placement::alloc`). The label is the only
///   evidence, so a leftover configured shell cannot be distinguished from a live
///   one — recorded in `docs/design.md` as a limitation of unmanaged harnesses
///   rather than papered over.
///
/// In both cases a `Done` agent status counts as dead.
fn rescued_pane_is_live(pane: &PaneInfo, marker_name: &str, managed: bool) -> bool {
    if pane.label.as_deref() != Some(marker_name) {
        // Only our own label proves which run a pane belongs to.
        return false;
    }
    if matches!(pane.agent_status, AgentStatus::Done) {
        return false;
    }
    !managed || pane.agent.is_some()
}

/// Panes in this workspace that an earlier rescue of this exact run created,
/// identified by the exact pane label this code set with `pane.rename`.
///
/// The label is used because it is the one field we both **write** (`pane.rename
/// {pane_id, label}`) and can **read back** (`PaneInfo::label`) under the pinned
/// protocol-17 schema. The `agent.start` name is deliberately *not* matched
/// against `PaneInfo::agent`: that field is not the exclusive name we chose (see
/// [`rescued_pane_is_live`]).
///
/// **Reliability, stated plainly:** because the user's design forbids any
/// database write, there is no authoritative record of a prior rescue. This label
/// match is a *diagnostic hint*. It is deterministic for the panes this code
/// creates, and `marker_name` derives only from card id + run id, so nothing a
/// user renames on the board (a column, say) can change it. It stops being
/// reliable if the user renames the pane, or if Herdr drops the label — then a
/// second `o` creates a second pane (though for a managed harness it will more
/// often fail closed with `agent_name_taken` instead, since Herdr agent names are
/// exclusive while the pane using one is open). That weakness is a direct
/// consequence of the no-DB-writes decision.
fn find_rescued_pane(
    client: &mut HerdrClient,
    workspace_id: &str,
    marker_name: &str,
) -> anyhow::Result<Vec<PaneInfo>> {
    let panes = client
        .pane_list(Some(workspace_id))
        .map_err(anyhow::Error::new)
        .context("herdr pane.list while looking for an existing rescued pane")?;
    Ok(panes
        .into_iter()
        .filter(|pane| {
            pane.workspace_id == workspace_id && pane.label.as_deref() == Some(marker_name)
        })
        .collect())
}

/// Focus-or-create: idempotent by `marker_name`. Opens its own Herdr connection
/// (one connection per operation, per `AGENTS.md`).
pub(crate) fn rescue_run_pane(plan: &RescuePlan<'_>) -> anyhow::Result<RescueOutcome> {
    let tab_key: CardTabKey = (
        plan.socket.to_path_buf(),
        plan.workspace_id.to_string(),
        plan.tab_label.to_string(),
    );
    // Serialize the whole discover→create→launch sequence for this card tab
    // against dispatch and against another concurrent rescue. Held to the end.
    let allocation_lock = plan
        .card_tabs
        .as_ref()
        .map(|registry| registry.allocation_lock(&tab_key))
        .transpose()?;
    let _allocation_guard = allocation_lock
        .as_ref()
        .map(|lock| {
            lock.lock()
                .map_err(|_| anyhow!("card-tab allocation lock poisoned"))
        })
        .transpose()?;

    // The gate must precede any placement or launch action, exactly as in
    // `spawn`; `connect_checked_for` is the one place that pairs the two.
    let mut client = connect_checked_for(plan.socket, "the run-pane rescue")?;

    let managed = plan.execution.agent_kind.is_some();

    // Idempotency first: never create before checking. The recorded pane was
    // already probed with `pane.get` by the caller, so this only looks for a
    // pane an *earlier rescue* left behind.
    let candidates = find_rescued_pane(&mut client, plan.workspace_id, plan.marker_name)?;
    if let Some(live) = candidates
        .iter()
        .find(|pane| rescued_pane_is_live(pane, plan.marker_name, managed))
    {
        let pane_id = live.pane_id.clone();
        client
            .pane_focus(&pane_id)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("herdr pane.focus existing rescued pane {pane_id}"))?;
        return Ok(RescueOutcome::AlreadyLive(pane_id));
    }
    // Whatever is left carries our exact run-scoped marker but is dead. Reclaim
    // it before splitting again: repeated presses of `o` would otherwise pile up
    // idle shells that nothing can ever collect, because a rescue leaves no run
    // row to reclaim them from. An actively working/blocked pane is never a
    // candidate, mirroring `placement::alloc::reclaim_prior_children`.
    for stale in candidates.iter().filter(|pane| {
        !matches!(
            pane.agent_status,
            AgentStatus::Working | AgentStatus::Blocked
        )
    }) {
        close_owned_for_retry(&mut client, &stale.pane_id).with_context(|| {
            format!(
                "reclaiming the dead pane {} of an earlier rescue",
                stale.pane_id
            )
        })?;
    }

    // Protocol 17 never inherits a workspace cwd, so the rescue pane's cwd is
    // read from a live pane of the run's recorded workspace — the same rule
    // dispatch uses. A vanished workspace fails here rather than launching in
    // some implicit directory.
    let cwd = workspace_cwd(&mut client, plan.workspace_id).with_context(|| {
        format!(
            "resolving cwd for the rescue of {} in workspace {}",
            plan.marker_name, plan.workspace_id
        )
    })?;
    let cwd = PathBuf::from(cwd);

    let env: BTreeMap<String, String> = plan.execution.env.iter().cloned().collect();
    let remembered = plan
        .card_tabs
        .as_ref()
        .map(|registry| registry.remembered(&tab_key))
        .transpose()?
        .flatten();
    // Prefer the caller's durable evidence, then this daemon's memory of the tab
    // (which may be a tab an earlier rescue created).
    let owned_tab_id = plan
        .ownership
        .owned_tab_id
        .map(str::to_string)
        .or_else(|| remembered.as_ref().map(|owned| owned.tab_id.clone()));
    let owned = allocate_owned_pane(
        &mut client,
        plan.workspace_id,
        plan.tab_label,
        Some(cwd.as_path()),
        &env,
        CardOwnership {
            owned_tab_id: owned_tab_id.as_deref(),
            durable_pane_ids: plan.ownership.durable_pane_ids,
            // A rescue never reclaims (closes) another run's pane.
            reclaimable_pane_ids: &[],
            durable_anchor_pane_ids: plan.ownership.durable_anchor_pane_ids,
            remembered_anchor_id: remembered
                .as_ref()
                .map(|owned| owned.anchor_pane_id.as_str()),
        },
    )
    .with_context(|| {
        format!(
            "placing a rescue pane in tab '{}' for {}",
            plan.tab_label, plan.marker_name
        )
    })?;
    if owned.pane_id.is_empty() {
        return Err(anyhow!("herdr returned an empty rescue pane id"));
    }

    // Did placement have to create the tab? `allocate_card_pane` creates one
    // whenever the exact owned tab id does not resolve, so a tab id we did not
    // ask for is a new tab — and a failure below must then close its anchor too,
    // not just the child, or the empty tab is orphaned forever (a rescue leaves
    // no run row that could ever reclaim it).
    let created_tab = owned_tab_id.as_deref() != Some(owned.tab_id.as_str());

    // Remember the exact tab/anchor for the next allocation — including a tab
    // this rescue created, so a later dispatch reuses it instead of making a
    // second one.
    if let (Some(registry), Some(anchor)) = (plan.card_tabs.as_ref(), owned.anchor_pane_id.as_ref())
    {
        registry.remember(tab_key.clone(), owned.tab_id.clone(), anchor.clone())?;
    }

    let launched = launch_rescue(&mut client, plan, &cwd, &owned.pane_id);
    if let Err(error) = launched {
        return Err(abandon_rescue(
            &mut client,
            plan,
            &tab_key,
            &owned,
            created_tab,
            error,
        ));
    }

    // The rescue has already succeeded here: the pane exists and the
    // conversation is resumed in it. A focus failure is cosmetic, so warn rather
    // than turn a completed rescue into an error the caller would only discover
    // was a lie on the next `o`.
    if let Err(error) = client.pane_focus(&owned.pane_id) {
        tracing::warn!(
            "rescued run pane {} was created and resumed, but focusing it failed: {error}",
            owned.pane_id
        );
    }
    Ok(RescueOutcome::Created(owned.pane_id))
}

/// Label the new pane with the dedup marker, then start the resumed harness in
/// it. Labelling happens before the launch so the correlator exists even if the
/// launch is slow; a failed launch removes the pane again.
///
/// Verified against Herdr 0.7.5 / protocol 17 on a live socket: `agent.start`
/// leaves a board-set `label` untouched, so this single pre-launch rename is
/// enough and the label is still `marker_name` afterwards. That was checked by
/// running `e2e/27-rescue-dead-pane.sh` with the label re-asserted post-launch
/// and again without it — identical result — so the scenario's second-focus
/// assertion is the standing guard if a future Herdr ever clobbers it.
fn launch_rescue(
    client: &mut HerdrClient,
    plan: &RescuePlan<'_>,
    cwd: &Path,
    pane_id: &str,
) -> anyhow::Result<()> {
    client
        .pane_rename(&PaneRenameParams {
            pane_id: pane_id.to_string(),
            label: plan.marker_name.to_string(),
        })
        .map_err(mark_retryable_placement_race)
        .with_context(|| format!("labeling rescued pane {pane_id}"))?;

    let req = HerdrLaunchPlan {
        name: plan.marker_name.to_string(),
        agent_kind: plan.execution.agent_kind.clone(),
        // Cleared by `resume_invocation`: resuming must not re-send the task.
        initial_prompt: plan.execution.initial_prompt.clone(),
        system_prompt: plan.execution.system_prompt.clone(),
        // No fallback name. A taken rescue name means a live rescued pane the
        // scan failed to see; failing closed beats a second pane.
        name_fallback: None,
        tab_label: Some(plan.tab_label.to_string()),
        owned_tab_id: None,
        durable_pane_ids: Vec::new(),
        reclaimable_pane_ids: Vec::new(),
        durable_anchor_pane_ids: Vec::new(),
        cwd: Some(cwd.to_path_buf()),
        workspace_ref: Some(plan.workspace_id.to_string()),
        herdr_socket: Some(plan.socket.to_path_buf()),
        env: plan.execution.env.clone(),
        argv: plan.execution.argv.clone(),
    };

    match req.agent_kind.as_deref() {
        Some(kind) => launch_managed(client, &req, kind, pane_id, DEFAULT_AGENT_START_DELAY),
        None => {
            let runner = HerdrCliPaneRunner;
            launch_configured(
                client,
                &runner as &dyn PaneRunner,
                plan.socket,
                &req,
                pane_id,
            )
        }
    }
}

/// Undo everything this rescue created. Always the child pane; additionally the
/// anchor (which removes the otherwise-empty tab) when placement had to create
/// the tab. Unlike dispatch, a rescue has neither a retry nor a run row, so an
/// orphan left here is permanent.
fn abandon_rescue(
    client: &mut HerdrClient,
    plan: &RescuePlan<'_>,
    tab_key: &CardTabKey,
    owned: &super::placement::OwnedPane,
    created_tab: bool,
    error: anyhow::Error,
) -> anyhow::Error {
    let error = close_owned_after_error(client, &owned.pane_id, error);
    if !created_tab {
        return error;
    }
    // The remembered tab is about to stop existing; a stale id would make the
    // next allocation try to split from a pane that is gone.
    if let Some(registry) = plan.card_tabs.as_ref() {
        if let Err(forget_error) = registry.forget(tab_key) {
            return error.context(format!(
                "additionally failed to forget the card tab this rescue created: {forget_error:#}"
            ));
        }
    }
    let Some(anchor) = owned.anchor_pane_id.as_deref() else {
        return error;
    };
    match client.pane_close(anchor) {
        Ok(()) => error,
        Err(cleanup_error) if is_pane_not_found(&cleanup_error) => error,
        Err(cleanup_error) => error.context(format!(
            "additionally failed to close the card tab this rescue created (anchor {anchor}): \
             {cleanup_error}"
        )),
    }
}
