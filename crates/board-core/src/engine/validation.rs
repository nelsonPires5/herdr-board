use crate::capability::{capabilities_for, efforts_for};
use crate::config::Config;
use crate::model::{Card, Column};
use crate::protocol::{CardStatus, CardUpdateParams, ColumnUpdateParams, Patch, SpaceKind};

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
