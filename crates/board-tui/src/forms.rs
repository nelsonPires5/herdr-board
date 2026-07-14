//! Modal form model: card create/edit, column create/edit, and add-comment.
//!
//! A [`Form`] is a flat list of [`Field`]s plus a focus index. Fields are either
//! free text (backed by a `tui_textarea::TextArea` so `Ctrl+E` can hand the buffer
//! to `$EDITOR`) or a cyclic [`Choice`]. Rendering lives in `view`; this module
//! owns construction, focus movement, field cycling, and turning a submitted form
//! into a protocol params struct.

use board_core::capability::HarnessCapabilities;
use board_core::model::{Card, Column};
use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, Effort, SpaceInfo,
    SpaceKind, Trigger,
};
use tui_textarea::TextArea;

/// Reasoning efforts in canonical (ascending) order — the fallback effort menu
/// and the ordering used when taking the union of a catalog's efforts.
const EFFORT_ORDER: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Permission modes offered when the capability catalog can't be fetched
/// (matches the builtin `claude` catalog so the form stays usable offline).
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
    SpaceKind,
    SpaceRef,
    /// Free-text space ref, revealed when the `SpaceRef` selector is `(custom)`.
    SpaceRefCustom,
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
    /// Live capability catalog for the current harness (card forms only).
    /// `None` = not yet fetched, or the fetch failed → guided fields fall back
    /// to free-text / static menus.
    pub caps: Option<HarnessCapabilities>,
    /// Live workspace list for the space selector (card forms only). Empty when
    /// unfetched / failed → the space ref falls back to free-text.
    pub spaces: Vec<SpaceInfo>,
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
        let values = CardValues::from_card(None);
        Form {
            kind: FormKind::CardCreate { column_id },
            fields: build_card_fields(&values, None, &[]),
            focus: 0,
            caps: None,
            spaces: Vec::new(),
        }
    }

    pub fn card_edit(card: &Card) -> Form {
        let values = CardValues::from_card(Some(card));
        Form {
            kind: FormKind::CardEdit { card_id: card.id },
            fields: build_card_fields(&values, None, &[]),
            focus: 0,
            caps: None,
            spaces: Vec::new(),
        }
    }

    pub fn column_create(columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnCreate,
            fields: column_fields(None, columns),
            focus: 0,
            caps: None,
            spaces: Vec::new(),
        }
    }

    pub fn column_edit(col: &Column, columns: &[Column]) -> Form {
        Form {
            kind: FormKind::ColumnEdit { column_id: col.id },
            fields: column_fields(Some(col), columns),
            focus: 0,
            caps: None,
            spaces: Vec::new(),
        }
    }

    pub fn comment(card_id: i64) -> Form {
        Form {
            kind: FormKind::Comment { card_id },
            fields: vec![Field::text(FieldId::CommentBody, "comment", "", true)],
            focus: 0,
            caps: None,
            spaces: Vec::new(),
        }
    }

    /// Whether this form has the guided card selectors (model/effort/…).
    pub fn is_card_form(&self) -> bool {
        matches!(
            self.kind,
            FormKind::CardCreate { .. } | FormKind::CardEdit { .. }
        )
    }

    /// The harness the guided selectors should be populated for.
    pub fn current_harness(&self) -> String {
        self.opt_choice_str(FieldId::Harness)
            .unwrap_or_else(|| "claude".to_string())
    }

    /// Install freshly fetched capabilities / spaces and rebuild the guided
    /// card fields (preserving whatever the user already selected/typed).
    /// A `None` argument means the fetch failed — the affected selectors fall
    /// back to free-text. No-op for non-card forms.
    pub fn apply_options(
        &mut self,
        caps: Option<HarnessCapabilities>,
        spaces: Option<Vec<SpaceInfo>>,
    ) {
        if !self.is_card_form() {
            return;
        }
        self.caps = caps;
        if let Some(sp) = spaces {
            self.spaces = sp;
        }
        self.rebuild_card_fields();
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

    fn rebuild_card_fields(&mut self) {
        if !self.is_card_form() {
            return;
        }
        let values = self.card_values();
        self.fields = build_card_fields(&values, self.caps.as_ref(), &self.spaces);
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
            effort: self.opt_choice_str(FieldId::Effort),
            permission: self.opt_choice_str(FieldId::Permission),
            space_kind: self
                .opt_choice_str(FieldId::SpaceKind)
                .unwrap_or_else(|| "workspace".to_string()),
            space_ref: self.card_space_ref().unwrap_or_default(),
            worktree_base: self.raw_text(FieldId::WorktreeBase),
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

    /// Whether a field is currently shown. The `(custom)` free-text companions
    /// appear only when their selector is on `(custom)`; worktree base only for
    /// the worktree space kind.
    pub fn field_visible(&self, idx: usize) -> bool {
        match self.fields[idx].id {
            FieldId::WorktreeBase => self.space_kind_is_worktree(),
            FieldId::ModelCustom => self.model_is_custom(),
            FieldId::SpaceRefCustom => self.space_ref_is_custom(),
            _ => true,
        }
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
                    model: self.card_model(),
                    effort: self.opt_effort(FieldId::Effort),
                    permission_mode: self.opt_choice_str(FieldId::Permission),
                    space_kind: self.opt_space_kind(),
                    space_ref: self.card_space_ref(),
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
                    model: self.card_model(),
                    effort: self.opt_effort(FieldId::Effort),
                    permission_mode: self.opt_choice_str(FieldId::Permission),
                    space_kind: self.opt_space_kind(),
                    space_ref: self.card_space_ref(),
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

/// A flat snapshot of a card form's values, used to (re)build the fields.
#[derive(Clone, Default)]
struct CardValues {
    title: String,
    description: String,
    harness: String,
    /// Effective model string ("" = none / catalog default).
    model: String,
    /// Effort wire string, or `None` for the harness default.
    effort: Option<String>,
    /// Permission-mode wire string, or `None` for the harness default.
    permission: Option<String>,
    space_kind: String,
    /// Effective space ref (workspace id or free text; "" = unset).
    space_ref: String,
    worktree_base: String,
}

impl CardValues {
    fn from_card(card: Option<&Card>) -> CardValues {
        match card {
            Some(c) => CardValues {
                title: c.title.clone(),
                description: c.description.clone(),
                harness: c.harness.clone(),
                model: c.model.clone().unwrap_or_default(),
                effort: c.effort.map(|e| e.as_str().to_string()),
                permission: c.permission_mode.clone(),
                space_kind: c.space_kind.as_str().to_string(),
                space_ref: c.space_ref.clone().unwrap_or_default(),
                worktree_base: c.worktree_base.clone().unwrap_or_default(),
            },
            None => CardValues {
                harness: "claude".to_string(),
                space_kind: "workspace".to_string(),
                ..CardValues::default()
            },
        }
    }
}

/// The reasoning efforts to offer for a catalog + selected model.
///
/// A known model contributes its own efforts; a custom/unknown model gets the
/// union of every catalog model's efforts (canonical order).
fn union_efforts(caps: &HarnessCapabilities) -> Vec<Effort> {
    EFFORT_ORDER
        .iter()
        .copied()
        .filter(|e| caps.models.iter().any(|m| m.efforts.contains(e)))
        .collect()
}

/// Build the guided card fields from the current values and (optional) live
/// catalog / workspace list. The field list is a fixed 11 entries in a stable
/// order — `(custom)` companions and `worktree base` are hidden via
/// [`Form::field_visible`] rather than omitted, so focus indices stay stable
/// across rebuilds.
fn build_card_fields(
    values: &CardValues,
    caps: Option<&HarnessCapabilities>,
    spaces: &[SpaceInfo],
) -> Vec<Field> {
    let v = values;

    // -- harness (only `claude` is known client-side) ------------------------
    let harness_opts = vec![ChoiceOpt::str("claude")];
    let harness_idx = harness_opts
        .iter()
        .position(|o| o.label == v.harness)
        .unwrap_or(0);

    // -- model ---------------------------------------------------------------
    let model_in_catalog = caps
        .map(|c| c.models.iter().any(|m| m.id == v.model))
        .unwrap_or(false);
    let use_custom_model =
        caps.map(|c| c.model_freeform).unwrap_or(false) && !v.model.is_empty() && !model_in_catalog;

    let model_field = match caps {
        Some(caps) => {
            let mut opts: Vec<ChoiceOpt> =
                caps.models.iter().map(|m| ChoiceOpt::str(&m.id)).collect();
            if caps.model_freeform {
                opts.push(ChoiceOpt::custom());
            }
            let idx = if use_custom_model {
                opts.iter()
                    .position(|o| matches!(o.val, ChoiceVal::Custom))
                    .unwrap_or(0)
            } else {
                opts.iter().position(|o| o.label == v.model).unwrap_or(0)
            };
            Field::choice(FieldId::Model, "model", opts, idx)
        }
        None => Field::text(FieldId::Model, "model (sonnet/opus/haiku)", &v.model, false),
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
            let selected_id = if use_custom_model {
                None
            } else if model_in_catalog {
                Some(v.model.clone())
            } else {
                // create/default: the model selector defaults to the first entry
                caps.models.first().map(|m| m.id.clone())
            };
            match selected_id {
                Some(id) => caps
                    .models
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.efforts.clone())
                    .unwrap_or_else(|| union_efforts(caps)),
                None => union_efforts(caps),
            }
        }
        None => EFFORT_ORDER.to_vec(),
    };
    let mut eff_opts = vec![ChoiceOpt::default_opt()];
    for e in &efforts {
        eff_opts.push(ChoiceOpt::str(e.as_str()));
    }
    let eff_idx = v
        .effort
        .as_deref()
        .and_then(|c| eff_opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    let effort_field = Field::choice(FieldId::Effort, "effort", eff_opts, eff_idx);

    // -- permission ----------------------------------------------------------
    let modes: Vec<String> = match caps {
        Some(caps) => caps.permission_modes.clone(),
        None => FALLBACK_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let mut perm_opts = vec![ChoiceOpt::default_opt()];
    for m in &modes {
        perm_opts.push(ChoiceOpt::str(m));
    }
    let perm_idx = v
        .permission
        .as_deref()
        .and_then(|c| perm_opts.iter().position(|o| o.label == c))
        .unwrap_or(0);
    let permission_field = Field::choice(FieldId::Permission, "permission", perm_opts, perm_idx);

    // -- space kind ----------------------------------------------------------
    let space_opts = vec![
        ChoiceOpt::str("workspace"),
        ChoiceOpt::str("cwd"),
        ChoiceOpt::str("worktree"),
    ];
    let space_idx = space_opts
        .iter()
        .position(|o| o.label == v.space_kind)
        .unwrap_or(0);

    // -- space ref (workspace selector, else free text) ----------------------
    let is_workspace = v.space_kind == "workspace";
    let ref_matches_workspace = spaces.iter().any(|s| s.id == v.space_ref);
    let (space_ref_field, space_ref_custom_init) = if is_workspace && !spaces.is_empty() {
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
        Field::choice(FieldId::SpaceKind, "space", space_opts, space_idx),
        space_ref_field,
        space_ref_custom_field,
        Field::text(
            FieldId::WorktreeBase,
            "worktree base",
            &v.worktree_base,
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
