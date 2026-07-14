//! Modal form model: card create/edit, column create/edit, and add-comment.
//!
//! A [`Form`] is a flat list of [`Field`]s plus a focus index. Fields are either
//! free text (backed by a `tui_textarea::TextArea` so `Ctrl+E` can hand the buffer
//! to `$EDITOR`) or a cyclic [`Choice`]. Rendering lives in `view`; this module
//! owns construction, focus movement, field cycling, and turning a submitted form
//! into a protocol params struct.

use board_core::model::{Card, Column};
use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, Effort, SpaceKind,
    Trigger,
};
use tui_textarea::TextArea;

/// Stable identity of a field, used by submit extraction and visibility rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldId {
    // card
    Title,
    Description,
    Harness,
    Model,
    Effort,
    Permission,
    SpaceKind,
    SpaceRef,
    WorktreeBase,
    // column
    Name,
    Trigger,
    SystemPrompt,
    OnSuccess,
    OnFail,
    FreshSession,
    ModelOverride,
    EffortOverride,
    HarnessOverride,
    PermissionOverride,
    Timeout,
    // comment
    CommentBody,
}

/// A concrete value carried by a [`Choice`] option.
#[derive(Clone, Debug, PartialEq)]
pub enum ChoiceVal {
    /// "none" — an explicit no-value.
    None,
    /// A literal wire string (effort/permission/harness/trigger/space kind).
    Str(String),
    /// A column id (on_success / on_fail transitions).
    Col(i64),
    /// A boolean toggle (fresh_session).
    Bool(bool),
}

/// One selectable option in a [`Choice`] field.
#[derive(Clone, Debug)]
pub struct ChoiceOpt {
    pub label: String,
    pub val: ChoiceVal,
}

impl ChoiceOpt {
    fn str(label: &str) -> ChoiceOpt {
        ChoiceOpt {
            label: label.to_string(),
            val: ChoiceVal::Str(label.to_string()),
        }
    }
    fn none() -> ChoiceOpt {
        ChoiceOpt {
            label: "none".to_string(),
            val: ChoiceVal::None,
        }
    }
}

/// Field contents: free text or a cyclic choice.
///
/// `TextArea` is large, so it is boxed to keep this enum small.
pub enum FieldKind {
    Text(Box<TextArea<'static>>),
    Choice { opts: Vec<ChoiceOpt>, idx: usize },
}

/// A single form field.
pub struct Field {
    pub id: FieldId,
    pub label: &'static str,
    pub kind: FieldKind,
    /// Multiline free-text field: eligible for `Ctrl+E` and rendered taller.
    pub multiline: bool,
}

impl Field {
    fn text(id: FieldId, label: &'static str, initial: &str, multiline: bool) -> Field {
        Field {
            id,
            label,
            kind: FieldKind::Text(Box::new(new_textarea(initial))),
            multiline,
        }
    }

    fn choice(id: FieldId, label: &'static str, opts: Vec<ChoiceOpt>, idx: usize) -> Field {
        Field {
            id,
            label,
            kind: FieldKind::Choice { opts, idx },
            multiline: false,
        }
    }

    /// Current text (single string, newlines preserved). Empty for choices.
    pub fn get_text(&self) -> String {
        match &self.kind {
            FieldKind::Text(ta) => ta.lines().join("\n"),
            FieldKind::Choice { .. } => String::new(),
        }
    }

    /// Overwrite a text field's buffer (used after `$EDITOR` returns).
    pub fn set_text(&mut self, s: &str) {
        if let FieldKind::Text(ta) = &mut self.kind {
            **ta = new_textarea(s);
        }
    }

    /// The selected choice's value, if this is a choice field.
    pub fn choice_val(&self) -> Option<&ChoiceVal> {
        match &self.kind {
            FieldKind::Choice { opts, idx } => opts.get(*idx).map(|o| &o.val),
            FieldKind::Text(_) => None,
        }
    }

    /// Human-readable current value (for rendering).
    pub fn display(&self) -> String {
        match &self.kind {
            FieldKind::Text(ta) => ta.lines().join(" "),
            FieldKind::Choice { opts, idx } => {
                opts.get(*idx).map(|o| o.label.clone()).unwrap_or_default()
            }
        }
    }

    /// Cycle a choice field by `delta` (wrapping); no-op for text fields.
    pub fn cycle(&mut self, delta: isize) {
        if let FieldKind::Choice { opts, idx } = &mut self.kind {
            if opts.is_empty() {
                return;
            }
            let n = opts.len() as isize;
            *idx = (*idx as isize + delta).rem_euclid(n) as usize;
        }
    }

    fn is_choice(&self) -> bool {
        matches!(self.kind, FieldKind::Choice { .. })
    }
}

fn new_textarea(initial: &str) -> TextArea<'static> {
    if initial.is_empty() {
        TextArea::default()
    } else {
        TextArea::new(initial.split('\n').map(|s| s.to_string()).collect())
    }
}

/// What a form submits into.
#[derive(Clone, Copy, Debug)]
pub enum FormKind {
    CardCreate { column_id: i64 },
    CardEdit { card_id: i64 },
    ColumnCreate,
    ColumnEdit { column_id: i64 },
    Comment { card_id: i64 },
}

/// A modal form.
pub struct Form {
    pub kind: FormKind,
    pub fields: Vec<Field>,
    pub focus: usize,
}

/// The params produced by a successful submit, ready for a client call.
pub enum Submit {
    CardCreate(CardCreateParams),
    CardUpdate(CardUpdateParams),
    ColumnCreate(ColumnCreateParams),
    ColumnUpdate(ColumnUpdateParams),
    Comment { card_id: i64, body: String },
}

impl Form {
    // -- construction --------------------------------------------------------

    pub fn card_create(column_id: i64) -> Form {
        Form {
            kind: FormKind::CardCreate { column_id },
            fields: card_fields(None),
            focus: 0,
        }
    }

    pub fn card_edit(card: &Card) -> Form {
        Form {
            kind: FormKind::CardEdit { card_id: card.id },
            fields: card_fields(Some(card)),
            focus: 0,
        }
    }

    pub fn column_create(columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnCreate,
            fields: column_fields(None, columns),
            focus: 0,
        }
    }

    pub fn column_edit(col: &Column, columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnEdit { column_id: col.id },
            fields: column_fields(Some(col), columns),
            focus: 0,
        }
    }

    pub fn comment(card_id: i64) -> Form {
        Form {
            kind: FormKind::Comment { card_id },
            fields: vec![Field::text(FieldId::CommentBody, "comment", "", true)],
            focus: 0,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            FormKind::CardCreate { .. } => "New card",
            FormKind::CardEdit { .. } => "Edit card",
            FormKind::ColumnCreate => "New column",
            FormKind::ColumnEdit { .. } => "Edit column",
            FormKind::Comment { .. } => "Add comment",
        }
    }

    // -- focus / visibility --------------------------------------------------

    /// Whether a field is currently shown (worktree_base only when kind=worktree).
    pub fn field_visible(&self, idx: usize) -> bool {
        let f = &self.fields[idx];
        if f.id == FieldId::WorktreeBase {
            return self.space_kind_is_worktree();
        }
        true
    }

    fn space_kind_is_worktree(&self) -> bool {
        self.fields
            .iter()
            .find(|f| f.id == FieldId::SpaceKind)
            .and_then(|f| f.choice_val())
            .map(|v| matches!(v, ChoiceVal::Str(s) if s == "worktree"))
            .unwrap_or(false)
    }

    /// Move focus to the next/previous visible field (wrapping).
    pub fn focus_step(&mut self, delta: isize) {
        let n = self.fields.len();
        if n == 0 {
            return;
        }
        let mut i = self.focus;
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.field_visible(i) {
                self.focus = i;
                return;
            }
        }
    }

    pub fn focused(&self) -> &Field {
        &self.fields[self.focus]
    }
    pub fn focused_mut(&mut self) -> &mut Field {
        &mut self.fields[self.focus]
    }

    pub fn focused_is_choice(&self) -> bool {
        self.fields[self.focus].is_choice()
    }
    pub fn focused_is_multiline(&self) -> bool {
        self.fields[self.focus].multiline
    }

    fn field(&self, id: FieldId) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

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
                    description: self.opt_text(FieldId::Description),
                    column_id: Some(column_id),
                    harness: self.opt_choice_str(FieldId::Harness),
                    model: self.opt_text(FieldId::Model),
                    effort: self.opt_effort(FieldId::Effort),
                    permission_mode: self.opt_choice_str(FieldId::Permission),
                    space_kind: self.opt_space_kind(),
                    space_ref: self.opt_text(FieldId::SpaceRef),
                    worktree_base: if self.space_kind_is_worktree() {
                        self.opt_text(FieldId::WorktreeBase)
                    } else {
                        None
                    },
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
                    model: self.opt_text(FieldId::Model),
                    effort: self.opt_effort(FieldId::Effort),
                    permission_mode: self.opt_choice_str(FieldId::Permission),
                    space_kind: self.opt_space_kind(),
                    space_ref: self.opt_text(FieldId::SpaceRef),
                    worktree_base: if self.space_kind_is_worktree() {
                        self.opt_text(FieldId::WorktreeBase)
                    } else {
                        None
                    },
                }))
            }
            FormKind::ColumnCreate => {
                let name = self.trim(FieldId::Name);
                if name.is_empty() {
                    return Err("name is required".into());
                }
                Ok(Submit::ColumnCreate(ColumnCreateParams {
                    name,
                    position: None,
                    system_prompt: self.opt_text(FieldId::SystemPrompt),
                    trigger: self.opt_trigger(),
                    on_success_column_id: self.opt_col(FieldId::OnSuccess),
                    on_fail_column_id: self.opt_col(FieldId::OnFail),
                    fresh_session: self.opt_bool(FieldId::FreshSession),
                    harness_override: self.opt_text(FieldId::HarnessOverride),
                    model_override: self.opt_text(FieldId::ModelOverride),
                    effort_override: self.opt_choice_str(FieldId::EffortOverride),
                    permission_override: self.opt_choice_str(FieldId::PermissionOverride),
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
                    system_prompt: Some(self.trim(FieldId::SystemPrompt)),
                    trigger: self.opt_trigger(),
                    on_success_column_id: self.opt_col(FieldId::OnSuccess),
                    on_fail_column_id: self.opt_col(FieldId::OnFail),
                    fresh_session: self.opt_bool(FieldId::FreshSession),
                    harness_override: self.opt_text(FieldId::HarnessOverride),
                    model_override: self.opt_text(FieldId::ModelOverride),
                    effort_override: self.opt_choice_str(FieldId::EffortOverride),
                    permission_override: self.opt_choice_str(FieldId::PermissionOverride),
                    timeout_minutes: self.opt_int(FieldId::Timeout),
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
    fn opt_text(&self, id: FieldId) -> Option<String> {
        let s = self.trim(id);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    fn opt_choice_str(&self, id: FieldId) -> Option<String> {
        match self.field(id).and_then(|f| f.choice_val()) {
            Some(ChoiceVal::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }
    fn opt_col(&self, id: FieldId) -> Option<i64> {
        match self.field(id).and_then(|f| f.choice_val()) {
            Some(ChoiceVal::Col(c)) => Some(*c),
            _ => None,
        }
    }
    fn opt_bool(&self, id: FieldId) -> Option<bool> {
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
    fn opt_int(&self, id: FieldId) -> Option<i64> {
        self.opt_text(id).and_then(|s| s.parse().ok())
    }
}

// -- field templates ---------------------------------------------------------

fn effort_opts(current: Option<&str>) -> (Vec<ChoiceOpt>, usize) {
    let opts = vec![
        ChoiceOpt::none(),
        ChoiceOpt::str("low"),
        ChoiceOpt::str("medium"),
        ChoiceOpt::str("high"),
        ChoiceOpt::str("xhigh"),
        ChoiceOpt::str("max"),
    ];
    let idx = current
        .and_then(|c| opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    (opts, idx)
}

fn permission_opts(current: Option<&str>) -> (Vec<ChoiceOpt>, usize) {
    let opts = vec![
        ChoiceOpt::none(),
        ChoiceOpt::str("acceptEdits"),
        ChoiceOpt::str("plan"),
        ChoiceOpt::str("manual"),
        ChoiceOpt::str("dontAsk"),
    ];
    let idx = current
        .and_then(|c| opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    (opts, idx)
}

fn card_fields(card: Option<&Card>) -> Vec<Field> {
    let (eff_opts, eff_idx) = effort_opts(card.and_then(|c| c.effort).map(|e| e.as_str()));
    let (perm_opts, perm_idx) = permission_opts(card.and_then(|c| c.permission_mode.as_deref()));

    let harness_opts = vec![ChoiceOpt::str("claude")];
    let space_opts = vec![
        ChoiceOpt::str("workspace"),
        ChoiceOpt::str("cwd"),
        ChoiceOpt::str("worktree"),
    ];
    let space_idx = card
        .map(|c| c.space_kind.as_str())
        .and_then(|s| space_opts.iter().position(|o| o.label == s))
        .unwrap_or(0);

    vec![
        Field::text(
            FieldId::Title,
            "title",
            card.map(|c| c.title.as_str()).unwrap_or(""),
            false,
        ),
        Field::text(
            FieldId::Description,
            "description (base prompt)",
            card.map(|c| c.description.as_str()).unwrap_or(""),
            true,
        ),
        Field::choice(FieldId::Harness, "harness", harness_opts, 0),
        Field::text(
            FieldId::Model,
            "model (sonnet/opus/haiku)",
            card.and_then(|c| c.model.as_deref()).unwrap_or(""),
            false,
        ),
        Field::choice(FieldId::Effort, "effort", eff_opts, eff_idx),
        Field::choice(FieldId::Permission, "permission", perm_opts, perm_idx),
        Field::choice(FieldId::SpaceKind, "space", space_opts, space_idx),
        Field::text(
            FieldId::SpaceRef,
            "space ref",
            card.and_then(|c| c.space_ref.as_deref()).unwrap_or(""),
            false,
        ),
        Field::text(
            FieldId::WorktreeBase,
            "worktree base",
            card.and_then(|c| c.worktree_base.as_deref()).unwrap_or(""),
            false,
        ),
    ]
}

fn column_fields(col: Option<&Column>, columns: &[Column]) -> Vec<Field> {
    let trigger_opts = vec![ChoiceOpt::str("manual"), ChoiceOpt::str("auto")];
    let trigger_idx = col
        .map(|c| c.trigger.as_str())
        .and_then(|s| trigger_opts.iter().position(|o| o.label == s))
        .unwrap_or(0);

    // on_success / on_fail: "none" plus every existing column.
    let mut col_opts = vec![ChoiceOpt::none()];
    for c in columns {
        // A column being edited may target another column, including itself.
        col_opts.push(ChoiceOpt {
            label: c.name.clone(),
            val: ChoiceVal::Col(c.id),
        });
    }
    let on_success_idx = col
        .and_then(|c| c.on_success_column_id)
        .and_then(|id| {
            col_opts
                .iter()
                .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
        })
        .unwrap_or(0);
    let on_fail_idx = col
        .and_then(|c| c.on_fail_column_id)
        .and_then(|id| {
            col_opts
                .iter()
                .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
        })
        .unwrap_or(0);

    let fresh_opts = vec![
        ChoiceOpt {
            label: "no".into(),
            val: ChoiceVal::Bool(false),
        },
        ChoiceOpt {
            label: "yes".into(),
            val: ChoiceVal::Bool(true),
        },
    ];
    let fresh_idx = usize::from(col.map(|c| c.fresh_session).unwrap_or(false));

    let (eff_opts, eff_idx) = effort_opts(col.and_then(|c| c.effort_override.as_deref()));
    let (perm_opts, perm_idx) = permission_opts(col.and_then(|c| c.permission_override.as_deref()));

    vec![
        Field::text(
            FieldId::Name,
            "name",
            col.map(|c| c.name.as_str()).unwrap_or(""),
            false,
        ),
        Field::choice(FieldId::Trigger, "trigger", trigger_opts, trigger_idx),
        Field::text(
            FieldId::SystemPrompt,
            "system prompt",
            col.and_then(|c| c.system_prompt.as_deref()).unwrap_or(""),
            true,
        ),
        Field::choice(
            FieldId::OnSuccess,
            "on success",
            col_opts.clone(),
            on_success_idx,
        ),
        Field::choice(FieldId::OnFail, "on fail", col_opts, on_fail_idx),
        Field::choice(
            FieldId::FreshSession,
            "fresh session",
            fresh_opts,
            fresh_idx,
        ),
        Field::text(
            FieldId::ModelOverride,
            "model override",
            col.and_then(|c| c.model_override.as_deref()).unwrap_or(""),
            false,
        ),
        Field::choice(
            FieldId::EffortOverride,
            "effort override",
            eff_opts,
            eff_idx,
        ),
        Field::text(
            FieldId::HarnessOverride,
            "harness override",
            col.and_then(|c| c.harness_override.as_deref())
                .unwrap_or(""),
            false,
        ),
        Field::choice(
            FieldId::PermissionOverride,
            "permission override",
            perm_opts,
            perm_idx,
        ),
        Field::text(
            FieldId::Timeout,
            "timeout (minutes)",
            &col.and_then(|c| c.timeout_minutes)
                .map(|t| t.to_string())
                .unwrap_or_default(),
            false,
        ),
    ]
}
