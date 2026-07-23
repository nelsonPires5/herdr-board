use std::collections::BTreeMap;
use std::path::Path;

use board_herdr::{
    HerdrClient, HerdrError, LayoutPane, PaneSplitParams, SplitDirection, TabCreateParams,
};

pub(crate) const ERR_PANE_NOT_FOUND: &str = "pane_not_found";
pub(crate) const ERR_EMPTY_TAB: &str = "empty_tab";
pub(crate) const ERR_EMPTY_LAYOUT: &str = "empty_layout";

#[derive(Debug)]
pub(crate) struct OwnedPane {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
}

/// Find/create the board tab, then consume its root pane or split an explicitly
/// selected existing pane. The caller owns the single bounded full-placement
/// retry, so a race at any discovery step restarts from `tab.list`.
pub(crate) fn allocate_owned_pane(
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
        });
    };

    let panes: Vec<_> = client
        .pane_list(Some(workspace_id))
        .map_err(mark_retryable_placement_race)?
        .into_iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .collect();
    let anchor = panes.first().ok_or_else(|| {
        mark_retryable_placement_race(HerdrError::Protocol {
            code: ERR_EMPTY_TAB.to_string(),
            message: format!("existing tab {} has no pane available to split", tab.tab_id),
        })
    })?;
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
