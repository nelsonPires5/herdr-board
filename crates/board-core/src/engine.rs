//! The column engine: pure, synchronous, no I/O. Given the current world it
//! returns *decisions* (target column, new statuses, system-comment text,
//! validation verdicts). The daemon executes the resulting effects.

use crate::model::Column;
use crate::protocol::{AwaitingReason, CardStatus, RunOutcome, SpaceKind, Trigger};

/// Validation failures that map onto protocol error code 3 (invalid state) or 1.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("column has cards; specify move_cards_to")]
    ColumnHasCards,
    #[error("column has a card with an open run")]
    ColumnHasActiveCard,
    #[error("card has an open run; cannot edit harness/space fields")]
    CardBusy,
    #[error("card has an active run; cancel it before archiving")]
    CardHasActiveRun,
    #[error("bypassPermissions is only allowed as an explicit per-card setting, never a column override")]
    BypassNotAllowed,
    #[error("new_workspace space requires a non-empty space_ref (label) and space_cwd")]
    NewWorkspaceIncomplete,
}

impl ValidationError {
    pub fn code(&self) -> i32 {
        match self {
            ValidationError::ColumnHasCards
            | ValidationError::ColumnHasActiveCard
            | ValidationError::CardBusy
            | ValidationError::CardHasActiveRun => 3,
            ValidationError::BypassNotAllowed | ValidationError::NewWorkspaceIncomplete => 1,
        }
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

/// Validate a `column.delete`.
pub fn validate_column_delete(
    has_cards: bool,
    has_active_card: bool,
    move_cards_to: Option<i64>,
) -> Result<(), ValidationError> {
    if has_active_card {
        return Err(ValidationError::ColumnHasActiveCard);
    }
    if has_cards && move_cards_to.is_none() {
        return Err(ValidationError::ColumnHasCards);
    }
    Ok(())
}

/// Validate a `card.update`. `edits_locked_fields` is true when the patch touches
/// harness/model/effort/permission/space fields, which are frozen while busy.
pub fn validate_card_edit(
    status: CardStatus,
    edits_locked_fields: bool,
) -> Result<(), ValidationError> {
    if edits_locked_fields
        && matches!(
            status,
            CardStatus::Running | CardStatus::Queued | CardStatus::Blocked | CardStatus::Awaiting
        )
    {
        return Err(ValidationError::CardBusy);
    }
    Ok(())
}

/// Archive is allowed only when no run can still be active or waiting.
/// `awaiting` keeps its run OPEN (pending human review), so it rejects too.
pub fn validate_card_archive(status: CardStatus) -> Result<(), ValidationError> {
    if matches!(
        status,
        CardStatus::Queued | CardStatus::Running | CardStatus::Blocked | CardStatus::Awaiting
    ) {
        return Err(ValidationError::CardHasActiveRun);
    }
    Ok(())
}

/// Validate a card's space configuration at `card.create`. A `new_workspace`
/// space needs both a label (`space_ref`) and a working directory (`space_cwd`);
/// a plain `workspace` space has no such requirement here (an empty ref is
/// resolved/errored at dispatch).
pub fn validate_card_space(
    kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
) -> Result<(), ValidationError> {
    if kind == SpaceKind::NewWorkspace {
        let ref_ok = space_ref.is_some_and(|s| !s.trim().is_empty());
        let cwd_ok = space_cwd.is_some_and(|s| !s.trim().is_empty());
        if !ref_ok || !cwd_ok {
            return Err(ValidationError::NewWorkspaceIncomplete);
        }
    }
    Ok(())
}

/// Reject `bypassPermissions` supplied as a column override.
pub fn validate_column_permission_override(perm: Option<&str>) -> Result<(), ValidationError> {
    if perm == Some("bypassPermissions") {
        return Err(ValidationError::BypassNotAllowed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent signals
// ---------------------------------------------------------------------------

/// A signal from the agent's pane, as observed by the daemon's watchers.
/// herdr's agent status is a HINT — `board done` remains the only terminal
/// success truth, so no signal ever finalizes a run with `ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSignal {
    /// herdr reported `working`.
    Working,
    /// herdr reported `blocked`.
    Blocked,
    /// herdr reported `done` while the run is still active (no `board done`).
    Done,
    /// herdr `idle` sustained past `idle_grace_seconds` (no `board done`).
    IdleExpired,
}

/// The engine's decision for an [`AgentSignal`]: apply via a single DB write
/// (`set_card_awaiting` when `awaiting_reason` is `Some`, `set_card_status`
/// otherwise, which clears the reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDecision {
    /// Card status after the signal.
    pub new_status: CardStatus,
    /// `Some` only when entering/staying in `awaiting`.
    pub awaiting_reason: Option<AwaitingReason>,
    /// herdr notification to fire, if any.
    pub emit_notification: Option<String>,
}

/// Map an agent signal onto a card-status decision. Pure: no DB, no clock.
///
/// Returns `None` for stale/no-op signals so the caller writes nothing:
/// - Signals only apply while a run can be active (`running`/`blocked`/
///   `awaiting`); anything else is stale and ignored.
/// - `working` on `running` and `blocked` on `blocked` are no-ops.
/// - `idle_expired` on an already-`awaiting` card keeps the existing (more
///   specific) reason — no-op.
/// - `done` on an already-`awaiting` card REFRESHES the reason to
///   `agent_done` (an explicit done supersedes `idle_expired`) without
///   re-notifying.
pub fn decide_signal(card_status: CardStatus, signal: AgentSignal) -> Option<SignalDecision> {
    let live = matches!(
        card_status,
        CardStatus::Running | CardStatus::Blocked | CardStatus::Awaiting
    );
    if !live {
        return None;
    }
    match signal {
        AgentSignal::Working => {
            if card_status == CardStatus::Running {
                None
            } else {
                // Back to work (e.g. human gave feedback in the pane): resume
                // running, clearing blocked/awaiting state.
                Some(SignalDecision {
                    new_status: CardStatus::Running,
                    awaiting_reason: None,
                    emit_notification: None,
                })
            }
        }
        AgentSignal::Blocked => {
            if card_status == CardStatus::Blocked {
                None
            } else {
                Some(SignalDecision {
                    new_status: CardStatus::Blocked,
                    awaiting_reason: None,
                    emit_notification: Some("is blocked (needs input)".to_string()),
                })
            }
        }
        AgentSignal::Done => Some(SignalDecision {
            new_status: CardStatus::Awaiting,
            awaiting_reason: Some(AwaitingReason::AgentDone),
            emit_notification: if card_status == CardStatus::Awaiting {
                None
            } else {
                Some("agent finished without `board done`; card is awaiting review".to_string())
            },
        }),
        AgentSignal::IdleExpired => {
            if card_status == CardStatus::Awaiting {
                None
            } else {
                Some(SignalDecision {
                    new_status: CardStatus::Awaiting,
                    awaiting_reason: Some(AwaitingReason::IdleExpired),
                    emit_notification: Some(
                        "agent idle past the grace period without `board done`; \
                         card is awaiting review"
                            .to_string(),
                    ),
                })
            }
        }
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
