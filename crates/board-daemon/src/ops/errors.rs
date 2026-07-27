//! Engine-decision adapters for the run handlers.
//!
//! The pure `board_core::engine` lifecycle decider knows nothing about the
//! protocol error codes, so the translation both ways lives here rather than
//! inside the handlers.

use board_core::engine::{LifecycleFacts, LifecycleHarness, LifecycleRejection};
use board_core::harness::is_builtin_harness;
use board_core::model::{Card, Run};
use board_core::Error;

pub(super) fn lifecycle_facts(
    run: &Run,
    card: &Card,
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

pub(super) fn lifecycle_rejection(card_id: i64, rejection: LifecycleRejection) -> Error {
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
