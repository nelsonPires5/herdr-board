use crate::model::{Column, Comment, Run};
use crate::protocol::{CardStatus, RunOutcome, Trigger};

/// The maximum number of automatic column hops allowed before human action.
///
/// This is a domain policy, not a daemon scheduling setting: keeping it in the
/// pure engine ensures every lifecycle entry point applies the same guard.
pub const MAX_AUTO_HOPS: u32 = 8;

/// Result of applying the automatic-hop guard to a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoHopDecision {
    Continue { hop: u32 },
    Stop { message: String },
    Reset,
}

/// Decide the next automatic-hop state without mutating scheduler state.
pub fn decide_auto_hop(current_hops: u32, transition: &TransitionDecision) -> AutoHopDecision {
    if !transition.enqueue {
        return AutoHopDecision::Reset;
    }
    let hop = current_hops.saturating_add(1);
    if hop > MAX_AUTO_HOPS {
        AutoHopDecision::Stop {
            message: format!(
                "auto-chain limit ({MAX_AUTO_HOPS}) reached without human action; stopping"
            ),
        }
    } else {
        AutoHopDecision::Continue { hop }
    }
}

/// Whether the stored harness conversation is safe to resume. A session id is
/// evidence only when a started run for that id posted the run-scoped agent
/// comment; a row or an arbitrary comment alone is not proof that the harness
/// conversation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumabilityDecision {
    Resumable,
    Fresh,
}

pub fn decide_resumability(
    session_id: Option<&str>,
    runs: &[Run],
    comments: &[Comment],
) -> ResumabilityDecision {
    let Some(session_id) = session_id else {
        return ResumabilityDecision::Fresh;
    };
    let resumable = runs.iter().any(|run| {
        run.started_at.is_some()
            && run.session_id.as_deref() == Some(session_id)
            && comments
                .iter()
                .any(|comment| comment.author == format!("agent:{}", run.id))
    });
    if resumable {
        ResumabilityDecision::Resumable
    } else {
        ResumabilityDecision::Fresh
    }
}

/// Outcome of applying a finished run's transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionDecision {
    /// Column to move the card into, or `None` to stay put.
    pub target_column_id: Option<i64>,
    /// Card status after the transition.
    pub new_status: CardStatus,
    /// Whether entering the target column should enqueue a fresh run.
    pub enqueue: bool,
    /// System comment to post recording the transition.
    pub system_comment: String,
}

/// Decide the transition for a finished run.
///
/// `ok` → `on_success`, `fail` → `on_fail`; `cancelled`/`lost` never transition.
/// No target column → the card stays (status `done` for ok — completion was
/// confirmed via `board done ok` — `failed` otherwise).
pub fn decide_transition(
    current: &Column,
    columns: &[Column],
    outcome: RunOutcome,
    elapsed_secs: Option<i64>,
) -> TransitionDecision {
    let target_id = match outcome {
        RunOutcome::Ok => current.on_success_column_id,
        RunOutcome::Fail => current.on_fail_column_id,
        RunOutcome::Cancelled | RunOutcome::Lost => None,
    };
    let target = target_id.and_then(|id| columns.iter().find(|c| c.id == id));
    let dur = format_duration(elapsed_secs);
    let word = outcome_word(outcome);

    match target {
        Some(t) => {
            let enqueue = t.trigger == Trigger::Auto;
            let new_status = if enqueue {
                CardStatus::Queued
            } else {
                CardStatus::Idle
            };
            TransitionDecision {
                target_column_id: Some(t.id),
                new_status,
                enqueue,
                system_comment: format!("{} {} in {} → {}", current.name, word, dur, t.name),
            }
        }
        None => {
            let new_status = match outcome {
                RunOutcome::Ok => CardStatus::Done,
                _ => CardStatus::Failed,
            };
            TransitionDecision {
                target_column_id: None,
                new_status,
                enqueue: false,
                system_comment: format!(
                    "{} {} in {} (no target column, staying)",
                    current.name, word, dur
                ),
            }
        }
    }
}

/// What happens when a card enters a column (via move or create).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryDecision {
    pub new_status: CardStatus,
    pub enqueue: bool,
    /// Fire a herdr notification (manual column reached via an auto-transition).
    pub notify: bool,
}

/// Decide what entering `target` does for a card in `status_before`.
///
/// Auto column + idle/failed/done card → enqueue (`queued`) — a `done` card
/// moved into an auto column is re-dispatched, like a failed one. Manual
/// column → `idle`, notifying only when the entry came from an
/// auto-transition rather than a human.
pub fn decide_entry(
    target: &Column,
    status_before: CardStatus,
    via_auto_transition: bool,
) -> EntryDecision {
    match target.trigger {
        Trigger::Auto => {
            let dispatchable = matches!(
                status_before,
                CardStatus::Idle | CardStatus::Failed | CardStatus::Done
            );
            if dispatchable {
                EntryDecision {
                    new_status: CardStatus::Queued,
                    enqueue: true,
                    notify: false,
                }
            } else {
                // Card is already busy; don't double-dispatch (guarded upstream too).
                EntryDecision {
                    new_status: status_before,
                    enqueue: false,
                    notify: false,
                }
            }
        }
        Trigger::Manual => EntryDecision {
            new_status: CardStatus::Idle,
            enqueue: false,
            notify: via_auto_transition,
        },
    }
}

fn outcome_word(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Ok => "ok",
        RunOutcome::Fail => "failed",
        RunOutcome::Cancelled => "cancelled",
        RunOutcome::Lost => "lost",
    }
}

/// Human-readable elapsed time, e.g. `4m12s`, `42s`, `1h3m`.
pub fn format_duration(secs: Option<i64>) -> String {
    match secs {
        None => "unknown".to_string(),
        Some(s) if s <= 0 => "0s".to_string(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m{}s", s / 60, s % 60),
        Some(s) => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}
