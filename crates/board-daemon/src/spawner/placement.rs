use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use board_herdr::{
    AgentStatus, HerdrClient, HerdrError, Layout, LayoutPane, PaneInfo, PaneRenameParams,
    PaneSplitParams, SplitDirection, TabCreateParams,
};

pub(crate) const ERR_PANE_NOT_FOUND: &str = "pane_not_found";
pub(crate) const ERR_EMPTY_TAB: &str = "empty_tab";
pub(crate) const ERR_EMPTY_LAYOUT: &str = "empty_layout";
pub(crate) const ERR_ANCHOR_TOO_SMALL: &str = "anchor_too_small";

/// The root pane of a durable card tab is deliberately kept as a shell. These
/// dimensions describe the smallest useful anchor/agent split; Herdr itself
/// accepts the ratio but does not enforce a board-specific minimum.
pub(crate) const ANCHOR_MIN_WIDTH: u64 = 24;
pub(crate) const ANCHOR_MIN_HEIGHT: u64 = 6;
pub(crate) const AGENT_MIN_WIDTH: u64 = 12;
pub(crate) const AGENT_MIN_HEIGHT: u64 = 8;
pub(crate) const ANCHOR_RATIO: f64 = 0.40;
pub(crate) const RECOVERY_TARGET_RATIO: f64 = 0.75;

pub(crate) struct CardOwnership<'a> {
    pub(crate) owned_tab_id: Option<&'a str>,
    /// Exact prior run-child ids, newest first. These are tab-proof fallback
    /// evidence, never anchor candidates.
    pub(crate) durable_pane_ids: &'a [String],
    /// Exact ended run-child ids that may be reclaimed before the next split.
    pub(crate) reclaimable_pane_ids: &'a [String],
    /// Exact anchor ids remembered by the daemon or persisted on prior runs.
    pub(crate) durable_anchor_pane_ids: &'a [String],
    pub(crate) remembered_anchor_id: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct OwnedPane {
    /// The newly split pane that is safe to launch/close for this run.
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: String,
    /// The persistent shell anchor. This is never returned as the run pane.
    pub(crate) anchor_pane_id: Option<String>,
}

fn is_card_tab(label: &str) -> bool {
    label.starts_with("card-")
}

fn anchor_label(tab_label: &str) -> String {
    format!("{tab_label}-anchor")
}

/// Keep per-run values out of the long-lived anchor shell. The child receives
/// the complete run environment on its split, while the shell gets only the
/// stable card identity (if present).
fn anchor_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(key, _)| key.as_str() == "BOARD_CARD_ID")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Allocate a run pane. New durable card tabs reserve their tab-create root as
/// a shell anchor and always split a child from it. Legacy `kanban` rows retain
/// the old root/split behavior. A card tab is considered owned only when its
/// exact id was supplied from durable pane evidence (or is already held by
/// this daemon); labels are never enough on their own.
pub(crate) fn allocate_owned_pane(
    client: &mut HerdrClient,
    workspace_id: &str,
    label: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
    ownership: CardOwnership<'_>,
) -> anyhow::Result<OwnedPane> {
    if is_card_tab(label) {
        allocate_card_pane(client, workspace_id, label, cwd, env, ownership)
    } else {
        allocate_legacy_pane(client, workspace_id, label, cwd, env)
    }
}

fn allocate_legacy_pane(
    client: &mut HerdrClient,
    workspace_id: &str,
    label: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<OwnedPane> {
    let cwd = cwd.map(|path| path.to_string_lossy().into_owned());
    let tabs = client
        .tab_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?;
    let existing = tabs
        .iter()
        .filter(|tab| tab.label == label)
        .min_by_key(|tab| tab.number);
    let Some(tab) = existing else {
        let created = client
            .tab_create(&TabCreateParams {
                workspace_id: Some(workspace_id.to_string()),
                cwd,
                label: Some(label.to_string()),
                env: env.clone(),
                focus: false,
            })
            .map_err(anyhow::Error::new)?;
        return Ok(OwnedPane {
            pane_id: created.root_pane.pane_id,
            workspace_id: created.tab.workspace_id,
            tab_id: created.tab.tab_id,
            anchor_pane_id: None,
        });
    };

    let panes: Vec<_> = client
        .pane_list(Some(workspace_id))
        .map_err(mark_retryable_placement_race)?
        .into_iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .collect();
    let Some(anchor) = panes.first() else {
        return Err(mark_retryable_placement_race(HerdrError::Protocol {
            code: ERR_EMPTY_TAB.to_string(),
            message: format!("existing tab {} has no pane available to split", tab.tab_id),
        }));
    };
    let layout = client
        .pane_layout(Some(&anchor.pane_id))
        .map_err(mark_retryable_placement_race)?;
    let (target_pane_id, direction) =
        grid_slot_result(&layout.panes).map_err(mark_retryable_placement_race)?;
    let pane = client
        .pane_split(&PaneSplitParams {
            workspace_id: Some(workspace_id.to_string()),
            target_pane_id,
            cwd,
            env: env.clone(),
            direction,
            ratio: None,
            focus: false,
        })
        .map_err(mark_retryable_placement_race)?;
    Ok(OwnedPane {
        pane_id: pane.pane_id,
        workspace_id: if pane.workspace_id.is_empty() {
            workspace_id.to_string()
        } else {
            pane.workspace_id
        },
        tab_id: tab.tab_id.clone(),
        anchor_pane_id: None,
    })
}

fn allocate_card_pane(
    client: &mut HerdrClient,
    workspace_id: &str,
    tab_label: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
    ownership: CardOwnership<'_>,
) -> anyhow::Result<OwnedPane> {
    let CardOwnership {
        owned_tab_id,
        durable_pane_ids,
        reclaimable_pane_ids,
        durable_anchor_pane_ids,
        remembered_anchor_id,
    } = ownership;
    let cwd_string = cwd.map(|path| path.to_string_lossy().into_owned());
    let tabs = client
        .tab_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?;
    let exact_tab = owned_tab_id.and_then(|tab_id| {
        tabs.iter()
            .find(|tab| tab.tab_id == tab_id && tab.workspace_id == workspace_id)
    });

    let Some(tab) = exact_tab else {
        return create_card_tab(client, workspace_id, tab_label, cwd_string, env);
    };

    let panes: Vec<_> = client
        .pane_list(Some(workspace_id))
        .map_err(mark_retryable_placement_race)?
        .into_iter()
        .filter(|pane| pane.tab_id == tab.tab_id && pane.workspace_id == workspace_id)
        .collect();
    let expected_anchor = anchor_label(tab_label);
    let durable_child = |pane: &PaneInfo| durable_pane_ids.iter().any(|id| id == &pane.pane_id);
    let usable_anchor = |pane: &PaneInfo| {
        pane.workspace_id == workspace_id
            && pane.tab_id == tab.tab_id
            && pane.agent.is_none()
            && matches!(pane.agent_status, AgentStatus::Unknown | AgentStatus::Idle)
            && !durable_child(pane)
    };

    // Anchor ownership is identity-only. The remembered/persisted pane id must
    // resolve inside this exact workspace/tab and still be a shell; a display
    // label is deliberately never consulted to select one.
    let anchor = remembered_anchor_id
        .into_iter()
        .chain(durable_anchor_pane_ids.iter().map(String::as_str))
        .find_map(|id| {
            panes
                .iter()
                .find(|pane| pane.pane_id == id && usable_anchor(pane))
        });

    if let Some(anchor) = anchor {
        reclaim_prior_children(
            client,
            &panes,
            anchor.pane_id.as_str(),
            reclaimable_pane_ids,
        )?;
        return split_run_child(
            client,
            workspace_id,
            tab.tab_id.as_str(),
            anchor.pane_id.as_str(),
            cwd_string.as_deref(),
            env,
        );
    }

    // If the anchor was renamed or closed, only a durable board-run pane can
    // authorize touching an existing tab. A foreign pane in this tab is never
    // a recovery target. The new child from this split is a fresh shell anchor.
    let recovery = durable_pane_ids.iter().find_map(|id| {
        // Configured harness panes are intentionally unmanaged and therefore
        // have no Herdr `agent` field. The durable run-pane id itself is the
        // ownership proof; never require managed-agent metadata here. Do not
        // split from a pane whose agent is still actively working/blocked.
        panes.iter().find(|pane| {
            pane.pane_id == *id
                && pane.workspace_id == workspace_id
                && pane.tab_id == tab.tab_id
                && !matches!(
                    pane.agent_status,
                    AgentStatus::Working | AgentStatus::Blocked
                )
        })
    });
    let Some(recovery) = recovery else {
        // The exact tab has no usable board pane left. Leave it untouched and
        // create a new owned tab rather than guessing from a label or a user
        // pane. This also handles an empty exact tab after closure.
        return create_card_tab(client, workspace_id, tab_label, cwd_string, env);
    };

    let layout = client
        .pane_layout(Some(&recovery.pane_id))
        .map_err(mark_retryable_placement_race)?;
    let (direction, _) = split_geometry(&layout, &recovery.pane_id)?;
    let recovery_ratio = recovery_target_ratio(&layout, &recovery.pane_id, direction)?;
    let anchor_pane = client
        .pane_split(&PaneSplitParams {
            workspace_id: Some(workspace_id.to_string()),
            target_pane_id: recovery.pane_id.clone(),
            cwd: cwd_string.clone(),
            env: anchor_env(env),
            direction,
            ratio: Some(recovery_ratio),
            focus: false,
        })
        .map_err(mark_retryable_placement_race)?;
    let anchor_id = anchor_pane.pane_id;
    client
        .pane_rename(&PaneRenameParams {
            pane_id: anchor_id.clone(),
            label: expected_anchor,
        })
        .map_err(mark_retryable_placement_race)
        .context("labeling recreated card-tab anchor")?;
    reclaim_prior_children(client, &panes, &anchor_id, reclaimable_pane_ids)?;

    split_run_child(
        client,
        workspace_id,
        tab.tab_id.as_str(),
        &anchor_id,
        cwd_string.as_deref(),
        env,
    )
}

fn create_card_tab(
    client: &mut HerdrClient,
    workspace_id: &str,
    tab_label: &str,
    cwd: Option<String>,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<OwnedPane> {
    let created = client
        .tab_create(&TabCreateParams {
            workspace_id: Some(workspace_id.to_string()),
            cwd: cwd.clone(),
            label: Some(tab_label.to_string()),
            env: anchor_env(env),
            focus: false,
        })
        .map_err(anyhow::Error::new)?;
    let anchor_id = created.root_pane.pane_id;
    if let Err(error) = client
        .pane_rename(&PaneRenameParams {
            pane_id: anchor_id.clone(),
            label: anchor_label(tab_label),
        })
        .map_err(mark_retryable_placement_race)
        .context("labeling new card-tab anchor")
    {
        return Err(cleanup_new_card_tab(client, &anchor_id, error));
    }

    // A new tab's root occupies the full tab. Read its live geometry so a
    // narrow terminal can choose a vertical split instead of making the
    // future anchor too small; later/recovered splits use the same policy.
    let layout = match client
        .pane_layout(Some(&anchor_id))
        .map_err(mark_retryable_placement_race)
    {
        Ok(layout) => layout,
        Err(error) => return Err(cleanup_new_card_tab(client, &anchor_id, error)),
    };
    let (direction, ratio) = match initial_split_geometry(&layout, &anchor_id) {
        Ok(geometry) => geometry,
        Err(error) => return Err(cleanup_new_card_tab(client, &anchor_id, error)),
    };
    let child = match client
        .pane_split(&PaneSplitParams {
            workspace_id: Some(workspace_id.to_string()),
            target_pane_id: anchor_id.clone(),
            cwd,
            env: env.clone(),
            direction,
            ratio: Some(ratio),
            focus: false,
        })
        .map_err(mark_retryable_placement_race)
    {
        Ok(child) => child,
        Err(error) => return Err(cleanup_new_card_tab(client, &anchor_id, error)),
    };
    Ok(OwnedPane {
        pane_id: child.pane_id,
        workspace_id: if child.workspace_id.is_empty() {
            workspace_id.to_string()
        } else {
            child.workspace_id
        },
        tab_id: created.tab.tab_id,
        anchor_pane_id: Some(anchor_id),
    })
}

fn split_run_child(
    client: &mut HerdrClient,
    workspace_id: &str,
    tab_id: &str,
    anchor_id: &str,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<OwnedPane> {
    let layout = client
        .pane_layout(Some(anchor_id))
        .map_err(mark_retryable_placement_race)?;
    let (direction, ratio) = split_geometry(&layout, anchor_id)?;
    let child = client
        .pane_split(&PaneSplitParams {
            workspace_id: Some(workspace_id.to_string()),
            target_pane_id: anchor_id.to_string(),
            cwd: cwd.map(str::to_string),
            env: env.clone(),
            direction,
            ratio: Some(ratio),
            focus: false,
        })
        .map_err(mark_retryable_placement_race)?;
    Ok(OwnedPane {
        pane_id: child.pane_id,
        workspace_id: if child.workspace_id.is_empty() {
            workspace_id.to_string()
        } else {
            child.workspace_id
        },
        tab_id: tab_id.to_string(),
        anchor_pane_id: Some(anchor_id.to_string()),
    })
}

pub(crate) fn initial_split_geometry(
    layout: &Layout,
    target_id: &str,
) -> anyhow::Result<(SplitDirection, f64)> {
    let target = layout
        .panes
        .iter()
        .find(|pane| pane.pane_id == target_id)
        .ok_or_else(|| anyhow::anyhow!("layout has no target pane {target_id}"))?;
    let right_ok = target.rect.width >= ANCHOR_MIN_WIDTH + AGENT_MIN_WIDTH
        && target.rect.height >= ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT);
    let down_ok = target.rect.height >= ANCHOR_MIN_HEIGHT + AGENT_MIN_HEIGHT
        && target.rect.width >= ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH);
    let prefer_right = target.rect.width >= 2_u64.saturating_mul(target.rect.height);
    let direction = if prefer_right && right_ok {
        SplitDirection::Right
    } else if down_ok {
        SplitDirection::Down
    } else if right_ok {
        SplitDirection::Right
    } else {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is {}x{}, need at least 36x14 for an anchor and agent",
            target.rect.width,
            target.rect.height
        ));
    };
    let (split_axis, anchor_min, child_min) = match direction {
        SplitDirection::Right => (target.rect.width, ANCHOR_MIN_WIDTH, AGENT_MIN_WIDTH),
        SplitDirection::Down => (target.rect.height, ANCHOR_MIN_HEIGHT, AGENT_MIN_HEIGHT),
    };
    let min_ratio = anchor_min as f64 / split_axis as f64;
    let max_ratio = 1.0 - child_min as f64 / split_axis as f64;
    Ok((direction, ANCHOR_RATIO.clamp(min_ratio, max_ratio)))
}

pub(crate) fn recovery_target_ratio(
    layout: &Layout,
    target_id: &str,
    direction: SplitDirection,
) -> anyhow::Result<f64> {
    let target = layout
        .panes
        .iter()
        .find(|pane| pane.pane_id == target_id)
        .ok_or_else(|| anyhow::anyhow!("layout has no target pane {target_id}"))?;
    let axis = match direction {
        SplitDirection::Right => target.rect.width,
        SplitDirection::Down => target.rect.height,
    };
    let target_min = match direction {
        SplitDirection::Right => AGENT_MIN_WIDTH,
        SplitDirection::Down => AGENT_MIN_HEIGHT,
    };
    let anchor_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_WIDTH + AGENT_MIN_WIDTH,
        SplitDirection::Down => ANCHOR_MIN_HEIGHT + AGENT_MIN_HEIGHT,
    };
    let total_min = target_min.saturating_add(anchor_min);
    if axis < total_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is too small to recreate an anchor and split a child"
        ));
    }
    let cross_axis = match direction {
        SplitDirection::Right => target.rect.height,
        SplitDirection::Down => target.rect.width,
    };
    let cross_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT),
        SplitDirection::Down => ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH),
    };
    if cross_axis < cross_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is too narrow to recreate an anchor"
        ));
    }
    let min_target_ratio = target_min as f64 / axis as f64;
    let max_target_ratio = 1.0 - anchor_min as f64 / axis as f64;
    Ok(RECOVERY_TARGET_RATIO.clamp(min_target_ratio, max_target_ratio))
}

pub(crate) fn split_geometry(
    layout: &Layout,
    target_id: &str,
) -> anyhow::Result<(SplitDirection, f64)> {
    let target = layout
        .panes
        .iter()
        .find(|pane| pane.pane_id == target_id)
        .ok_or_else(|| anyhow::anyhow!("layout has no target pane {target_id}"))?;
    let right_ok = target.rect.width >= ANCHOR_MIN_WIDTH + AGENT_MIN_WIDTH
        && target.rect.height >= ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT);
    let down_ok = target.rect.height >= ANCHOR_MIN_HEIGHT + AGENT_MIN_HEIGHT
        && target.rect.width >= ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH);
    let prefer_right = target.rect.width >= 2_u64.saturating_mul(target.rect.height);
    let direction = if prefer_right && right_ok {
        SplitDirection::Right
    } else if down_ok {
        SplitDirection::Down
    } else if right_ok {
        SplitDirection::Right
    } else {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {} is {}x{}, need at least 36x14 for an anchor and agent",
            target_id,
            target.rect.width,
            target.rect.height
        ));
    };
    let (split_axis, cross_axis, anchor_min, child_min) = match direction {
        SplitDirection::Right => (
            target.rect.width,
            target.rect.height,
            ANCHOR_MIN_WIDTH,
            AGENT_MIN_WIDTH,
        ),
        SplitDirection::Down => (
            target.rect.height,
            target.rect.width,
            ANCHOR_MIN_HEIGHT,
            AGENT_MIN_HEIGHT,
        ),
    };
    let cross_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT),
        SplitDirection::Down => ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH),
    };
    if split_axis < anchor_min.saturating_add(child_min) || cross_axis < cross_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is {}x{}, need at least {}x{} for an anchor and agent",
            target.rect.width,
            target.rect.height,
            if matches!(direction, SplitDirection::Right) {
                anchor_min + child_min
            } else {
                cross_min
            },
            if matches!(direction, SplitDirection::Right) {
                cross_min
            } else {
                anchor_min + child_min
            }
        ));
    }
    let min_ratio = anchor_min as f64 / split_axis as f64;
    let max_ratio = 1.0 - child_min as f64 / split_axis as f64;
    let ratio = ANCHOR_RATIO.clamp(min_ratio, max_ratio);
    Ok((direction, ratio))
}

fn grid_slot_result(panes: &[LayoutPane]) -> Result<(String, SplitDirection), HerdrError> {
    if panes.is_empty() {
        return Err(HerdrError::Protocol {
            code: ERR_EMPTY_LAYOUT.to_string(),
            message: "existing tab layout has no pane available to split".to_string(),
        });
    }
    Ok(grid_slot(panes))
}

/// Choose the largest pane and a roughly-square split direction.
pub fn grid_slot(panes: &[LayoutPane]) -> (String, SplitDirection) {
    let Some(target) = panes
        .iter()
        .max_by_key(|pane| pane.rect.width.saturating_mul(pane.rect.height))
    else {
        // The public helper predates fallible placement. Production checks the
        // precondition in `grid_slot_result`; retain a non-panicking fallback.
        return (String::new(), SplitDirection::Down);
    };
    let direction = if target.rect.width >= 2_u64.saturating_mul(target.rect.height) {
        SplitDirection::Right
    } else {
        SplitDirection::Down
    };
    (target.pane_id.clone(), direction)
}

// ---------------------------------------------------------------------------
// Placement retry / error helpers
// ---------------------------------------------------------------------------

/// Marks placement disappearance only at operations where restarting the
/// complete placement is safe. Keeping `HerdrError` as the source preserves
/// its typed protocol code in the anyhow chain.
#[derive(Debug)]
pub(crate) struct RetryablePlacementRace(pub(crate) HerdrError);

impl std::fmt::Display for RetryablePlacementRace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RetryablePlacementRace {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub(crate) fn mark_retryable_placement_race(error: HerdrError) -> anyhow::Error {
    if is_placement_disappearance(&error) {
        anyhow::Error::new(RetryablePlacementRace(error))
    } else {
        anyhow::Error::new(error)
    }
}

pub(crate) fn is_retryable_placement_race(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<RetryablePlacementRace>().is_some()
            || cause
                .downcast_ref::<RetryableRunnerPlacementRace>()
                .is_some()
    })
}

pub(crate) fn mark_retryable_runner_race(error: anyhow::Error) -> anyhow::Error {
    let pane_disappeared = error.chain().any(|cause| {
        cause
            .downcast_ref::<HerdrError>()
            .is_some_and(is_pane_not_found)
    });
    if pane_disappeared {
        anyhow::Error::new(RetryableRunnerPlacementRace(error))
    } else {
        error
    }
}

#[derive(Debug)]
pub(crate) struct RetryableRunnerPlacementRace(anyhow::Error);

impl std::fmt::Display for RetryableRunnerPlacementRace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RetryableRunnerPlacementRace {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub(crate) fn is_placement_disappearance(error: &HerdrError) -> bool {
    matches!(
        error,
        HerdrError::Protocol { code, .. }
            if matches!(code.as_str(), ERR_PANE_NOT_FOUND | ERR_EMPTY_TAB | ERR_EMPTY_LAYOUT)
    )
}

pub(crate) fn is_pane_not_found(error: &HerdrError) -> bool {
    matches!(
        error,
        HerdrError::Protocol { code, .. } if code == ERR_PANE_NOT_FOUND
    )
}

/// Close only exact ended child panes from this card/tab before another split.
/// Active/blocked panes are left untouched even when a stale database row says
/// the run ended; foreign panes and the anchor are not candidates.
fn reclaim_prior_children(
    client: &mut HerdrClient,
    panes: &[PaneInfo],
    anchor_id: &str,
    reclaimable_ids: &[String],
) -> anyhow::Result<()> {
    for pane in panes.iter().filter(|pane| {
        pane.pane_id != anchor_id
            && reclaimable_ids.iter().any(|id| id == &pane.pane_id)
            && !matches!(
                pane.agent_status,
                AgentStatus::Working | AgentStatus::Blocked
            )
    }) {
        close_owned_for_retry(client, &pane.pane_id)
            .with_context(|| format!("reclaiming prior board-owned child {}", pane.pane_id))?;
    }
    Ok(())
}

/// A fresh card tab has no durable ownership yet. If any later placement step
/// fails, close only its newly-created root; closing that root also removes the
/// otherwise empty tab. A race that already removed it is success.
fn cleanup_new_card_tab(
    client: &mut HerdrClient,
    anchor_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match client.pane_close(anchor_id) {
        Ok(()) => error,
        Err(cleanup_error) if is_pane_not_found(&cleanup_error) => error,
        Err(cleanup_error) => error.context(format!(
            "additionally failed to clean up newly created card-tab anchor {anchor_id}: {cleanup_error}"
        )),
    }
}

pub(crate) fn close_owned_for_retry(client: &mut HerdrClient, pane_id: &str) -> anyhow::Result<()> {
    match client.pane_close(pane_id) {
        Ok(()) => Ok(()),
        Err(error) if is_pane_not_found(&error) => Ok(()),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("herdr pane.close board-owned pane {pane_id}"))),
    }
}

pub(crate) fn close_owned_after_error(
    client: &mut HerdrClient,
    pane_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    match client.pane_close(pane_id) {
        Ok(()) => error,
        Err(cleanup_error) if is_pane_not_found(&cleanup_error) => error,
        Err(cleanup_error) => error.context(format!(
            "additionally failed to close board-owned pane {pane_id}: {cleanup_error}"
        )),
    }
}
