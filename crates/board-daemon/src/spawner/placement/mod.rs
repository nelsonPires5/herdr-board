//! Board-owned pane placement, split into the three concerns it had grown:
//! [`alloc`] talks to Herdr, [`geometry`] is pure sizing, and [`race`] owns the
//! retry taxonomy plus the cleanup that follows a failed placement.

mod alloc;
mod geometry;
mod race;

#[cfg(test)]
pub(crate) use alloc::grid_slot;
pub(crate) use alloc::{allocate_owned_pane, CardOwnership, OwnedPane};
#[cfg(test)]
pub(crate) use geometry::{
    initial_split_geometry, recovery_target_ratio, split_geometry, ANCHOR_RATIO,
    ERR_ANCHOR_TOO_SMALL,
};
pub(crate) use race::{
    close_owned_after_error, close_owned_for_retry, is_pane_not_found, is_retryable_placement_race,
    mark_retryable_placement_race, mark_retryable_runner_race, RetryablePlacementRace,
    ERR_PANE_NOT_FOUND,
};
