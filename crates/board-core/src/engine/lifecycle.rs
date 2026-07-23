use crate::protocol::{CardStatus, RunOutcome};

/// The lifecycle operation observed by the daemon. The enum deliberately uses
/// board-domain concepts only; Herdr events are translated at the daemon edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleAction {
    /// Explicit `board done`, the only operation that can confirm success.
    Done {
        outcome: RunOutcome,
    },
    Cancel,
    Timeout,
    PaneExited,
}

/// Whether an open run belongs to a managed built-in or a configured harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleHarness {
    BuiltIn,
    Configured,
}

/// Facts needed to decide whether one lifecycle operation may close an open
/// run. The daemon gathers these facts under its scheduler/store lock; this
/// function performs no I/O and does not inspect Herdr-specific types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleFacts {
    pub open_run_id: Option<i64>,
    pub supplied_run_id: Option<i64>,
    pub started: bool,
    pub harness: LifecycleHarness,
    pub card_status: CardStatus,
}

/// A pure description of the durable finalization work. The executor owns the
/// transaction and all process/event/notification effects; this value only
/// states the policy those effects must implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizePlan {
    pub outcome: RunOutcome,
    pub kill: bool,
    pub transition: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleRejection {
    NoOpenRun,
    SuppliedRunIdMismatch { expected: i64, supplied: i64 },
    QueuedCompletionRequiresRunId,
    QueuedBuiltinCompletion,
    PaneExitRequiresRunId,
    PaneExitBuiltin,
    TimeoutBeforeStart,
    TimeoutPaused,
}

/// The pure result of applying a lifecycle operation to the currently known
/// run facts. Rejected observations are intentionally explicit so callers do
/// not accidentally turn stale callbacks into terminal writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleDecision {
    Finalize(FinalizePlan),
    Reject(LifecycleRejection),
}

/// Decide whether a lifecycle operation is eligible to finalize its run.
pub fn decide_lifecycle(facts: &LifecycleFacts, action: LifecycleAction) -> LifecycleDecision {
    let Some(open_run_id) = facts.open_run_id else {
        return LifecycleDecision::Reject(LifecycleRejection::NoOpenRun);
    };

    if let Some(supplied) = facts.supplied_run_id {
        if supplied != open_run_id {
            return LifecycleDecision::Reject(LifecycleRejection::SuppliedRunIdMismatch {
                expected: open_run_id,
                supplied,
            });
        }
    }

    match action {
        LifecycleAction::Done { outcome } => {
            if !facts.started {
                if facts.harness == LifecycleHarness::BuiltIn {
                    return LifecycleDecision::Reject(LifecycleRejection::QueuedBuiltinCompletion);
                }
                if facts.supplied_run_id.is_none() {
                    return LifecycleDecision::Reject(
                        LifecycleRejection::QueuedCompletionRequiresRunId,
                    );
                }
            }
            LifecycleDecision::Finalize(FinalizePlan {
                outcome,
                kill: false,
                transition: true,
            })
        }
        LifecycleAction::Cancel => LifecycleDecision::Finalize(FinalizePlan {
            outcome: RunOutcome::Cancelled,
            kill: facts.started,
            transition: false,
        }),
        LifecycleAction::Timeout => {
            if !facts.started {
                LifecycleDecision::Reject(LifecycleRejection::TimeoutBeforeStart)
            } else if facts.card_status == CardStatus::Awaiting {
                LifecycleDecision::Reject(LifecycleRejection::TimeoutPaused)
            } else {
                LifecycleDecision::Finalize(FinalizePlan {
                    outcome: RunOutcome::Fail,
                    kill: true,
                    transition: true,
                })
            }
        }
        LifecycleAction::PaneExited => {
            if facts.supplied_run_id.is_none() {
                return LifecycleDecision::Reject(LifecycleRejection::PaneExitRequiresRunId);
            }
            if facts.harness == LifecycleHarness::BuiltIn {
                LifecycleDecision::Reject(LifecycleRejection::PaneExitBuiltin)
            } else {
                LifecycleDecision::Finalize(FinalizePlan {
                    outcome: RunOutcome::Fail,
                    kill: false,
                    transition: false,
                })
            }
        }
    }
}
