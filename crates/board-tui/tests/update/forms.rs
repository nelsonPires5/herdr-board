//! Form tests: field cycling, capabilities, model/effort/harness/space selectors.

use super::helpers::{
    demo_client, driver_of, is_choice, key, opt_labels, set_choice, split_effort_caps,
    RecordingClient,
};
use board_core::capability::{claude_capabilities, pi_capabilities};
use board_core::client::BoardClient;
use board_core::protocol::{Effort, Patch, SpaceInfo, SpaceKind};
use board_tui::app::{update, Effect, Screen};
use board_tui::forms::{ChoiceVal, FieldId, Form, Submit};
use crossterm::event::KeyCode;
use std::sync::{Arc, Mutex};

#[test]
fn editing_nullable_fields_emits_explicit_clears() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let mut card = board
        .cards
        .iter()
        .find(|card| card.model.is_some())
        .unwrap()
        .clone();
    // Include values that the demo card does not need for rendering, so this
    // test proves that clearing populated fields is intentional.
    card.session = Some("feature".into());
    card.space_cwd = Some("/repo".into());
    let mut card_form = Form::card_edit(&card);
    card_form
        .fields
        .iter_mut()
        .find(|field| field.id == FieldId::Model)
        .unwrap()
        .set_text("");
    set_choice(&mut card_form, FieldId::Effort, "(default)");
    set_choice(&mut card_form, FieldId::Permission, "(default)");
    set_choice(&mut card_form, FieldId::Session, "(default)");
    for id in [FieldId::SpaceRef, FieldId::SpaceCwd] {
        card_form
            .fields
            .iter_mut()
            .find(|field| field.id == id)
            .unwrap()
            .set_text("");
    }
    match card_form.submit().unwrap() {
        Submit::CardUpdate(params) => {
            assert!(matches!(params.model, Patch::Clear));
            assert!(matches!(params.effort, Patch::Clear));
            assert!(matches!(params.permission_mode, Patch::Clear));
            assert!(matches!(params.session, Patch::Clear));
            assert!(matches!(params.space_ref, Patch::Clear));
            assert!(matches!(params.space_cwd, Patch::Clear));
        }
        _ => panic!("expected card update"),
    }

    let mut column = board.columns.first().unwrap().clone();
    column.system_prompt = Some("instructions".into());
    column.on_success_column_id = Some(column.id);
    column.on_fail_column_id = Some(column.id);
    column.harness_override = Some("claude".into());
    column.model_override = Some("model".into());
    column.effort_override = Some("high".into());
    column.permission_override = Some("manual".into());
    column.timeout_minutes = Some(15);
    let mut column_form = Form::column_edit(&column, &[column.clone()]);
    for id in [
        FieldId::SystemPrompt,
        FieldId::ModelOverride,
        FieldId::Timeout,
    ] {
        column_form
            .fields
            .iter_mut()
            .find(|field| field.id == id)
            .unwrap()
            .set_text("");
    }
    set_choice(&mut column_form, FieldId::OnSuccess, "none");
    set_choice(&mut column_form, FieldId::OnFail, "none");
    set_choice(&mut column_form, FieldId::HarnessOverride, "none");
    set_choice(&mut column_form, FieldId::EffortOverride, "(default)");
    set_choice(&mut column_form, FieldId::PermissionOverride, "(default)");
    match column_form.submit().unwrap() {
        Submit::ColumnUpdate(params) => {
            assert!(matches!(params.system_prompt, Patch::Clear));
            assert!(matches!(params.on_success_column_id, Patch::Clear));
            assert!(matches!(params.on_fail_column_id, Patch::Clear));
            assert!(matches!(params.harness_override, Patch::Clear));
            assert!(matches!(params.model_override, Patch::Clear));
            assert!(matches!(params.effort_override, Patch::Clear));
            assert!(matches!(params.permission_override, Patch::Clear));
            assert!(matches!(params.timeout_minutes, Patch::Clear));
        }
        _ => panic!("expected column update"),
    }
}

#[test]
fn create_forms_submit_the_active_board_id() {
    let mut app = super::helpers::demo_app();
    app.board.board.id = 42;
    update(&mut app, key(KeyCode::Char('n')));
    app.form.as_mut().unwrap().fields[0].set_text("scoped card");
    let effects = update(&mut app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::CardCreate(params)] => assert_eq!(params.board_id, Some(42)),
        _ => panic!("expected scoped card create"),
    }

    update(&mut app, key(KeyCode::Char('N')));
    app.form.as_mut().unwrap().fields[0].set_text("Scoped column");
    let effects = update(&mut app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::ColumnCreate(params)] => assert_eq!(params.board_id, Some(42)),
        _ => panic!("expected scoped column create"),
    }
}

#[test]
fn form_field_cycling_wraps_and_skips_hidden() {
    let mut app = super::helpers::demo_app();
    update(&mut app, key(KeyCode::Char('n'))); // open new-card form
    assert_eq!(app.screen, Screen::CardForm);

    // Focus starts at Title (0). Tab advances; `cwd` is hidden while the space
    // kind is `workspace`, so the visible-field walk skips it.
    let start = app.form.as_ref().unwrap().focus;
    assert_eq!(start, 0);
    update(&mut app, key(KeyCode::Tab));
    assert_eq!(app.form.as_ref().unwrap().focus, 1);

    // BackTab from field 0 wraps to the last *visible* field.
    while app.form.as_ref().unwrap().focus != 0 {
        update(&mut app, key(KeyCode::BackTab));
    }
    update(&mut app, key(KeyCode::BackTab));
    let last = app.form.as_ref().unwrap().focus;
    assert!(app.form.as_ref().unwrap().field_visible(last));
    assert_ne!(
        app.form.as_ref().unwrap().fields[last].id,
        FieldId::SpaceCwd
    );
}

#[test]
fn cwd_visibility_follows_space_kind() {
    let mut form = Form::card_create(1);
    // Find the space-kind choice field and cycle it to "new workspace".
    let space_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::SpaceKind)
        .unwrap();
    let cwd_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::SpaceCwd)
        .unwrap();
    assert!(!form.field_visible(cwd_idx)); // hidden by default (workspace)
                                           // workspace -> new workspace
    form.fields[space_idx].cycle(1);
    assert!(form.field_visible(cwd_idx));
}

#[test]
fn space_kind_selector_has_exactly_two_options() {
    let form = Form::card_create(1);
    assert_eq!(
        opt_labels(&form, FieldId::SpaceKind),
        vec!["workspace", "new workspace"]
    );
}

#[test]
fn choice_cycling_wraps() {
    let mut form = Form::card_create(1);
    let eff_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::Effort)
        .unwrap();
    // Fallback effort menu (no catalog yet): (default)/low/medium/high/xhigh/max.
    // Cycle back one from 0 -> last.
    form.fields[eff_idx].cycle(-1);
    assert_eq!(form.fields[eff_idx].display(), "max");
    form.fields[eff_idx].cycle(1);
    assert_eq!(form.fields[eff_idx].display(), "(default)");
}

// -- Feature 1: guided card-form selectors -----------------------------------

#[test]
fn new_card_defaults_to_pi_and_lists_both_builtins() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.current_harness(), "pi");
    assert_eq!(opt_labels(form, FieldId::Harness), vec!["pi", "claude"]);
    assert_eq!(form.caps.as_ref().unwrap().harness, "pi");
}

#[test]
fn opening_column_form_loads_only_column_metadata() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut d = driver_of(RecordingClient {
        inner: demo_client().unwrap(),
        calls: Arc::clone(&calls),
    });
    calls.lock().unwrap().clear();

    d.handle(key(KeyCode::Char('N')));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(d.app.screen, Screen::ColumnForm);
    assert_eq!(form.caps.as_ref().unwrap().harness, "pi");
    assert_eq!(
        opt_labels(form, FieldId::HarnessOverride),
        vec!["none", "pi", "claude"]
    );
    assert!(form.spaces.is_empty(), "column forms do not load spaces");
    assert!(
        form.sessions.is_empty(),
        "column forms do not load sessions"
    );
    assert!(!form
        .fields
        .iter()
        .any(|field| field.id == FieldId::PermissionOverride
            && form.field_visible(form.fields.iter().position(|f| f.id == field.id).unwrap())));
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["harness.capabilities", "harness.list"]
    );

    // Edit follows the same metadata path; a newly opened form starts at Name.
    d.handle(key(KeyCode::Esc));
    calls.lock().unwrap().clear();
    d.handle(key(KeyCode::Char('E')));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.caps.as_ref().unwrap().harness, "pi");
    assert_eq!(form.fields[form.focus].id, FieldId::Name);
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["harness.capabilities", "harness.list"]
    );
}

#[test]
fn opening_card_form_fetches_capabilities_and_spaces() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    assert_eq!(d.app.screen, Screen::CardForm);
    let form = d.app.form.as_ref().unwrap();
    assert!(form.caps.is_some(), "capabilities fetched");
    assert!(!form.spaces.is_empty(), "spaces fetched");
    // Model became a guided selector (was free text before the fetch).
    assert!(is_choice(form, FieldId::Model));
    // Effort menu starts with the "unset" sentinel.
    assert_eq!(opt_labels(form, FieldId::Effort)[0], "(default)");
}

#[test]
fn fetch_failure_falls_back_to_free_text() {
    let client = demo_client().unwrap().without_caps().without_spaces();
    let mut d = driver_of(client);
    d.handle(key(KeyCode::Char('n')));
    let form = d.app.form.as_ref().unwrap();
    assert!(form.caps.is_none());
    assert!(form.spaces.is_empty());
    // Model + space ref stay free text.
    assert!(!is_choice(form, FieldId::Model));
    assert!(!is_choice(form, FieldId::SpaceRef));
    // ...and the user is warned.
    assert!(d.app.toast.as_ref().is_some_and(|t| t.is_error));
}

#[test]
fn pi_form_defaults_model_hides_permission_and_offers_low() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(pi_capabilities()), None, Some(vec![]), None);
    assert_eq!(
        opt_labels(&form, FieldId::Model),
        vec!["(default)", "(custom)"]
    );
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::Model)
            .unwrap()
            .display(),
        "(default)"
    );
    assert!(opt_labels(&form, FieldId::Effort).contains(&"low".to_string()));
    let permission_idx = form
        .fields
        .iter()
        .position(|field| field.id == FieldId::Permission)
        .unwrap();
    assert!(!form.field_visible(permission_idx));
}

#[test]
fn switching_harness_reloads_capabilities() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    let form = d.app.form.as_mut().unwrap();
    form.focus = form
        .fields
        .iter()
        .position(|field| field.id == FieldId::Harness)
        .unwrap();
    d.handle(key(KeyCode::Right));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.current_harness(), "claude");
    assert_eq!(form.caps.as_ref().unwrap().harness, "claude");
    assert!(opt_labels(form, FieldId::Model).contains(&"sonnet".to_string()));
    let permission_idx = form
        .fields
        .iter()
        .position(|field| field.id == FieldId::Permission)
        .unwrap();
    assert!(form.field_visible(permission_idx));
}

#[test]
fn switching_from_claude_to_pi_resets_only_permission() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(claude_capabilities()), None, Some(vec![]), None);
    set_choice(&mut form, FieldId::Harness, "claude");
    set_choice(&mut form, FieldId::Model, "sonnet");
    set_choice(&mut form, FieldId::Effort, "high");
    set_choice(&mut form, FieldId::Permission, "acceptEdits");
    set_choice(&mut form, FieldId::Harness, "pi");
    form.apply_options(Some(pi_capabilities()), None, Some(vec![]), None);

    assert_eq!(form.current_harness(), "pi");
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::Model)
            .unwrap()
            .display(),
        "(custom)"
    );
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::ModelCustom)
            .unwrap()
            .get_text(),
        "sonnet"
    );
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::Effort)
            .unwrap()
            .display(),
        "high"
    );
    form.fields[0].set_text("switch");
    match form.submit().unwrap() {
        Submit::CardCreate(params) => assert!(params.permission_mode.is_none()),
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn switching_from_pi_to_claude_resets_incompatible_effort() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(pi_capabilities()), None, Some(vec![]), None);
    set_choice(&mut form, FieldId::Effort, "off");
    set_choice(&mut form, FieldId::Harness, "claude");
    form.apply_options(Some(claude_capabilities()), None, Some(vec![]), None);
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::Effort)
            .unwrap()
            .display(),
        "(default)"
    );
}

#[test]
fn pi_submit_carries_custom_model_low_and_no_permission() {
    let mut form = Form::card_create(7);
    form.apply_options(Some(pi_capabilities()), None, Some(vec![]), None);
    form.fields[0].set_text("pi task");
    set_choice(&mut form, FieldId::Model, "(custom)");
    form.fields
        .iter_mut()
        .find(|field| field.id == FieldId::ModelCustom)
        .unwrap()
        .set_text("openai-codex/example");
    form.on_model_changed();
    set_choice(&mut form, FieldId::Effort, "low");

    match form.submit().unwrap() {
        Submit::CardCreate(params) => {
            assert_eq!(params.harness.as_deref(), Some("pi"));
            assert_eq!(params.model.as_deref(), Some("openai-codex/example"));
            assert_eq!(params.effort, Some(Effort::Low));
            assert!(params.permission_mode.is_none());
        }
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn model_selector_cycles_catalog_plus_custom() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(split_effort_caps()), None, Some(vec![]), None);
    assert_eq!(
        opt_labels(&form, FieldId::Model),
        vec!["(default)", "opus", "haiku", "(custom)"]
    );
}

#[test]
fn effort_options_follow_selected_model_and_reset_when_invalid() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(split_effort_caps()), None, Some(vec![]), None);
    // `(default)` preserves an omitted model; selecting opus narrows efforts.
    set_choice(&mut form, FieldId::Model, "opus");
    form.on_model_changed();
    assert_eq!(
        opt_labels(&form, FieldId::Effort),
        vec!["(default)", "low", "high"]
    );
    // Pick a valid effort for opus, then switch to haiku (efforts: medium).
    set_choice(&mut form, FieldId::Effort, "high");
    set_choice(&mut form, FieldId::Model, "haiku");
    form.on_model_changed();
    assert_eq!(
        opt_labels(&form, FieldId::Effort),
        vec!["(default)", "medium"]
    );
    // "high" is invalid for haiku -> effort reset to the default sentinel.
    let eff = form
        .fields
        .iter()
        .find(|f| f.id == FieldId::Effort)
        .unwrap();
    assert_eq!(eff.display(), "(default)");
}

#[test]
fn effort_kept_when_still_valid_across_model_change() {
    let mut caps = split_effort_caps();
    // Give both models `low`.
    caps.models[1].efforts = vec![Effort::Low, Effort::Medium];
    let mut form = Form::card_create(1);
    form.apply_options(Some(caps), None, Some(vec![]), None);
    set_choice(&mut form, FieldId::Effort, "low");
    set_choice(&mut form, FieldId::Model, "haiku");
    form.on_model_changed();
    let eff = form
        .fields
        .iter()
        .find(|f| f.id == FieldId::Effort)
        .unwrap();
    assert_eq!(eff.display(), "low"); // still valid -> preserved
}

#[test]
fn custom_model_reveals_free_text_and_submits_it() {
    let mut form = Form::card_create(7);
    form.apply_options(Some(split_effort_caps()), None, Some(vec![]), None);
    form.fields[0].set_text("t"); // title required
    set_choice(&mut form, FieldId::Model, "(custom)");
    form.on_model_changed();
    assert_eq!(
        form.fields
            .iter()
            .find(|field| field.id == FieldId::Model)
            .unwrap()
            .display(),
        "(custom)"
    );
    // ModelCustom becomes visible; type an arbitrary id.
    let mc = form
        .fields
        .iter_mut()
        .find(|f| f.id == FieldId::ModelCustom)
        .unwrap();
    mc.set_text("claude-opus-4-8[1m]");
    let mc_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::ModelCustom)
        .unwrap();
    assert!(form.field_visible(mc_idx));
    match form.submit().unwrap() {
        Submit::CardCreate(p) => assert_eq!(p.model.as_deref(), Some("claude-opus-4-8[1m]")),
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn space_selector_shows_label_but_stores_id() {
    let spaces = vec![
        SpaceInfo {
            id: "w4".to_string(),
            label: "MELI scraper".to_string(),
        },
        SpaceInfo {
            id: "w1".to_string(),
            label: "auth refactor".to_string(),
        },
    ];
    let mut form = Form::card_create(1);
    form.apply_options(Some(claude_capabilities()), None, Some(spaces), None);
    // space_kind defaults to workspace -> the ref is a selector.
    assert!(is_choice(&form, FieldId::SpaceRef));
    let labels = opt_labels(&form, FieldId::SpaceRef);
    assert_eq!(labels[0], "MELI scraper (w4)");
    assert_eq!(labels.last().unwrap(), "(custom)");
    // Default selection (first workspace) submits the id, not the label.
    form.fields[0].set_text("t");
    match form.submit().unwrap() {
        Submit::CardCreate(p) => assert_eq!(p.space_ref.as_deref(), Some("w4")),
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn space_custom_escape_hatch_stores_free_text() {
    let spaces = vec![SpaceInfo {
        id: "w4".to_string(),
        label: "MELI scraper".to_string(),
    }];
    let mut form = Form::card_create(1);
    form.apply_options(Some(claude_capabilities()), None, Some(spaces), None);
    form.fields[0].set_text("t");
    set_choice(&mut form, FieldId::SpaceRef, "(custom)");
    form.fields
        .iter_mut()
        .find(|f| f.id == FieldId::SpaceRefCustom)
        .unwrap()
        .set_text("w99");
    match form.submit().unwrap() {
        Submit::CardCreate(p) => assert_eq!(p.space_ref.as_deref(), Some("w99")),
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn editing_card_preselects_matching_workspace() {
    // The seeded running card in Plan has space_ref "w4" (a demo workspace).
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Right)); // Plan
    d.handle(key(KeyCode::Char('e')));
    let form = d.app.form.as_ref().unwrap();
    assert!(is_choice(form, FieldId::SpaceRef));
    let f = form
        .fields
        .iter()
        .find(|f| f.id == FieldId::SpaceRef)
        .unwrap();
    assert!(matches!(f.choice_val(), Some(ChoiceVal::Str(s)) if s == "w4"));
    assert_eq!(f.display(), "MELI scraper (w4)");
}

#[test]
fn changing_space_kind_toggles_selector_and_free_text() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    // Workspace (default) -> space ref is a selector.
    assert!(is_choice(d.app.form.as_ref().unwrap(), FieldId::SpaceRef));
    // Navigate focus to the space-kind field and cycle to `new workspace`.
    let form = d.app.form.as_mut().unwrap();
    form.focus = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::SpaceKind)
        .unwrap();
    d.handle(key(KeyCode::Right)); // cycle space kind: workspace -> new workspace
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(
        form.fields
            .iter()
            .find(|f| f.id == FieldId::SpaceKind)
            .unwrap()
            .display(),
        "new workspace"
    );
    // new_workspace -> the space ref becomes a free-text `workspace name`.
    assert!(!is_choice(form, FieldId::SpaceRef));
    // ...and the `cwd` field is now visible.
    let cwd_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::SpaceCwd)
        .unwrap();
    assert!(form.field_visible(cwd_idx));
}

#[test]
fn new_workspace_submit_carries_name_and_cwd() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(claude_capabilities()), None, Some(vec![]), None);
    form.fields[0].set_text("t"); // title required
    set_choice(&mut form, FieldId::SpaceKind, "new workspace");
    form.on_space_kind_changed();
    // Both space ref (label) and cwd are now plain text fields.
    assert!(!is_choice(&form, FieldId::SpaceRef));
    form.fields
        .iter_mut()
        .find(|f| f.id == FieldId::SpaceRef)
        .unwrap()
        .set_text("my-feature");
    form.fields
        .iter_mut()
        .find(|f| f.id == FieldId::SpaceCwd)
        .unwrap()
        .set_text("/repo/feature");
    match form.submit().unwrap() {
        Submit::CardCreate(p) => {
            assert_eq!(p.space_kind, Some(SpaceKind::NewWorkspace));
            assert_eq!(p.space_ref.as_deref(), Some("my-feature"));
            assert_eq!(p.space_cwd.as_deref(), Some("/repo/feature"));
        }
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn changing_session_refetches_spaces() {
    // The demo client returns different workspaces per session, so switching the
    // session field re-scopes the space selector.
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    // Default session -> demo_spaces (MELI scraper / auth refactor / docs site).
    let before = opt_labels(d.app.form.as_ref().unwrap(), FieldId::SpaceRef);
    assert!(before.iter().any(|l| l.contains("MELI scraper")));
    // Focus the session field and cycle to "feature".
    let form = d.app.form.as_mut().unwrap();
    form.focus = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::Session)
        .unwrap();
    // Session options are [(default), default, feature]; cycle to "feature".
    d.handle(key(KeyCode::Right)); // (default) -> default (re-fetch)
    d.handle(key(KeyCode::Right)); // default -> feature (re-fetch)
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(
        form.fields
            .iter()
            .find(|f| f.id == FieldId::Session)
            .unwrap()
            .display(),
        "feature"
    );
    let after = opt_labels(form, FieldId::SpaceRef);
    assert!(
        after.iter().any(|l| l.contains("feature sandbox")),
        "space list re-scoped to the feature session; got {after:?}"
    );
}

#[test]
fn session_selector_offers_default_plus_running() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    let labels = opt_labels(d.app.form.as_ref().unwrap(), FieldId::Session);
    // (default) first, then the running demo sessions.
    assert_eq!(labels[0], "(default)");
    assert!(labels.contains(&"default".to_string()));
    assert!(labels.contains(&"feature".to_string()));
}
