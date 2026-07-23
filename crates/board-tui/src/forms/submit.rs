//! Submit conversion and value extraction helpers.

use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, Effort, Patch,
    SpaceKind, Trigger,
};

use super::{ChoiceVal, FieldId, FieldKind, Form, FormKind, Submit};

impl Form {
    // -- submit --------------------------------------------------------------

    /// Turn the current field values into params, or an error message to toast.
    pub fn submit(&self) -> Result<Submit, String> {
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
                    permission_mode: (self.current_harness() != "pi")
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
                    model: patch(self.card_model()),
                    effort: patch(self.opt_effort(FieldId::Effort)),
                    permission_mode: patch(
                        (self.current_harness() != "pi")
                            .then(|| self.opt_choice_str(FieldId::Permission))
                            .flatten(),
                    ),
                    session: patch(self.current_session()),
                    space_kind: self.opt_space_kind(),
                    space_ref: patch(self.card_space_ref()),
                    space_cwd: patch(self.new_workspace_cwd()),
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
                    system_prompt: patch(self.opt_text(FieldId::SystemPrompt)),
                    trigger: self.opt_trigger(),
                    on_success_column_id: patch(self.opt_col(FieldId::OnSuccess)),
                    on_fail_column_id: patch(self.opt_col(FieldId::OnFail)),
                    fresh_session: self.opt_bool(FieldId::FreshSession),
                    harness_override: patch(self.opt_str_field(FieldId::HarnessOverride)),
                    model_override: patch(self.opt_text(FieldId::ModelOverride)),
                    effort_override: patch(self.opt_str_field(FieldId::EffortOverride)),
                    permission_override: patch(self.opt_str_field(FieldId::PermissionOverride)),
                    timeout_minutes: patch(self.opt_int(FieldId::Timeout)),
                }))
            }
            FormKind::Comment { card_id } => {
                let body = self.trim(FieldId::CommentBody);
                if body.is_empty() {
                    return Err("comment is empty".into());
                }
                Ok(Submit::Comment { card_id, body })
            }
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

fn patch<T>(value: Option<T>) -> Patch<T> {
    value.map(Patch::Set).unwrap_or(Patch::Clear)
}

/// Parse a herdr session name from a `HERDR_SOCKET_PATH` value.
///
/// A named session's socket lives at `…/sessions/<name>/herdr.sock`; anything
/// else (unset, or the plain default `…/herdr.sock`) means the daemon's default
/// session, represented as `None`. This function is pure so production
/// composition can inject its result without test-time environment reads.
pub fn session_name_from_socket(path: Option<&str>) -> Option<String> {
    // Expect the tail `sessions/<name>/herdr.sock`.
    let rest = path?.strip_suffix("/herdr.sock")?;
    let (parent, name) = rest.rsplit_once('/')?;
    let last_seg = parent.rsplit('/').next().unwrap_or(parent);
    (last_seg == "sessions" && !name.is_empty()).then(|| name.to_string())
}
