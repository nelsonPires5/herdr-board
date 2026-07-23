//! The column engine: pure, synchronous, no I/O. Given the current world it
//! returns *decisions* (target column, new statuses, system-comment text,
//! validation verdicts). The daemon executes the resulting effects.

use crate::capability::{capabilities_for, efforts_for};
use crate::config::Config;
use crate::model::{Card, Column, Comment, Run};
use crate::protocol::{
    AwaitingReason, CardStatus, CardUpdateParams, ColumnUpdateParams, Patch, RunOutcome, SpaceKind,
    Trigger,
};

/// Validation failures that map onto protocol error code 3 (invalid state) or 1.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("column has cards; specify move_cards_to")]
    ColumnHasCards,
    #[error("column has a card with an open run")]
    ColumnHasActiveCard,
    #[error("card has an open run; cannot edit harness/space fields")]
    CardBusy,
    #[error("card has an open run; cancel it before archiving")]
    CardHasActiveRun,
    #[error("bypassPermissions is only allowed as an explicit per-card setting, never a column override")]
    BypassNotAllowed,
    #[error("new_workspace space requires a non-empty space_ref (label) and space_cwd")]
    NewWorkspaceIncomplete,
    #[error("unknown harness '{0}'")]
    UnknownHarness(String),
    #[error("model '{0}' is not accepted by harness")]
    InvalidModel(String),
    #[error("effort '{0}' is not accepted by harness/model")]
    InvalidEffort(String),
    #[error("pi does not support permission modes")]
    PiPermissionUnsupported,
    #[error("permission mode '{0}' is not accepted by harness")]
    InvalidPermission(String),
    #[error("column override depends on an explicit harness override")]
    OrphanedColumnOverride,
}

impl ValidationError {
    pub fn code(&self) -> i32 {
        match self {
            ValidationError::ColumnHasCards
            | ValidationError::ColumnHasActiveCard
            | ValidationError::CardBusy
            | ValidationError::CardHasActiveRun => 3,
            ValidationError::BypassNotAllowed
            | ValidationError::NewWorkspaceIncomplete
            | ValidationError::UnknownHarness(_)
            | ValidationError::InvalidModel(_)
            | ValidationError::InvalidEffort(_)
            | ValidationError::PiPermissionUnsupported
            | ValidationError::InvalidPermission(_)
            | ValidationError::OrphanedColumnOverride => 1,
        }
    }
}

/// The maximum number of automatic column hops allowed before human action.
///
/// This is a domain policy, not a daemon scheduling setting: keeping it in the
/// pure engine ensures every lifecycle entry point applies the same guard.
pub const MAX_AUTO_HOPS: u32 = 8;

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
/// harness/model/effort/permission/session/space fields, which are frozen while
/// a run is open.
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

/// Whether a permission value belongs to a card or a column override.
/// `bypassPermissions` is a deliberate per-card opt-in and is never allowed
/// in a column, where it would silently apply to every card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionContext {
    Card,
    ColumnOverride,
}

/// Apply a partial card update without performing validation or I/O. The
/// returned value is the only value that callers should validate or persist.
pub fn merge_card_update(current: &Card, update: &CardUpdateParams) -> Card {
    let mut merged = current.clone();
    if let Some(value) = &update.title {
        merged.title = value.clone();
    }
    if let Some(value) = &update.description {
        merged.description = value.clone();
    }
    if let Some(value) = &update.harness {
        merged.harness = value.clone();
    }
    apply_patch(&mut merged.model, &update.model);
    apply_patch(&mut merged.effort, &update.effort);
    apply_patch(&mut merged.permission_mode, &update.permission_mode);
    apply_patch(&mut merged.session, &update.session);
    if let Some(value) = update.space_kind {
        merged.space_kind = value;
    }
    apply_patch(&mut merged.space_ref, &update.space_ref);
    apply_patch(&mut merged.space_cwd, &update.space_cwd);
    merged
}

/// Apply a partial column update without validation or I/O.
pub fn merge_column_update(current: &Column, update: &ColumnUpdateParams) -> Column {
    let mut merged = current.clone();
    if let Some(value) = &update.name {
        merged.name = value.clone();
    }
    apply_patch(&mut merged.system_prompt, &update.system_prompt);
    if let Some(value) = update.trigger {
        merged.trigger = value;
    }
    apply_patch(
        &mut merged.on_success_column_id,
        &update.on_success_column_id,
    );
    apply_patch(&mut merged.on_fail_column_id, &update.on_fail_column_id);
    if let Some(value) = update.fresh_session {
        merged.fresh_session = value;
    }
    apply_patch(&mut merged.harness_override, &update.harness_override);
    apply_patch(&mut merged.model_override, &update.model_override);
    apply_patch(&mut merged.effort_override, &update.effort_override);
    apply_patch(&mut merged.permission_override, &update.permission_override);
    apply_patch(&mut merged.timeout_minutes, &update.timeout_minutes);
    merged
}

fn apply_patch<T: Clone>(target: &mut Option<T>, patch: &Patch<T>) {
    match patch {
        Patch::Unchanged => {}
        Patch::Clear => *target = None,
        Patch::Set(value) => *target = Some(value.clone()),
    }
}

/// Validate all card settings together. This is intentionally independent of
/// persistence so daemon, CLI, and tests cannot validate individual fields
/// against stale values.
pub fn validate_card_settings(card: &Card, config: &Config) -> Result<(), ValidationError> {
    validate_card_values(
        &card.harness,
        card.model.as_deref(),
        card.effort,
        card.permission_mode.as_deref(),
        card.space_kind,
        card.space_ref.as_deref(),
        card.space_cwd.as_deref(),
        config,
    )
}

/// Validate card settings before a card row exists (for `card.create`).
#[allow(clippy::too_many_arguments)]
pub fn validate_card_values(
    harness: &str,
    model: Option<&str>,
    effort: Option<crate::protocol::Effort>,
    permission: Option<&str>,
    space_kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
    config: &Config,
) -> Result<(), ValidationError> {
    validate_settings_values(
        harness,
        model,
        effort,
        permission,
        config,
        PermissionContext::Card,
    )?;
    validate_card_space(space_kind, space_ref, space_cwd)
}

/// Validate a complete column override. Dependent overrides without an
/// explicit harness are allowed: they apply to the entering card harness and
/// are checked again by [`validate_effective_settings`].
pub fn validate_column_settings(
    column: &Column,
    config: &Config,
    context: PermissionContext,
) -> Result<(), ValidationError> {
    validate_column_values(
        column.harness_override.as_deref(),
        column.model_override.as_deref(),
        column.effort_override.as_deref(),
        column.permission_override.as_deref(),
        config,
        context,
    )
}

/// Validate the transition from an existing column to a merged update. A
/// harness clear may not leave dependent override fields behind unless those
/// fields are explicitly cleared in the same request.
pub fn validate_column_update(
    current: &Column,
    merged: &Column,
    update: &ColumnUpdateParams,
    config: &Config,
) -> Result<(), ValidationError> {
    if current.harness_override.is_some()
        && merged.harness_override.is_none()
        && (merged.model_override.is_some()
            || merged.effort_override.is_some()
            || merged.permission_override.is_some())
        && (update.model_override.is_unchanged()
            || update.effort_override.is_unchanged()
            || update.permission_override.is_unchanged())
    {
        return Err(ValidationError::OrphanedColumnOverride);
    }
    validate_column_settings(merged, config, PermissionContext::ColumnOverride)
}

/// Validate column overrides before a column row exists (for `column.create`).
pub fn validate_column_values(
    harness_override: Option<&str>,
    model_override: Option<&str>,
    effort_override: Option<&str>,
    permission_override: Option<&str>,
    config: &Config,
    context: PermissionContext,
) -> Result<(), ValidationError> {
    if context == PermissionContext::ColumnOverride {
        validate_column_permission_override(permission_override)?;
    }
    let parsed_effort = match effort_override {
        Some(value) => Some(
            crate::protocol::Effort::parse_str(value)
                .ok_or_else(|| ValidationError::InvalidEffort(value.to_string()))?,
        ),
        None => None,
    };
    if let Some(harness) = harness_override {
        validate_settings_values(
            harness,
            model_override,
            parsed_effort,
            permission_override,
            config,
            context,
        )?;
    }
    Ok(())
}

/// Revalidate the complete effective card + column pair immediately before a
/// run is enqueued. Stored data may predate the capability validator, so this
/// check must not be replaced by update-time checks alone.
pub fn validate_effective_settings(
    card: &Card,
    column: &Column,
    config: &Config,
) -> Result<(), ValidationError> {
    // Validate only the resolved settings here. A legacy card can contain a
    // value that is invalid for its old harness but fully replaced by column
    // overrides; rejecting that stale base would not validate the run that is
    // actually about to be enqueued. With no override, the effective values
    // are the card values and receive the same validation.
    // A column may intentionally provide a model/effort override for the
    // entering card's harness without naming a harness itself. Validate that
    // effective pair below; only reject bypassPermissions at the column
    // boundary.
    validate_column_permission_override(column.permission_override.as_deref())?;
    let harness = column
        .harness_override
        .as_deref()
        .unwrap_or(card.harness.as_str());
    let model = column.model_override.as_deref().or(card.model.as_deref());
    let effort = match column.effort_override.as_deref() {
        Some(value) => Some(
            crate::protocol::Effort::parse_str(value)
                .ok_or_else(|| ValidationError::InvalidEffort(value.to_string()))?,
        ),
        None => card.effort,
    };
    let permission = column
        .permission_override
        .as_deref()
        .or(card.permission_mode.as_deref());
    validate_settings_values(
        harness,
        model,
        effort,
        permission,
        config,
        PermissionContext::Card,
    )?;
    validate_card_space(
        card.space_kind,
        card.space_ref.as_deref(),
        card.space_cwd.as_deref(),
    )
}

fn validate_settings_values(
    harness: &str,
    model: Option<&str>,
    effort: Option<crate::protocol::Effort>,
    permission: Option<&str>,
    config: &Config,
    permission_context: PermissionContext,
) -> Result<(), ValidationError> {
    let caps = capabilities_for(harness, config)
        .ok_or_else(|| ValidationError::UnknownHarness(harness.to_string()))?;
    if let Some(model) = model {
        if !caps.model_freeform && !caps.models.iter().any(|known| known.id == model) {
            return Err(ValidationError::InvalidModel(model.to_string()));
        }
    }
    if let Some(effort) = effort {
        if !efforts_for(&caps, model).contains(&effort) {
            return Err(ValidationError::InvalidEffort(effort.to_string()));
        }
    }
    if let Some(permission) = permission {
        if caps.permission_modes.is_empty() {
            return Err(ValidationError::PiPermissionUnsupported);
        }
        if permission_context == PermissionContext::ColumnOverride
            && permission == "bypassPermissions"
        {
            return Err(ValidationError::BypassNotAllowed);
        }
        if !caps.permission_modes.iter().any(|mode| mode == permission) {
            return Err(ValidationError::InvalidPermission(permission.to_string()));
        }
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
