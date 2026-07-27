//! What a card tab's Herdr identity can be proved from after a restart.
//!
//! Board ownership of a tab is never inferred from its label: only an exact
//! pane id persisted on one of the card's own durable runs counts.

use board_core::model::Run;
use board_herdr::SessionSnapshot;

/// Reconstruct a card tab's ownership from a durable board-owned pane id.
/// A matching pane is proof of ownership; tab labels alone are never used.
pub(crate) fn reconstruct_owned_tab_id(
    snapshot: &SessionSnapshot,
    workspace_id: &str,
    prior_pane_ids: &[String],
) -> Option<String> {
    // `prior_pane_ids` is ordered by newest durable run first. Herdr snapshot
    // ordering is not an ownership or recency signal, so never let it decide
    // which exact board-owned pane wins an otherwise valid reconstruction.
    prior_pane_ids.iter().find_map(|pane_id| {
        snapshot
            .panes
            .iter()
            .find(|pane| {
                pane.pane_id == *pane_id
                    && pane.workspace_id == workspace_id
                    && !pane.tab_id.is_empty()
            })
            .map(|pane| pane.tab_id.clone())
    })
}

/// Which persisted pane identity to collect from a card's runs.
pub(crate) enum OwnedPanes {
    /// Every durable run child in scope. These prove the tab after a daemon
    /// restart and authorize recovering a missing anchor.
    DurableChildren,
    /// Durable run children of *ended* runs — the only panes eligible for
    /// geometry reclamation. Queued/open rows are intentionally excluded;
    /// placement performs a second live status check before closing any pane.
    ReclaimableChildren,
    /// Durable card-tab anchors. v11 rows have NULL here by design and
    /// therefore cannot prove an anchor after restart.
    Anchors,
}

/// Return the exact persisted pane ids of `selector`, newest run first.
///
/// Only durable protocol-v12 launch rows for the selected session and workspace
/// are eligible; legacy rows and panes from another placement scope cannot
/// confer tab ownership. Newest-first keeps reconstruction deterministic when
/// old runs occupy different tabs.
pub(crate) fn owned_pane_ids(
    runs: &[Run],
    session: Option<&str>,
    workspace_id: &str,
    selector: OwnedPanes,
) -> Vec<String> {
    let mut owned = runs
        .iter()
        .filter(|run| {
            run.launch_spec.is_some()
                && run.session.as_deref() == session
                && run.herdr_workspace_id.as_deref() == Some(workspace_id)
                && (!matches!(selector, OwnedPanes::ReclaimableChildren) || run.ended_at.is_some())
        })
        .filter_map(|run| {
            let pane = match selector {
                OwnedPanes::Anchors => run.herdr_anchor_pane_id.as_deref(),
                OwnedPanes::DurableChildren | OwnedPanes::ReclaimableChildren => {
                    run.herdr_pane_id.as_deref()
                }
            };
            pane.filter(|pane| !pane.is_empty())
                .map(|pane| (run.id, pane.to_string()))
        })
        .collect::<Vec<_>>();
    owned.sort_unstable_by_key(|(run_id, _)| std::cmp::Reverse(*run_id));
    owned.into_iter().map(|(_, pane)| pane).collect()
}
