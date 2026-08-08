//! Tab/pane allocation: choose (or create) a card's tab, keep its shell anchor,
//! and split the run child from it. Every function here talks to Herdr; the
//! pure sizing decisions live in [`super::geometry`].

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use board_herdr::{
    AgentStatus, HerdrClient, HerdrError, LayoutPane, PaneInfo, PaneRenameParams, PaneSplitParams,
    SplitDirection, TabCreateParams, TabRenameParams,
};

use super::geometry::{initial_split_geometry, recovery_target_ratio, split_geometry};
use super::race::{
    cleanup_new_card_tab, close_owned_for_retry, mark_retryable_placement_race, ERR_EMPTY_LAYOUT,
    ERR_EMPTY_TAB,
};
use crate::spawner::WorkspaceBootstrapHint;

pub(crate) struct CardOwnership<'a> {
    pub(crate) owned_tab_id: Option<&'a str>,
    /// One-shot bootstrap hint from a workspace this launch just created.
    /// Only the first card-tab allocation may adopt it, and only after strict
    /// verification; any mismatch falls back to a fresh `tab.create`.
    pub(crate) bootstrap: Option<&'a WorkspaceBootstrapHint>,
    /// Exact prior run-child ids, newest first. These are tab-proof fallback
    /// evidence, never anchor candidates.
    pub(crate) durable_pane_ids: &'a [String],
    /// Exact ended run-child ids that may be reclaimed before the next split.
    pub(crate) reclaimable_pane_ids: &'a [String],
    /// Exact anchor ids remembered by the daemon or persisted on prior runs.
    pub(crate) durable_anchor_pane_ids: &'a [String],
    pub(crate) remembered_anchor_id: Option<&'a str>,
    /// A prior run-child pane to reuse on a same-conversation resume hop
    /// (instead of splitting a fresh child). `None` keeps the historical
    /// always-split behavior.
    pub(crate) reuse_pane_id: Option<&'a str>,
    /// Expected managed-agent kind (`pi`/`claude`) on the reuse candidate.
    /// Pane identity alone must not redirect a stage prompt to a different
    /// agent a user started after the prior run.
    pub(crate) reuse_agent_kind: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct OwnedPane {
    /// The pane that is safe to launch/close for this run. On a reuse hop this
    /// is the prior run's still-live agent pane rather than a freshly split one.
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
        bootstrap,
        durable_pane_ids,
        reclaimable_pane_ids,
        durable_anchor_pane_ids,
        remembered_anchor_id,
        reuse_pane_id,
        reuse_agent_kind,
    } = ownership;
    let cwd_string = cwd.map(|path| path.to_string_lossy().into_owned());
    let cwd_owned = cwd_string.clone();
    let tabs = client
        .tab_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?;
    let exact_tab = owned_tab_id.and_then(|tab_id| {
        tabs.iter()
            .find(|tab| tab.tab_id == tab_id && tab.workspace_id == workspace_id)
    });

    // A workspace this dispatch just created starts with exactly one initial
    // tab whose root is an idle shell. Adopt it as this card's first tab
    // instead of leaving an unused initial tab next to a fresh `card-<id>`
    // one: verify the exact ids are still live, that the root is the tab's
    // sole pane, and that it carries no agent — then rename tab and root and
    // use the ordinary anchor split path. Any verification mismatch falls
    // back to a fresh `tab.create` and never touches that root. The hint is
    // one-shot: reused/existing/user workspaces never produce one.
    if let Some(bootstrap) = bootstrap {
        if let Some(owned) =
            adopt_bootstrap_tab(client, workspace_id, tab_label, bootstrap, cwd_owned, env)
                .with_context(|| {
                    format!("adopting the created workspace's initial tab for '{tab_label}'")
                })?
        {
            return Ok(owned);
        }
    }

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

    // A same-conversation resume hop reuses the prior run's still-live
    // agent pane: the conversation + agent are already there, so the next
    // stage is delivered with `agent.prompt` on this pane rather than a
    // fresh `pane.split` + `agent.start`. Only an exact prior durable child
    // in this tab/workspace that still holds its agent qualifies; the
    // candidate's live status is re-checked here. Herdr's derived Done is a
    // live, quiescent end-of-turn state (not a missing pane). A briefly
    // Working/Blocked pane is still adopted — launch waits for it to become
    // quiescent before prompting.
    //
    // Eligibility deliberately comes BEFORE anchor selection: managed tabs
    // converge to exactly one harness pane and no anchor, so a tab with no
    // usable anchor must still reuse its harness pane on the next hop.
    if let Some(reuse_id) = reuse_pane_id {
        if let Some(reuse_pane) = panes.iter().find(|pane| {
            pane.pane_id == reuse_id
                && pane.workspace_id == workspace_id
                && pane.tab_id == tab.tab_id
                && reuse_agent_kind.is_some()
                && pane.agent.as_deref() == reuse_agent_kind
                && matches!(
                    pane.agent_status,
                    AgentStatus::Idle
                        | AgentStatus::Working
                        | AgentStatus::Blocked
                        | AgentStatus::Done
                )
        }) {
            // Close any *other* ended children (e.g. leftovers from a fresh
            // column) so the tab keeps one agent child; the reuse pane is
            // protected and stays open. The anchor (when one still exists) is
            // not a reclaim candidate either way.
            let protected = anchor.map(|pane| pane.pane_id.as_str()).unwrap_or(reuse_id);
            reclaim_prior_children(
                client,
                &panes,
                protected,
                reclaimable_pane_ids,
                Some(reuse_id),
            )?;
            return Ok(OwnedPane {
                pane_id: reuse_pane.pane_id.clone(),
                workspace_id: workspace_id.to_string(),
                tab_id: tab.tab_id.clone(),
                anchor_pane_id: anchor.map(|pane| pane.pane_id.clone()),
            });
        }
    }

    if let Some(anchor) = anchor {
        reclaim_prior_children(
            client,
            &panes,
            anchor.pane_id.as_str(),
            reclaimable_pane_ids,
            None,
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
    reclaim_prior_children(client, &panes, &anchor_id, reclaimable_pane_ids, None)?;

    split_run_child(
        client,
        workspace_id,
        tab.tab_id.as_str(),
        &anchor_id,
        cwd_string.as_deref(),
        env,
    )
}

/// Try to adopt a workspace this launch just created as the card's first tab.
///
/// Returns `Ok(None)` when the exact bootstrap ids no longer verify (tab or
/// root missing, the root is not the tab's sole pane, or the root carries an
/// agent) — the caller then falls back to a fresh `tab.create` and the
/// workspace root is left completely untouched. Returns `Ok(Some(owned))`
/// after renaming tab and root and splitting the run child from the adopted
/// root. Post-verification Herdr errors (renames/split) propagate so the
/// caller's existing retry/cleanup taxonomy applies; the workspace root is
/// never closed here — unlike a `tab.create` root, it predates this card and
/// must survive a failed run.
fn adopt_bootstrap_tab(
    client: &mut HerdrClient,
    workspace_id: &str,
    tab_label: &str,
    bootstrap: &WorkspaceBootstrapHint,
    cwd: Option<String>,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<Option<OwnedPane>> {
    // Verification: the exact tab still exists in the exact workspace.
    let tabs = client
        .tab_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?;
    if !tabs
        .iter()
        .any(|tab| tab.tab_id == bootstrap.tab_id && tab.workspace_id == workspace_id)
    {
        return Ok(None);
    }
    // Verification: the exact root still exists, is the tab's SOLE pane, and
    // carries no agent. Anything else means the workspace is no longer the
    // pristine shell it was created as; adopt nothing.
    let panes: Vec<_> = client
        .pane_list(Some(workspace_id))
        .map_err(anyhow::Error::new)?
        .into_iter()
        .filter(|pane| pane.tab_id == bootstrap.tab_id && pane.workspace_id == workspace_id)
        .collect();
    if panes.len() != 1 || panes[0].pane_id != bootstrap.root_pane_id || panes[0].agent.is_some() {
        return Ok(None);
    }
    let root_id = panes[0].pane_id.clone();

    // Adoption: this tab/root is now this card's tab/anchor. The tab label
    // becomes `card-<id>` and the root becomes `card-<id>-anchor`, then the
    // ordinary anchor split path supplies the run child.
    client
        .tab_rename(&TabRenameParams {
            tab_id: bootstrap.tab_id.clone(),
            label: tab_label.to_string(),
        })
        .map_err(mark_retryable_placement_race)
        .context("renaming the adopted workspace tab to the card tab")?;
    client
        .pane_rename(&PaneRenameParams {
            pane_id: root_id.clone(),
            label: anchor_label(tab_label),
        })
        .map_err(mark_retryable_placement_race)
        .context("labeling the adopted workspace root as the card-tab anchor")?;
    let owned = split_run_child(
        client,
        workspace_id,
        &bootstrap.tab_id,
        &root_id,
        cwd.as_deref(),
        env,
    )?;
    Ok(Some(owned))
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

/// Close only exact ended child panes from this card/tab before another split.
/// Active/blocked panes are left untouched even when a stale database row says
/// the run ended; foreign panes and the anchor are not candidates.
fn reclaim_prior_children(
    client: &mut HerdrClient,
    panes: &[PaneInfo],
    anchor_id: &str,
    reclaimable_ids: &[String],
    // A board-owned child to keep open even when it is ended/idle (the reuse
    // pane). It is never closed here; it is re-prompted by the launch instead.
    protect: Option<&str>,
) -> anyhow::Result<()> {
    for pane in panes.iter().filter(|pane| {
        pane.pane_id != anchor_id
            && Some(pane.pane_id.as_str()) != protect
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
