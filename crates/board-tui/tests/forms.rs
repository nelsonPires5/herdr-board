use board_core::capability::{
    claude_capabilities, pi_capabilities, HarnessCapabilities, ModelInfo,
};
use board_core::protocol::Effort;
use board_tui::forms::{session_name_from_socket, Field, FieldId, FieldKind, Form, Submit};

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
