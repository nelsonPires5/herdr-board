//! Unit tests for the pure reducer: navigation wrapping, form field cycling,
//! and drag-state transitions.

use board_core::capability::{HarnessCapabilities, ModelInfo};
use board_core::client::BoardClient;
use board_core::protocol::{Effort, SpaceInfo};
use board_tui::app::{update, App, Effect, Msg, Screen};
use board_tui::editor::FakeEditor;
use board_tui::forms::{ChoiceVal, FieldId, FieldKind, Form, Submit};
use board_tui::testkit::demo_client;
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::empty()))
}

fn demo_app() -> App {
    let mut c = demo_client().unwrap();
    App::new(c.board_get().unwrap())
}

fn driver_of<C: BoardClient + 'static>(client: C) -> Driver {
    Driver::with_editor(Box::new(client), Box::new(FakeEditor::new("x"))).unwrap()
}

/// A two-model catalog where the models carry *different* effort sets, so tests
/// can observe the effort menu tracking the selected model.
fn split_effort_caps() -> HarnessCapabilities {
    HarnessCapabilities {
        harness: "claude".to_string(),
        models: vec![
            ModelInfo {
                id: "opus".to_string(),
                efforts: vec![Effort::Low, Effort::High],
            },
            ModelInfo {
                id: "haiku".to_string(),
                efforts: vec![Effort::Medium],
            },
        ],
        model_freeform: true,
        permission_modes: vec!["manual".to_string()],
    }
}

/// Labels of a choice field's options.
fn opt_labels(form: &Form, id: FieldId) -> Vec<String> {
    match &form.fields.iter().find(|f| f.id == id).unwrap().kind {
        FieldKind::Choice { opts, .. } => opts.iter().map(|o| o.label.clone()).collect(),
        FieldKind::Text(_) => panic!("{id:?} is not a choice"),
    }
}

fn set_choice(form: &mut Form, id: FieldId, label: &str) {
    let f = form.fields.iter_mut().find(|f| f.id == id).unwrap();
    if let FieldKind::Choice { opts, idx } = &mut f.kind {
        *idx = opts.iter().position(|o| o.label == label).unwrap();
    } else {
        panic!("{id:?} is not a choice");
    }
}

fn is_choice(form: &Form, id: FieldId) -> bool {
    matches!(
        form.fields.iter().find(|f| f.id == id).unwrap().kind,
        FieldKind::Choice { .. }
    )
}

#[test]
fn column_navigation_wraps() {
    let mut app = demo_app();
    let n = app.board.columns.len();
    assert!(n >= 2);
    assert_eq!(app.sel_col, 0);
    // left from first wraps to last
    update(&mut app, key(KeyCode::Left));
    assert_eq!(app.sel_col, n - 1);
    // right from last wraps to first
    update(&mut app, key(KeyCode::Right));
    assert_eq!(app.sel_col, 0);
    // hjkl aliases
    update(&mut app, key(KeyCode::Char('l')));
    assert_eq!(app.sel_col, 1);
    update(&mut app, key(KeyCode::Char('h')));
    assert_eq!(app.sel_col, 0);
}

#[test]
fn card_navigation_wraps_within_column() {
    let mut app = demo_app();
    // Move to Execute (index 2): it has 2 cards.
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right));
    let col_id = app.col_id_at(app.sel_col).unwrap();
    let count = app.cards_of(col_id).len();
    assert_eq!(count, 2);
    assert_eq!(app.sel_card, 0);
    update(&mut app, key(KeyCode::Up)); // wrap to last
    assert_eq!(app.sel_card, count - 1);
    update(&mut app, key(KeyCode::Down)); // wrap to first
    assert_eq!(app.sel_card, 0);
}

#[test]
fn switching_columns_clamps_card_index() {
    let mut app = demo_app();
    // Execute has 2 cards; select the 2nd.
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.sel_card, 1);
    // Move to Human Review (index 4) which is empty -> clamps to 0.
    update(&mut app, key(KeyCode::Right)); // Review
    update(&mut app, key(KeyCode::Right)); // Human Review
    assert_eq!(app.sel_card, 0);
}

#[test]
fn form_field_cycling_wraps_and_skips_hidden() {
    let mut app = demo_app();
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

#[test]
fn drag_card_to_other_column_produces_move() {
    let mut app = demo_app();
    // Grab the running card in Plan (column index 1).
    let plan_id = app.col_id_at(1).unwrap();
    let card_id = app.cards_of(plan_id)[0].id;
    app.begin_card_drag(card_id, 1);
    // Hover the same column -> no effect on finish.
    app.drag_hover(1);
    // Hover Execute (index 2) then drop.
    app.drag_hover(2);
    let effects = app.finish_drag();
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::CardMove(p) => {
            assert_eq!(p.id, card_id);
            assert_eq!(p.column_id, app.col_id_at(2).unwrap());
        }
        _ => panic!("expected CardMove"),
    }
    assert!(app.drag.is_none(), "drag cleared after finish");
}

#[test]
fn drag_dropped_on_origin_is_noop() {
    let mut app = demo_app();
    app.begin_card_drag(42, 1);
    app.drag_hover(1);
    let effects = app.finish_drag();
    assert!(effects.is_empty());
    assert!(app.drag.is_none());
}

#[test]
fn column_drag_produces_reorder() {
    let mut app = demo_app();
    let col_id = app.col_id_at(1).unwrap();
    app.begin_column_drag(col_id, 1);
    app.drag_hover(3);
    let effects = app.finish_drag();
    match &effects[0] {
        Effect::ColumnReorder { id, position } => {
            assert_eq!(*id, col_id);
            assert_eq!(*position, 3);
        }
        _ => panic!("expected ColumnReorder"),
    }
}

#[test]
fn shove_moves_card_and_focus() {
    let mut app = demo_app();
    // Focus Plan's running card.
    update(&mut app, key(KeyCode::Right));
    let card_id = app.selected_card_id().unwrap();
    let effects = update(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.sel_col, 2); // moved focus to Execute
    match &effects[0] {
        Effect::CardMove(p) => assert_eq!(p.id, card_id),
        _ => panic!("expected CardMove"),
    }
}

#[test]
fn template_only_on_empty_board() {
    // Empty board -> T applies template.
    let mut c = board_core::client::FakeBoardClient::new().unwrap();
    let mut app = App::new(c.board_get().unwrap());
    assert!(app.is_empty_board());
    let effects = update(&mut app, key(KeyCode::Char('T')));
    assert!(matches!(effects.as_slice(), [Effect::TemplateApply(_)]));

    // Non-empty board -> T just toasts, no effect.
    let mut app2 = demo_app();
    let effects2 = update(&mut app2, key(KeyCode::Char('T')));
    assert!(effects2.is_empty());
    assert!(app2.toast.is_some());
}

#[test]
fn help_and_quit() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.screen, Screen::Board);
    let effects = update(&mut app, key(KeyCode::Char('q')));
    assert!(matches!(effects.as_slice(), [Effect::Quit]));
}

// -- Feature 2: `r` refresh --------------------------------------------------

#[test]
fn r_refreshes_board_with_toast() {
    let mut app = demo_app();
    let effects = update(&mut app, key(KeyCode::Char('r')));
    assert!(matches!(effects.as_slice(), [Effect::Refetch]));
    assert_eq!(app.toast.as_ref().unwrap().text, "refreshed");
    assert!(!app.toast.as_ref().unwrap().is_error);
}

#[test]
fn r_reloads_through_the_driver() {
    // The seeded board has a "Todo" card; deleting it out-of-band and then
    // pressing `r` should reload state from the client.
    let mut d = driver_of(demo_client().unwrap());
    let before = d.app.board.cards.len();
    let victim = d.app.board.cards[0].id;
    d.app.board.cards.retain(|c| c.id != victim); // desync local state
    assert_eq!(d.app.board.cards.len(), before - 1);
    d.handle(key(KeyCode::Char('r')));
    assert_eq!(d.app.board.cards.len(), before); // refetched from client
}

// -- Feature 1: guided card-form selectors -----------------------------------

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
fn model_selector_cycles_catalog_plus_custom() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(split_effort_caps()), Some(vec![]), None);
    assert_eq!(
        opt_labels(&form, FieldId::Model),
        vec!["opus", "haiku", "(custom)"]
    );
}

#[test]
fn effort_options_follow_selected_model_and_reset_when_invalid() {
    let mut form = Form::card_create(1);
    form.apply_options(Some(split_effort_caps()), Some(vec![]), None);
    // Default model = opus -> efforts low/high.
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
    form.apply_options(Some(caps), Some(vec![]), None);
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
    form.apply_options(Some(split_effort_caps()), Some(vec![]), None);
    form.fields[0].set_text("t"); // title required
    set_choice(&mut form, FieldId::Model, "(custom)");
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
    form.apply_options(
        Some(board_core::capability::claude_capabilities()),
        Some(spaces),
        None,
    );
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
    form.apply_options(
        Some(board_core::capability::claude_capabilities()),
        Some(spaces),
        None,
    );
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
    form.apply_options(
        Some(board_core::capability::claude_capabilities()),
        Some(vec![]),
        None,
    );
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
            assert_eq!(
                p.space_kind,
                Some(board_core::protocol::SpaceKind::NewWorkspace)
            );
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
