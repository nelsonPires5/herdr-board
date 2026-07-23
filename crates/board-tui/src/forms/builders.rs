//! Form construction and field builders.

use board_core::capability::{efforts_for, HarnessCapabilities};
use board_core::harness::{BUILTIN_HARNESSES, DEFAULT_HARNESS};
use board_core::model::{Card, Column};
use board_core::protocol::{Effort, SessionInfo, SpaceInfo};

use super::{ChoiceOpt, ChoiceVal, Field, FieldId, Form, FormKind};

const EFFORT_ORDER: [Effort; 7] = [
    Effort::Off,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

const FALLBACK_PERMISSION_MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

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
    pub fn title(&self) -> &'static str {
        match self.kind {
            FormKind::CardCreate { .. } => "New card",
            FormKind::CardEdit { .. } => "Edit card",
            FormKind::ColumnCreate => "New column",
            FormKind::ColumnEdit { .. } => "Edit column",
            FormKind::Comment { .. } => "Add comment",
        }
    }
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
pub(super) struct CardValues {
    pub(super) title: String,
    pub(super) description: String,
    pub(super) harness: String,
    /// Effective model string ("" = none / catalog default).
    pub(super) model: String,
    /// Keep an empty `(custom)` selection stable while its companion field is
    /// first revealed.
    pub(super) model_custom_selected: bool,
    /// Effort wire string, or `None` for the harness default.
    pub(super) effort: Option<String>,
    /// Permission-mode wire string, or `None` for the harness default.
    pub(super) permission: Option<String>,
    /// Selected herdr session name, or `None` for the daemon's default session.
    pub(super) session: Option<String>,
    pub(super) space_kind: String,
    /// Effective space ref (workspace id, or new-workspace label / free text).
    pub(super) space_ref: String,
    /// Working directory for a `new_workspace` space ("" = unset).
    pub(super) space_cwd: String,
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
pub(super) fn build_card_fields(
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
pub(super) struct ColumnValues {
    pub(super) name: String,
    pub(super) system_prompt: String,
    /// Trigger wire string (`None` = default manual).
    pub(super) trigger: Option<String>,
    pub(super) on_success: Option<i64>,
    pub(super) on_fail: Option<i64>,
    pub(super) fresh_session: Option<bool>,
    /// Selected override harness (`None` = no override / column default).
    pub(super) harness_override: Option<String>,
    /// Free-text model override (`None` = unset).
    pub(super) model_override: Option<String>,
    pub(super) effort_override: Option<String>,
    pub(super) permission_override: Option<String>,
    pub(super) timeout: Option<i64>,
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
pub(super) fn column_fields_from_values(
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
