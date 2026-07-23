//! Public field and form value model.

use board_core::capability::HarnessCapabilities;
use board_core::model::Column;
use board_core::protocol::{
    CardCreateParams, CardUpdateParams, ColumnCreateParams, ColumnUpdateParams, SessionInfo,
    SpaceInfo,
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
    pub(super) fn str(label: &str) -> ChoiceOpt {
        ChoiceOpt {
            label: label.to_string(),
            val: ChoiceVal::Str(label.to_string()),
        }
    }
    pub(super) fn none() -> ChoiceOpt {
        ChoiceOpt {
            label: "none".to_string(),
            val: ChoiceVal::None,
        }
    }
    /// The "unset / harness default" option (extracts to `None`).
    pub(super) fn default_opt() -> ChoiceOpt {
        ChoiceOpt {
            label: "(default)".to_string(),
            val: ChoiceVal::None,
        }
    }
    /// The free-text escape hatch.
    pub(super) fn custom() -> ChoiceOpt {
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
    pub(super) fn text(id: FieldId, label: &'static str, initial: &str, multiline: bool) -> Field {
        Field {
            id,
            label,
            kind: FieldKind::Text(Box::new(new_textarea(initial))),
            multiline,
        }
    }

    pub(super) fn choice(
        id: FieldId,
        label: &'static str,
        opts: Vec<ChoiceOpt>,
        idx: usize,
    ) -> Field {
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

    pub(super) fn is_choice(&self) -> bool {
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
