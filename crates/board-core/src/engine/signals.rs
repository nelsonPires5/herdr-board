use crate::protocol::{AwaitingReason, CardStatus};

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
