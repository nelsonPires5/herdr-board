//! Placement retry taxonomy and the cleanup that follows a failed placement.
//!
//! A pane/tab that disappears mid-placement is a race the caller can safely
//! restart; anything else is terminal. Keeping the marker types here keeps that
//! decision out of both the allocator and the spawner.

use board_herdr::{HerdrClient, HerdrError};

pub(crate) const ERR_PANE_NOT_FOUND: &str = "pane_not_found";
pub(crate) const ERR_EMPTY_TAB: &str = "empty_tab";
pub(crate) const ERR_EMPTY_LAYOUT: &str = "empty_layout";

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

/// A fresh card tab has no durable ownership yet. If any later placement step
/// fails, close only its newly-created root; closing that root also removes the
/// otherwise empty tab. A race that already removed it is success.
pub(super) fn cleanup_new_card_tab(
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
