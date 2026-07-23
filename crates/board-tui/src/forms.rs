//! Modal form model: card create/edit, column create/edit, and add-comment.
//!
//! A [`Form`] is a flat list of [`Field`]s plus a focus index. Fields are either
//! free text (backed by a `tui_textarea::TextArea` so `Ctrl+E` can hand the buffer
//! to `$EDITOR`) or a cyclic [`Choice`]. Rendering lives in `view`; this module
//! owns construction, focus movement, field cycling, and turning a submitted form
//! into a protocol params struct.

use board_core::capability::{efforts_for, HarnessCapabilities};
use board_core::harness::{BUILTIN_HARNESSES, DEFAULT_HARNESS};
use board_core::model::{Card, Column};
use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, Effort, Patch,
    SessionInfo, SpaceInfo, SpaceKind, Trigger,
};
use tui_textarea::TextArea;

/// Reasoning efforts in canonical (ascending) order — the fallback effort menu
/// and the ordering used when taking the union of a catalog's efforts.
const EFFORT_ORDER: [Effort; 7] = [
    Effort::Off,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Claude permission modes offered when its capability catalog cannot be
/// fetched. Pi hides this field even in fallback mode.
const FALLBACK_PERMISSION_MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// Stable identity of a field, used by submit extraction and visibility rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldId {
    // card
    Title,
    Description,
    Harness,
    Model,
    /// Free-text model id, revealed when `Model` is set to `(custom)`.
    ModelCustom,
    Effort,
    Permission,
    /// herdr session selector; `(default)` = the daemon's default session.
    Session,
    SpaceKind,
    SpaceRef,
    /// Free-text space ref, revealed when the `SpaceRef` selector is `(custom)`.
    SpaceRefCustom,
    /// Working directory for a `new_workspace` space (shown only for that kind).
    SpaceCwd,
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
    /// The `(custom)` escape hatch — reveals a paired free-text field.
    Custom,
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
    /// The "unset / harness default" option (extracts to `None`).
    fn default_opt() -> ChoiceOpt {
        ChoiceOpt {
            label: "(default)".to_string(),
            val: ChoiceVal::None,
        }
    }
    /// The free-text escape hatch.
    fn custom() -> ChoiceOpt {
        ChoiceOpt {
            label: "(custom)".to_string(),
            val: ChoiceVal::Custom,
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
    /// Live capability catalog for the form's driving harness — the card
    /// `Harness` field for card forms, the column `HarnessOverride` field for
    /// column forms. `None` = not yet fetched, or the fetch failed → guided
    /// fields fall back to free-text / static menus.
    pub caps: Option<HarnessCapabilities>,
    /// Available harness names (built-ins + config-defined), fetched via
    /// `harness.list`. Seeds both the card `Harness` selector and the column
    /// `HarnessOverride` selector. Defaults to the built-ins so the form is
    /// usable before the fetch lands.
    pub harnesses: Vec<String>,
    /// Sibling columns (column forms only) — the on-success/on-fail option
    /// source, retained so rebuilds on caps/harness changes regenerate them.
    pub columns: Vec<Column>,
    /// Live workspace list for the space selector (card forms only). Empty when
    /// unfetched / failed → the space ref falls back to free-text.
    pub spaces: Vec<SpaceInfo>,
    /// Live herdr session list for the session selector (card forms only).
    /// Empty when unfetched / failed → only `(default)` (plus any preselected
    /// session) is offered.
    pub sessions: Vec<SessionInfo>,
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
        Self::card_create_with_session(column_id, None)
    }

    pub fn card_create_with_session(column_id: i64, session: Option<&str>) -> Form {
        let values = CardValues::from_card(None, session);
        Form {
            kind: FormKind::CardCreate { column_id },
            fields: build_card_fields(&values, None, &default_harnesses(), &[], &[]),
            focus: 0,
            caps: None,
            harnesses: default_harnesses(),
            columns: Vec::new(),
            spaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    pub fn card_edit(card: &Card) -> Form {
        let values = CardValues::from_card(Some(card), None);
        Form {
            kind: FormKind::CardEdit { card_id: card.id },
            fields: build_card_fields(&values, None, &default_harnesses(), &[], &[]),
            focus: 0,
            caps: None,
            harnesses: default_harnesses(),
            columns: Vec::new(),
            spaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    pub fn column_create(columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnCreate,
            fields: column_fields(None, columns, None, &default_harnesses()),
            focus: 0,
            caps: None,
            harnesses: default_harnesses(),
            columns: columns.to_vec(),
            spaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    pub fn column_edit(col: &Column, columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnEdit { column_id: col.id },
            fields: column_fields(Some(col), columns, None, &default_harnesses()),
            focus: 0,
            caps: None,
            harnesses: default_harnesses(),
            columns: columns.to_vec(),
            spaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    pub fn comment(card_id: i64) -> Form {
        Form {
            kind: FormKind::Comment { card_id },
            fields: vec![Field::text(FieldId::CommentBody, "comment", "", true)],
            focus: 0,
            caps: None,
            harnesses: default_harnesses(),
            columns: Vec::new(),
            spaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    /// Whether this form has the guided card selectors (model/effort/…).
    pub fn is_card_form(&self) -> bool {
        matches!(
            self.kind,
            FormKind::CardCreate { .. } | FormKind::CardEdit { .. }
        )
    }

    /// Whether this form is a column create/edit form.
    pub fn is_column_form(&self) -> bool {
        matches!(
            self.kind,
            FormKind::ColumnCreate | FormKind::ColumnEdit { .. }
        )
    }

    /// The harness the guided selectors should be populated for: the card
    /// `Harness` field for card forms, the column `HarnessOverride` field for
    /// column forms. Drives which `harness.capabilities` the loader fetches.
    pub fn current_harness(&self) -> String {
        let id = if self.is_column_form() {
            FieldId::HarnessOverride
        } else {
            FieldId::Harness
        };
        self.opt_choice_str(id)
            .unwrap_or_else(|| DEFAULT_HARNESS.to_string())
    }

    /// The currently selected herdr session (`None` = the daemon's default
    /// session). Drives which session `space.list` is fetched for.
    pub fn current_session(&self) -> Option<String> {
        self.opt_choice_str(FieldId::Session)
    }

    /// Install freshly fetched capabilities / harness list / spaces / sessions
    /// and rebuild the guided fields (preserving whatever the user already
    /// selected/typed). A `None` argument means the fetch failed — the affected
    /// selectors fall back to free-text / minimal menus. No-op for comment forms.
    pub fn apply_options(
        &mut self,
        caps: Option<HarnessCapabilities>,
        harnesses: Option<Vec<String>>,
        spaces: Option<Vec<SpaceInfo>>,
        sessions: Option<Vec<SessionInfo>>,
    ) {
        self.caps = caps;
        if let Some(h) = harnesses {
            self.harnesses = h;
        }
        if self.is_card_form() {
            if let Some(sp) = spaces {
                self.spaces = sp;
            }
            if let Some(se) = sessions {
                self.sessions = se;
            }
        }
        self.rebuild_fields();
    }

    /// React to a model change: effort options follow the selected model, and
    /// the current effort is kept only if still valid (else reset to default).
    pub fn on_model_changed(&mut self) {
        self.rebuild_card_fields();
    }

    /// React to a space-kind change: the space ref flips between the workspace
    /// selector and free text.
    pub fn on_space_kind_changed(&mut self) {
        self.rebuild_card_fields();
    }

    fn rebuild_fields(&mut self) {
        if self.is_card_form() {
            self.rebuild_card_fields();
        } else if self.is_column_form() {
            self.rebuild_column_fields();
        }
    }

    fn rebuild_card_fields(&mut self) {
        if !self.is_card_form() {
            return;
        }
        let values = self.card_values();
        self.fields = build_card_fields(
            &values,
            self.caps.as_ref(),
            &self.harnesses,
            &self.spaces,
            &self.sessions,
        );
        if self.focus >= self.fields.len() {
            self.focus = 0;
        }
        if !self.field_visible(self.focus) {
            self.focus_step(1);
        }
    }

    /// Rebuild the column fields after caps / harness list arrive or after the
    /// harness-override changes. Values that stay valid are preserved; an
    /// effort/permission override that the new harness doesn't offer resets to
    /// the default option.
    fn rebuild_column_fields(&mut self) {
        if !self.is_column_form() {
            return;
        }
        let values = self.column_values();
        self.fields =
            column_fields_from_values(&values, &self.columns, self.caps.as_ref(), &self.harnesses);
        if self.focus >= self.fields.len() {
            self.focus = 0;
        }
        if !self.field_visible(self.focus) {
            self.focus_step(1);
        }
    }

    /// Snapshot the current card field values (for rebuilding in place).
    fn card_values(&self) -> CardValues {
        CardValues {
            title: self.raw_text(FieldId::Title),
            description: self.raw_text(FieldId::Description),
            harness: self.current_harness(),
            model: self.card_model().unwrap_or_default(),
            model_custom_selected: self.model_is_custom(),
            effort: self.opt_choice_str(FieldId::Effort),
            permission: self.opt_choice_str(FieldId::Permission),
            session: self.current_session(),
            space_kind: self
                .opt_choice_str(FieldId::SpaceKind)
                .unwrap_or_else(|| "workspace".to_string()),
            space_ref: self.card_space_ref().unwrap_or_default(),
            space_cwd: self.raw_text(FieldId::SpaceCwd),
        }
    }

    /// Snapshot the current column field values (for rebuilding in place).
    /// The override harness drives caps; `effort_override`/`permission_override`
    /// are read as the effective choice/text string (`None` = default/none).
    fn column_values(&self) -> ColumnValues {
        ColumnValues {
            name: self.raw_text(FieldId::Name),
            system_prompt: self.raw_text(FieldId::SystemPrompt),
            trigger: self.opt_choice_str(FieldId::Trigger),
            on_success: self.opt_col(FieldId::OnSuccess),
            on_fail: self.opt_col(FieldId::OnFail),
            fresh_session: self.opt_bool(FieldId::FreshSession),
            harness_override: self.opt_str_field(FieldId::HarnessOverride),
            model_override: self.opt_text(FieldId::ModelOverride),
            effort_override: self.opt_str_field(FieldId::EffortOverride),
            permission_override: self.opt_str_field(FieldId::PermissionOverride),
            timeout: self.opt_int(FieldId::Timeout),
        }
    }

    fn raw_text(&self, id: FieldId) -> String {
        self.field(id).map(|f| f.get_text()).unwrap_or_default()
    }

    fn model_is_custom(&self) -> bool {
        matches!(
            self.field(FieldId::Model).and_then(|f| f.choice_val()),
            Some(ChoiceVal::Custom)
        )
    }

    fn space_ref_is_custom(&self) -> bool {
        matches!(
            self.field(FieldId::SpaceRef).and_then(|f| f.choice_val()),
            Some(ChoiceVal::Custom)
        )
    }

    /// The effective model string: the selected catalog id, the custom text, or
    /// the raw free-text field (fallback).
    fn card_model(&self) -> Option<String> {
        match self.field(FieldId::Model).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => {
                match self.field(FieldId::Model).and_then(|f| f.choice_val()) {
                    Some(ChoiceVal::Str(s)) => Some(s.clone()),
                    Some(ChoiceVal::Custom) => self.opt_text(FieldId::ModelCustom),
                    _ => None,
                }
            }
            _ => self.opt_text(FieldId::Model),
        }
    }

    /// The effective space ref: the selected workspace id, the custom text, or
    /// the raw free-text field (non-workspace kinds / fallback).
    fn card_space_ref(&self) -> Option<String> {
        match self.field(FieldId::SpaceRef).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => {
                match self.field(FieldId::SpaceRef).and_then(|f| f.choice_val()) {
                    Some(ChoiceVal::Str(s)) => Some(s.clone()),
                    Some(ChoiceVal::Custom) => self.opt_text(FieldId::SpaceRefCustom),
                    _ => None,
                }
            }
            _ => self.opt_text(FieldId::SpaceRef),
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

    /// Whether a field is currently shown. The `(custom)` free-text companion
    /// appears only when the `SpaceRef` selector is on `(custom)`; `cwd` only for
    /// the `new_workspace` space kind; both `permission` selectors disappear
    /// when the driving harness has no permission modes (e.g. Pi).
    pub fn field_visible(&self, idx: usize) -> bool {
        match self.fields[idx].id {
            FieldId::SpaceCwd => self.space_kind_is_new_workspace(),
            FieldId::ModelCustom => self.model_is_custom(),
            FieldId::Permission | FieldId::PermissionOverride => self.permission_is_applicable(),
            FieldId::SpaceRefCustom => self.space_ref_is_custom(),
            _ => true,
        }
    }

    /// Whether any permission selector applies for the form's driving harness.
    /// With a loaded catalog this is `!permission_modes.is_empty()`; without one
    /// (fetch failed / pending) it falls back to "not pi" so the field stays
    /// reachable for claude/config harnesses.
    fn permission_is_applicable(&self) -> bool {
        self.caps
            .as_ref()
            .map(|caps| !caps.permission_modes.is_empty())
            .unwrap_or_else(|| self.current_harness() != "pi")
    }

    fn space_kind_is_new_workspace(&self) -> bool {
        self.fields
            .iter()
            .find(|f| f.id == FieldId::SpaceKind)
            .and_then(|f| f.choice_val())
            .map(|v| matches!(v, ChoiceVal::Str(s) if s == "new_workspace"))
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
    /// Effective string for a field that may be a choice (default/none → `None`)
    /// or free text (fallback when no catalog is loaded). Used by the column
    /// override fields, which are choices once caps arrive but text otherwise.
    fn opt_str_field(&self, id: FieldId) -> Option<String> {
        match self.field(id).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => self.opt_choice_str(id),
            _ => self.opt_text(id),
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
    /// The `cwd` text, only for a `new_workspace` space (else `None`).
    fn new_workspace_cwd(&self) -> Option<String> {
        if self.space_kind_is_new_workspace() {
            self.opt_text(FieldId::SpaceCwd)
        } else {
            None
        }
    }
    fn opt_int(&self, id: FieldId) -> Option<i64> {
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

// -- field templates ---------------------------------------------------------

/// Built-in harness names, the pre-fetch default for [`Form::harnesses`] so the
/// harness/harness-override selectors are usable before `harness.list` lands.
fn default_harnesses() -> Vec<String> {
    BUILTIN_HARNESSES.iter().map(|s| (*s).to_string()).collect()
}

/// Harness selector options (no leading sentinel): every available harness
/// from the shared `harness.list` source, preserving an unknown current value
/// (e.g. a card whose harness isn't listed yet) by appending it. Used by the
/// card `Harness` field so it draws from the same source as the column
/// harness_override selector. `current` selects itself (else index 0).
fn harness_choice_opts(harnesses: &[String], current: &str) -> (Vec<ChoiceOpt>, usize) {
    let mut opts: Vec<ChoiceOpt> = harnesses.iter().map(|h| ChoiceOpt::str(h)).collect();
    if !current.is_empty() && !opts.iter().any(|o| o.label == current) {
        opts.push(ChoiceOpt::str(current));
    }
    let idx = opts.iter().position(|o| o.label == current).unwrap_or(0);
    (opts, idx)
}

/// Harness-override selector options: `(none)` + every available harness,
/// preserving an unknown current value (e.g. a config harness not yet listed)
/// by appending it. `current` of `None` selects `(none)` (no override).
fn harness_override_opts(harnesses: &[String], current: Option<&str>) -> (Vec<ChoiceOpt>, usize) {
    let mut opts = vec![ChoiceOpt::none()];
    for h in harnesses {
        if !opts.iter().any(|o| &o.label == h) {
            opts.push(ChoiceOpt::str(h));
        }
    }
    if let Some(cur) = current {
        if !opts.iter().any(|o| o.label == cur) {
            opts.push(ChoiceOpt::str(cur));
        }
    }
    let idx = current
        .and_then(|c| opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    (opts, idx)
}

/// Shared effort selector: `(default)` (wire `None`) + each effort. Used by
/// both the card `Effort` field and the column `EffortOverride` field so the
/// two forms share one source of truth for the effort menu.
fn effort_choice_opts(efforts: &[Effort], current: Option<&str>) -> (Vec<ChoiceOpt>, usize) {
    let mut opts = vec![ChoiceOpt::default_opt()];
    for e in efforts {
        opts.push(ChoiceOpt::str(e.as_str()));
    }
    let idx = current
        .and_then(|c| opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    (opts, idx)
}

/// Shared permission selector: `(default)` (wire `None`) + each mode. Used by
/// both the card `Permission` field and the column `PermissionOverride` field.
fn permission_choice_opts(modes: &[String], current: Option<&str>) -> (Vec<ChoiceOpt>, usize) {
    let mut opts = vec![ChoiceOpt::default_opt()];
    for m in modes {
        opts.push(ChoiceOpt::str(m));
    }
    let idx = current
        .and_then(|c| opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    (opts, idx)
}

/// A flat snapshot of a card form's values, used to (re)build the fields.
#[derive(Clone, Default)]
struct CardValues {
    title: String,
    description: String,
    harness: String,
    /// Effective model string ("" = none / catalog default).
    model: String,
    /// Keep an empty `(custom)` selection stable while its companion field is
    /// first revealed.
    model_custom_selected: bool,
    /// Effort wire string, or `None` for the harness default.
    effort: Option<String>,
    /// Permission-mode wire string, or `None` for the harness default.
    permission: Option<String>,
    /// Selected herdr session name, or `None` for the daemon's default session.
    session: Option<String>,
    space_kind: String,
    /// Effective space ref (workspace id, or new-workspace label / free text).
    space_ref: String,
    /// Working directory for a `new_workspace` space ("" = unset).
    space_cwd: String,
}

impl CardValues {
    fn from_card(card: Option<&Card>, default_session: Option<&str>) -> CardValues {
        match card {
            Some(c) => CardValues {
                title: c.title.clone(),
                description: c.description.clone(),
                harness: c.harness.clone(),
                model: c.model.clone().unwrap_or_default(),
                model_custom_selected: false,
                effort: c.effort.map(|e| e.as_str().to_string()),
                permission: c.permission_mode.clone(),
                session: c.session.clone(),
                space_kind: c.space_kind.as_str().to_string(),
                space_ref: c.space_ref.clone().unwrap_or_default(),
                space_cwd: c.space_cwd.clone().unwrap_or_default(),
            },
            None => CardValues {
                harness: DEFAULT_HARNESS.to_string(),
                session: default_session.map(str::to_string),
                space_kind: "workspace".to_string(),
                ..CardValues::default()
            },
        }
    }
}

/// Build the guided card fields from the current values and (optional) live
/// catalog / workspace / session lists. The field list is a fixed set in a
/// stable order — `(custom)` companions and `cwd` are hidden via
/// [`Form::field_visible`] rather than omitted, so focus indices stay stable
/// across rebuilds.
fn build_card_fields(
    values: &CardValues,
    caps: Option<&HarnessCapabilities>,
    harnesses: &[String],
    spaces: &[SpaceInfo],
    sessions: &[SessionInfo],
) -> Vec<Field> {
    let v = values;

    // -- harness -------------------------------------------------------------
    // Drawn from the shared `harness.list` source (`Form::harnesses`), same list
    // the column harness_override selector uses; pi stays first (default).
    let (harness_opts, harness_idx) = harness_choice_opts(harnesses, &v.harness);

    // -- model ---------------------------------------------------------------
    let model_in_catalog = caps
        .map(|c| c.models.iter().any(|m| m.id == v.model))
        .unwrap_or(false);
    let use_custom_model = caps.map(|c| c.model_freeform).unwrap_or(false)
        && (v.model_custom_selected || (!v.model.is_empty() && !model_in_catalog));

    let model_field = match caps {
        Some(caps) => {
            let mut opts = vec![ChoiceOpt::default_opt()];
            opts.extend(caps.models.iter().map(|model| ChoiceOpt::str(&model.id)));
            if caps.model_freeform {
                opts.push(ChoiceOpt::custom());
            }
            let idx = if use_custom_model {
                opts.iter()
                    .position(|o| matches!(o.val, ChoiceVal::Custom))
                    .unwrap_or(0)
            } else if v.model.is_empty() {
                0
            } else {
                opts.iter().position(|o| o.label == v.model).unwrap_or(0)
            };
            Field::choice(FieldId::Model, "model", opts, idx)
        }
        None => Field::text(
            FieldId::Model,
            "model (blank = harness default)",
            &v.model,
            false,
        ),
    };
    let model_custom_init = if use_custom_model {
        v.model.as_str()
    } else {
        ""
    };
    let model_custom_field = Field::text(
        FieldId::ModelCustom,
        "custom model",
        model_custom_init,
        false,
    );

    // -- effort (options follow the selected model) --------------------------
    let efforts: Vec<Effort> = match caps {
        Some(caps) => {
            let selected_id = if !use_custom_model && model_in_catalog {
                Some(v.model.clone())
            } else {
                None
            };
            efforts_for(caps, selected_id.as_deref())
        }
        None if v.harness == "pi" => EFFORT_ORDER.to_vec(),
        None => EFFORT_ORDER[2..].to_vec(),
    };
    let (eff_opts, eff_idx) = effort_choice_opts(&efforts, v.effort.as_deref());
    let effort_field = Field::choice(FieldId::Effort, "effort", eff_opts, eff_idx);

    // -- permission ----------------------------------------------------------
    let modes: Vec<String> = match caps {
        Some(caps) => caps.permission_modes.clone(),
        None => FALLBACK_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let (perm_opts, perm_idx) = permission_choice_opts(&modes, v.permission.as_deref());
    let permission_field = Field::choice(FieldId::Permission, "permission", perm_opts, perm_idx);

    // -- session (running sessions + `(default)` = daemon's default) ---------
    let session_field = session_field(v.session.as_deref(), sessions);

    // -- space kind (exactly workspace / new workspace) ----------------------
    let space_opts = vec![
        ChoiceOpt {
            label: "workspace".to_string(),
            val: ChoiceVal::Str("workspace".to_string()),
        },
        ChoiceOpt {
            label: "new workspace".to_string(),
            val: ChoiceVal::Str("new_workspace".to_string()),
        },
    ];
    let space_idx = space_opts
        .iter()
        .position(|o| matches!(&o.val, ChoiceVal::Str(s) if *s == v.space_kind))
        .unwrap_or(0);
    let is_new_workspace = v.space_kind == "new_workspace";

    // -- space ref --------------------------------------------------------
    //  * workspace     → an open-workspace selector (label shown, id stored),
    //    with a `(custom)` free-text escape hatch; free text if none fetched.
    //  * new_workspace → a plain text field: the workspace label/name.
    let is_workspace = !is_new_workspace;
    let ref_matches_workspace = spaces.iter().any(|s| s.id == v.space_ref);
    let (space_ref_field, space_ref_custom_init) = if is_new_workspace {
        (
            Field::text(FieldId::SpaceRef, "workspace name", &v.space_ref, false),
            "",
        )
    } else if is_workspace && !spaces.is_empty() {
        // Show the label but store the id; keep a `(custom)` escape hatch.
        let mut opts: Vec<ChoiceOpt> = spaces
            .iter()
            .map(|s| ChoiceOpt {
                label: format!("{} ({})", s.label, s.id),
                val: ChoiceVal::Str(s.id.clone()),
            })
            .collect();
        opts.push(ChoiceOpt::custom());
        let idx = if ref_matches_workspace {
            opts.iter()
                .position(|o| matches!(&o.val, ChoiceVal::Str(id) if *id == v.space_ref))
                .unwrap_or(0)
        } else if v.space_ref.is_empty() {
            0 // default to the first workspace
        } else {
            opts.iter()
                .position(|o| matches!(o.val, ChoiceVal::Custom))
                .unwrap_or(0)
        };
        let custom_init = if v.space_ref.is_empty() || ref_matches_workspace {
            ""
        } else {
            v.space_ref.as_str()
        };
        (
            Field::choice(FieldId::SpaceRef, "space ref", opts, idx),
            custom_init,
        )
    } else {
        (
            Field::text(FieldId::SpaceRef, "space ref", &v.space_ref, false),
            "",
        )
    };
    let space_ref_custom_field = Field::text(
        FieldId::SpaceRefCustom,
        "custom space ref",
        space_ref_custom_init,
        false,
    );

    vec![
        Field::text(FieldId::Title, "title", &v.title, false),
        Field::text(
            FieldId::Description,
            "description (base prompt)",
            &v.description,
            true,
        ),
        Field::choice(FieldId::Harness, "harness", harness_opts, harness_idx),
        model_field,
        model_custom_field,
        effort_field,
        permission_field,
        session_field,
        Field::choice(FieldId::SpaceKind, "space", space_opts, space_idx),
        space_ref_field,
        space_ref_custom_field,
        Field::text(FieldId::SpaceCwd, "cwd", &v.space_cwd, false),
    ]
}

/// Build the session selector: `(default)` (the daemon's default session, wire
/// value `None`) plus every running session by name. `current` (the preselected
/// session name, `None` = default) is always offered even if it is not in the
/// fetched list — so the env-detected session survives the empty→fetched
/// rebuild and edits of cards whose session is stopped stay visible.
fn session_field(current: Option<&str>, sessions: &[SessionInfo]) -> Field {
    let mut opts = vec![ChoiceOpt::default_opt()];
    for s in sessions.iter().filter(|s| s.running) {
        opts.push(ChoiceOpt::str(&s.name));
    }
    if let Some(name) = current {
        if !opts.iter().any(|o| o.label == name) {
            opts.push(ChoiceOpt::str(name));
        }
    }
    let idx = match current {
        Some(name) => opts.iter().position(|o| o.label == name).unwrap_or(0),
        None => 0,
    };
    Field::choice(FieldId::Session, "session", opts, idx)
}

/// A flat snapshot of a column form's values, used to (re)build the fields.
/// The override harness drives the capability catalog; `*_override` strings
/// are `None` for the default/none option.
#[derive(Clone, Default)]
struct ColumnValues {
    name: String,
    system_prompt: String,
    /// Trigger wire string (`None` = default manual).
    trigger: Option<String>,
    on_success: Option<i64>,
    on_fail: Option<i64>,
    fresh_session: Option<bool>,
    /// Selected override harness (`None` = no override / column default).
    harness_override: Option<String>,
    /// Free-text model override (`None` = unset).
    model_override: Option<String>,
    effort_override: Option<String>,
    permission_override: Option<String>,
    timeout: Option<i64>,
}

impl ColumnValues {
    /// Seed from a column model (create = `None`).
    fn from_column(col: Option<&Column>) -> ColumnValues {
        match col {
            Some(c) => ColumnValues {
                name: c.name.clone(),
                system_prompt: c.system_prompt.clone().unwrap_or_default(),
                trigger: Some(c.trigger.as_str().to_string()),
                on_success: c.on_success_column_id,
                on_fail: c.on_fail_column_id,
                fresh_session: Some(c.fresh_session),
                harness_override: c.harness_override.clone(),
                model_override: c.model_override.clone(),
                effort_override: c.effort_override.clone(),
                permission_override: c.permission_override.clone(),
                timeout: c.timeout_minutes,
            },
            None => ColumnValues::default(),
        }
    }
}

/// Build the column fields from a column model. Delegates to
/// [`column_fields_from_values`] via a [`ColumnValues`] snapshot.
fn column_fields(
    col: Option<&Column>,
    columns: &[Column],
    caps: Option<&HarnessCapabilities>,
    harnesses: &[String],
) -> Vec<Field> {
    column_fields_from_values(&ColumnValues::from_column(col), columns, caps, harnesses)
}

/// Build the column fields from a value snapshot. The harness-override,
/// effort-override, and permission-override selectors share the same builders
/// as the card form so the two forms draw from one source of truth
/// (`harness.capabilities` + `harness.list`). Invalid override values (e.g. an
/// effort the new harness doesn't offer) reset to the default option via the
/// builder's `unwrap_or(0)`.
fn column_fields_from_values(
    v: &ColumnValues,
    columns: &[Column],
    caps: Option<&HarnessCapabilities>,
    harnesses: &[String],
) -> Vec<Field> {
    let trigger_opts = vec![ChoiceOpt::str("manual"), ChoiceOpt::str("auto")];
    let trigger_idx = v
        .trigger
        .as_deref()
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
    let on_success_idx = v
        .on_success
        .and_then(|id| {
            col_opts
                .iter()
                .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
        })
        .unwrap_or(0);
    let on_fail_idx = v
        .on_fail
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
    let fresh_idx = match v.fresh_session {
        Some(true) => 1,
        _ => 0,
    };

    // -- override fields share the card form's builders ---------------------
    // model_override stays free text: models are advisory and every harness is
    // model-freeform, so a select would add noise without adding safety.
    let model_override_field = Field::text(
        FieldId::ModelOverride,
        "model override",
        v.model_override.as_deref().unwrap_or(""),
        false,
    );

    // Efforts for the override harness (its default/free-form set); fallback
    // to the canonical ladder when caps aren't loaded yet.
    let efforts: Vec<Effort> = match caps {
        Some(c) => efforts_for(c, None),
        None => EFFORT_ORDER.to_vec(),
    };
    let (eff_opts, eff_idx) = effort_choice_opts(&efforts, v.effort_override.as_deref());
    let effort_override_field = Field::choice(
        FieldId::EffortOverride,
        "effort override",
        eff_opts,
        eff_idx,
    );

    // harness_override is now a SELECT over the available harnesses (built-ins
    // + config-defined), not free text. `(none)` = no override.
    let (ho_opts, ho_idx) = harness_override_opts(harnesses, v.harness_override.as_deref());
    let harness_override_field = Field::choice(
        FieldId::HarnessOverride,
        "harness override",
        ho_opts,
        ho_idx,
    );

    // permission_override mirrors the card Permission selector and is hidden
    // (via field_visible) when the harness has no permission modes (Pi).
    let modes: Vec<String> = match caps {
        Some(c) => c.permission_modes.clone(),
        None => FALLBACK_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let (po_opts, po_idx) = permission_choice_opts(&modes, v.permission_override.as_deref());
    let permission_override_field = Field::choice(
        FieldId::PermissionOverride,
        "permission override",
        po_opts,
        po_idx,
    );

    vec![
        Field::text(FieldId::Name, "name", &v.name, false),
        Field::choice(FieldId::Trigger, "trigger", trigger_opts, trigger_idx),
        Field::text(
            FieldId::SystemPrompt,
            "system prompt",
            &v.system_prompt,
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
        model_override_field,
        effort_override_field,
        harness_override_field,
        permission_override_field,
        Field::text(
            FieldId::Timeout,
            "timeout (minutes)",
            &v.timeout.map(|t| t.to_string()).unwrap_or_default(),
            false,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::capability::{
        claude_capabilities, pi_capabilities, HarnessCapabilities, ModelInfo,
    };
    use board_core::protocol::Effort;

    /// Find a field by id.
    fn field(form: &Form, id: FieldId) -> &Field {
        form.fields
            .iter()
            .find(|f| f.id == id)
            .expect("field present")
    }

    /// Labels of a choice field's options.
    fn choice_labels(form: &Form, id: FieldId) -> Vec<String> {
        match &field(form, id).kind {
            FieldKind::Choice { opts, .. } => opts.iter().map(|o| o.label.clone()).collect(),
            _ => panic!("field {id:?} is not a choice"),
        }
    }

    /// Index of a column-field id in the flat field list (for field_visible).
    fn idx_of(form: &Form, id: FieldId) -> usize {
        form.fields.iter().position(|f| f.id == id).unwrap()
    }

    #[test]
    fn card_harness_select_consumes_harness_list() {
        // The card Harness selector draws from the shared `harness.list` source
        // (Form::harnesses) — the same source as the column harness_override
        // selector — so config-defined harnesses appear there too, with pi first
        // (the card default).
        let mut form = Form::card_create(1);
        let before = choice_labels(&form, FieldId::Harness);
        assert_eq!(before, vec!["pi".to_string(), "claude".to_string()]);
        form.apply_options(
            None,
            Some(vec!["pi".into(), "claude".into(), "fake".into()]),
            None,
            None,
        );
        let after = choice_labels(&form, FieldId::Harness);
        assert_eq!(
            after,
            vec!["pi".to_string(), "claude".to_string(), "fake".to_string()]
        );
    }

    #[test]
    fn column_harness_override_is_select_with_builtins() {
        // Before any fetch, harness_override is already a Choice (not free text)
        // seeded with the built-ins + a leading `(none)`.
        let form = Form::column_create(&[]);
        let labels = choice_labels(&form, FieldId::HarnessOverride);
        assert!(labels.first().is_some_and(|l| l == "none"));
        assert!(labels.contains(&"pi".to_string()));
        assert!(labels.contains(&"claude".to_string()));
    }

    #[test]
    fn column_harness_override_select_includes_config_defined() {
        // A harness.list fetch advertising a config-defined harness adds it.
        let mut form = Form::column_create(&[]);
        form.apply_options(
            None,
            Some(vec!["claude".into(), "pi".into(), "fake".into()]),
            None,
            None,
        );
        let labels = choice_labels(&form, FieldId::HarnessOverride);
        assert!(labels.contains(&"fake".to_string()));
    }

    #[test]
    fn column_permission_override_hidden_for_pi_shown_for_claude() {
        // Default (no override) resolves to Pi → permission_override hidden.
        let mut form = Form::column_create(&[]);
        form.apply_options(Some(pi_capabilities()), None, None, None);
        assert!(!form.field_visible(idx_of(&form, FieldId::PermissionOverride)));

        // Switching the override to claude (and loading its caps) shows it.
        form.apply_options(Some(claude_capabilities()), None, None, None);
        assert!(form.field_visible(idx_of(&form, FieldId::PermissionOverride)));
        // And its modes come from the catalog, not a hardcoded list.
        let modes = choice_labels(&form, FieldId::PermissionOverride);
        assert!(modes.contains(&"acceptEdits".to_string()));
        assert!(modes.contains(&"plan".to_string()));
    }

    #[test]
    fn column_effort_override_follows_catalog() {
        // A catalog exposing only `low` restricts the effort-override menu.
        let caps = HarnessCapabilities {
            harness: "fake".into(),
            models: vec![ModelInfo {
                id: "m".into(),
                efforts: vec![Effort::Low],
            }],
            model_freeform: true,
            default_efforts: vec![Effort::Low],
            permission_modes: vec![],
        };
        let mut form = Form::column_create(&[]);
        form.apply_options(Some(caps), None, None, None);
        let labels = choice_labels(&form, FieldId::EffortOverride);
        // `(default)` plus the single declared effort.
        assert_eq!(labels, vec!["(default)".to_string(), "low".to_string()]);
    }

    #[test]
    fn column_cascading_resets_invalid_effort_on_harness_change() {
        // Start on claude; its effort-override menu includes xhigh.
        let mut form = Form::column_create(&[]);
        form.apply_options(Some(claude_capabilities()), None, None, None);
        let before = choice_labels(&form, FieldId::EffortOverride);
        assert!(before.contains(&"xhigh".to_string()));

        // Switch to a harness whose only effort is `low`. After the rebuild the
        // stale `xhigh` is no longer offered (an invalid selection resets to the
        // default option), proving the menu follows the new harness.
        let caps = HarnessCapabilities {
            harness: "fake".into(),
            models: vec![ModelInfo {
                id: "m".into(),
                efforts: vec![Effort::Low],
            }],
            model_freeform: true,
            default_efforts: vec![Effort::Low],
            permission_modes: vec!["auto".into()],
        };
        form.apply_options(Some(caps), None, None, None);
        let after = choice_labels(&form, FieldId::EffortOverride);
        assert!(!after.contains(&"xhigh".to_string()));
        assert!(after.contains(&"low".to_string()));
    }

    #[test]
    fn column_options_rebuild_preserves_values_and_focus() {
        let mut form = Form::column_create(&[]);
        form.fields
            .iter_mut()
            .find(|field| field.id == FieldId::Name)
            .unwrap()
            .set_text("stage");
        form.fields
            .iter_mut()
            .find(|field| field.id == FieldId::SystemPrompt)
            .unwrap()
            .set_text("instructions");
        form.fields
            .iter_mut()
            .find(|field| field.id == FieldId::Timeout)
            .unwrap()
            .set_text("15");
        form.focus = idx_of(&form, FieldId::Timeout);

        form.apply_options(Some(pi_capabilities()), None, None, None);

        assert_eq!(form.focus, idx_of(&form, FieldId::Timeout));
        assert_eq!(field(&form, FieldId::Name).get_text(), "stage");
        assert_eq!(
            field(&form, FieldId::SystemPrompt).get_text(),
            "instructions"
        );
        assert_eq!(field(&form, FieldId::Timeout).get_text(), "15");
    }

    #[test]
    fn column_submit_none_harness_override_extracts_none() {
        // `(none)` harness override extracts to `None` (no override).
        let mut form = Form::column_create(&[]);
        form.apply_options(None, None, None, None);
        // Set a name so submit passes the required-field check.
        if let Some(f) = form.fields.iter_mut().find(|f| f.id == FieldId::Name) {
            f.set_text("Col");
        }
        match form.submit().unwrap() {
            Submit::ColumnCreate(p) => assert_eq!(p.harness_override, None),
            _ => panic!("expected ColumnCreate"),
        }
    }

    // -- session socket parsing ---------------------------------------------

    #[test]
    fn session_name_from_named_session_socket() {
        assert_eq!(
            session_name_from_socket(Some("/home/np/.config/herdr/sessions/feature/herdr.sock")),
            Some("feature".to_string())
        );
    }

    #[test]
    fn session_name_from_default_socket_is_none() {
        // The plain default socket (no `sessions/<name>/` segment) = default.
        assert_eq!(
            session_name_from_socket(Some("/home/np/.config/herdr/herdr.sock")),
            None
        );
    }

    #[test]
    fn session_name_unset_or_unrelated_is_none() {
        assert_eq!(session_name_from_socket(None), None);
        assert_eq!(session_name_from_socket(Some("")), None);
        assert_eq!(session_name_from_socket(Some("/tmp/whatever.sock")), None);
        // A `sessions` dir with an empty name is not a valid session.
        assert_eq!(
            session_name_from_socket(Some("/x/sessions//herdr.sock")),
            None
        );
    }
}
