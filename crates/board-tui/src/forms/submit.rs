//! Submit conversion and value extraction helpers.

use board_core::engine::{
    validate_card_space, validate_column_permission_override, ValidationError,
};
use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, Effort, Patch,
    SpaceKind, Trigger,
};

use super::{ChoiceVal, FieldId, FieldKind, Form, FormKind, Submit};

/// Render a core validation failure as the toast text a submit returns.
fn err(e: ValidationError) -> String {
    e.to_string()
}

impl Form {
    // -- submit --------------------------------------------------------------

    /// Turn the current field values into params, or an error message to toast.
    ///
    /// Before building params this runs the **core** validators the daemon
    /// would run (`validate_card_space`, `validate_column_permission_override`)
    /// as a pre-flight, so a doomed request is caught in the open form instead
    /// of after a failed round-trip. The daemon stays authoritative — this
    /// never approves anything, it only declines earlier.
    pub fn submit(&self) -> Result<Submit, String> {
        self.preflight().map_err(err)?;
        match self.kind {
            FormKind::CardCreate { column_id } => {
                let title = self.trim(FieldId::Title);
                if title.is_empty() {
                    return Err("title is required".into());
                }
                Ok(Submit::CardCreate(CardCreateParams {
                    title,
                    board_id: None,
                    description: self.opt_text(FieldId::Description),
                    column_id: Some(column_id),
                    harness: self.opt_choice_str(FieldId::Harness),
                    model: self.card_model(),
                    effort: self.opt_effort(FieldId::Effort),
                    permission_mode: self
                        .permission_is_applicable()
                        .then(|| self.opt_choice_str(FieldId::Permission))
                        .flatten(),
                    session: self.current_session(),
                    space_kind: self.opt_space_kind(),
                    space_ref: self.card_space_ref(),
                    space_cwd: self.new_workspace_cwd(),
                    position: None,
                }))
            }
            FormKind::CardEdit { card_id } => {
                let title = self.trim(FieldId::Title);
                if title.is_empty() {
                    return Err("title is required".into());
                }
                Ok(Submit::CardUpdate(CardUpdateParams {
                    id: card_id,
                    title: Some(title),
                    description: Some(self.trim(FieldId::Description)),
                    harness: self.opt_choice_str(FieldId::Harness),
                    model: Patch::from_option(self.card_model()),
                    effort: Patch::from_option(self.opt_effort(FieldId::Effort)),
                    permission_mode: Patch::from_option(
                        self.permission_is_applicable()
                            .then(|| self.opt_choice_str(FieldId::Permission))
                            .flatten(),
                    ),
                    session: Patch::from_option(self.current_session()),
                    space_kind: self.opt_space_kind(),
                    space_ref: Patch::from_option(self.card_space_ref()),
                    space_cwd: Patch::from_option(self.new_workspace_cwd()),
                }))
            }
            FormKind::ColumnCreate => {
                let name = self.trim(FieldId::Name);
                if name.is_empty() {
                    return Err("name is required".into());
                }
                Ok(Submit::ColumnCreate(ColumnCreateParams {
                    name,
                    board_id: None,
                    position: None,
                    system_prompt: self.opt_text(FieldId::SystemPrompt),
                    trigger: self.opt_trigger(),
                    on_success_column_id: self.opt_col(FieldId::OnSuccess),
                    on_fail_column_id: self.opt_col(FieldId::OnFail),
                    fresh_session: self.opt_bool(FieldId::FreshSession),
                    harness_override: self.opt_str_field(FieldId::HarnessOverride),
                    model_override: self.opt_text(FieldId::ModelOverride),
                    effort_override: self.opt_str_field(FieldId::EffortOverride),
                    permission_override: self.opt_str_field(FieldId::PermissionOverride),
                    timeout_minutes: self.opt_int(FieldId::Timeout),
                }))
            }
            FormKind::ColumnEdit { column_id } => {
                let name = self.trim(FieldId::Name);
                if name.is_empty() {
                    return Err("name is required".into());
                }
                Ok(Submit::ColumnUpdate(ColumnUpdateParams {
                    id: column_id,
                    name: Some(name),
                    position: None,
                    system_prompt: Patch::from_option(self.opt_text(FieldId::SystemPrompt)),
                    trigger: self.opt_trigger(),
                    on_success_column_id: Patch::from_option(self.opt_col(FieldId::OnSuccess)),
                    on_fail_column_id: Patch::from_option(self.opt_col(FieldId::OnFail)),
                    fresh_session: self.opt_bool(FieldId::FreshSession),
                    harness_override: Patch::from_option(
                        self.opt_str_field(FieldId::HarnessOverride),
                    ),
                    model_override: Patch::from_option(self.opt_text(FieldId::ModelOverride)),
                    effort_override: Patch::from_option(
                        self.opt_str_field(FieldId::EffortOverride),
                    ),
                    permission_override: Patch::from_option(
                        self.opt_str_field(FieldId::PermissionOverride),
                    ),
                    timeout_minutes: Patch::from_option(self.opt_int(FieldId::Timeout)),
                }))
            }
            FormKind::Comment { card_id } => {
                let body = self.trim(FieldId::CommentBody);
                if body.is_empty() {
                    return Err("comment is empty".into());
                }
                Ok(Submit::Comment { card_id, body })
            }
            FormKind::CommentEdit { comment_id } => {
                let body = self.trim(FieldId::CommentBody);
                if body.is_empty() {
                    return Err("comment is empty".into());
                }
                Ok(Submit::CommentEdit { comment_id, body })
            }
        }
    }

    /// The core validators that apply to this form's shape. Deliberately a
    /// subset: only rules that are *decidable* from the form's own values.
    /// Anything needing board/daemon state (busy cards, orphaned overrides)
    /// stays the daemon's job.
    fn preflight(&self) -> Result<(), ValidationError> {
        match self.kind {
            FormKind::CardCreate { .. } | FormKind::CardEdit { .. } => validate_card_space(
                self.opt_space_kind().unwrap_or(SpaceKind::Workspace),
                self.card_space_ref().as_deref(),
                self.new_workspace_cwd().as_deref(),
            ),
            FormKind::ColumnCreate | FormKind::ColumnEdit { .. } => {
                validate_column_permission_override(
                    self.opt_str_field(FieldId::PermissionOverride).as_deref(),
                )
            }
            FormKind::Comment { .. } | FormKind::CommentEdit { .. } => Ok(()),
        }
    }

    // -- extraction helpers --------------------------------------------------

    fn trim(&self, id: FieldId) -> String {
        self.field(id)
            .map(|f| f.get_text())
            .unwrap_or_default()
            .trim()
            .to_string()
    }
    pub(super) fn opt_text(&self, id: FieldId) -> Option<String> {
        let s = self.trim(id);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    pub(super) fn opt_choice_str(&self, id: FieldId) -> Option<String> {
        match self.field(id).and_then(|f| f.choice_val()) {
            Some(ChoiceVal::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }
    /// Effective string for a field that may be a choice (default/none → `None`)
    /// or free text (fallback when no catalog is loaded). Used by the column
    /// override fields, which are choices once caps arrive but text otherwise.
    pub(super) fn opt_str_field(&self, id: FieldId) -> Option<String> {
        match self.field(id).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => self.opt_choice_str(id),
            _ => self.opt_text(id),
        }
    }
    pub(super) fn opt_col(&self, id: FieldId) -> Option<i64> {
        match self.field(id).and_then(|f| f.choice_val()) {
            Some(ChoiceVal::Col(c)) => Some(*c),
            _ => None,
        }
    }
    pub(super) fn opt_bool(&self, id: FieldId) -> Option<bool> {
        match self.field(id).and_then(|f| f.choice_val()) {
            Some(ChoiceVal::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    fn opt_effort(&self, id: FieldId) -> Option<Effort> {
        self.opt_choice_str(id).and_then(|s| Effort::parse_str(&s))
    }
    fn opt_trigger(&self) -> Option<Trigger> {
        self.opt_choice_str(FieldId::Trigger)
            .and_then(|s| Trigger::parse_str(&s))
    }
    fn opt_space_kind(&self) -> Option<SpaceKind> {
        self.opt_choice_str(FieldId::SpaceKind)
            .and_then(|s| SpaceKind::parse_str(&s))
    }
    /// The `cwd` text, only for a `new_workspace` space (else `None`).
    fn new_workspace_cwd(&self) -> Option<String> {
        if self.space_kind_is_new_workspace() {
            self.opt_text(FieldId::SpaceCwd)
        } else {
            None
        }
    }
    pub(super) fn opt_int(&self, id: FieldId) -> Option<i64> {
        self.opt_text(id).and_then(|s| s.parse().ok())
    }
}
