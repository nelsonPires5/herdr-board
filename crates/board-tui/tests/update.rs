//! Unit tests for the pure reducer: navigation wrapping, form field cycling,
//! and drag-state transitions.

use board_core::capability::{
    claude_capabilities, pi_capabilities, HarnessCapabilities, ModelInfo,
};
use board_core::client::BoardClient;
use board_core::protocol::{CardStatus, Effort, Patch, RunOutcome, SpaceInfo};
use board_tui::app::{update, App, CardFilter, DetailScrollTarget, Effect, Msg, Screen};
use board_tui::editor::FakeEditor;
use board_tui::forms::{ChoiceVal, FieldId, FieldKind, Form, FormKind, Submit};
use board_tui::testkit::demo_client;
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

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
        default_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
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
fn board_layout_always_fills_available_width() {
    let mut app = demo_app();

    let area = Rect::new(0, 0, 121, 35);
    let layout = board_tui::view::board_layout(&app, area);
    assert_eq!(layout.cols.first().unwrap().rect.x, area.x);
    let last = layout.cols.last().unwrap().rect;
    assert_eq!(last.x + last.width, area.x + area.width);

    // When not every column fits, the selected column drives a full-width
    // window rather than leaving a partial final page.
    app.sel_col = app.board.columns.len() - 1;
    let layout = board_tui::view::board_layout(&app, area);
    assert_eq!(layout.cols.last().unwrap().idx, app.sel_col);
    let last = layout.cols.last().unwrap().rect;
    assert_eq!(last.x + last.width, area.x + area.width);

    // A single-column board also consumes the entire viewport.
    let mut client = board_core::client::FakeBoardClient::new().unwrap();
    let single = App::new(client.board_get().unwrap());
    let layout = board_tui::view::board_layout(&single, area);
    assert_eq!(layout.cols[0].rect.width, area.width);
}

#[test]
fn board_picker_loads_and_switch_preserves_filter() {
    let mut app = demo_app();
    let effects = update(&mut app, key(KeyCode::Char('b')));
    assert!(matches!(effects.as_slice(), [Effect::LoadBoards]));

    let mut driver = driver_of(demo_client().unwrap());
    driver.handle(key(KeyCode::Char('v')));
    assert_eq!(driver.app.card_filter, CardFilter::All);
    driver.handle(key(KeyCode::Char('b')));
    assert_eq!(driver.app.screen, Screen::Picker);
    assert!(driver.app.picker.as_ref().unwrap().options.len() >= 3);
    driver.handle(key(KeyCode::Down));
    driver.handle(key(KeyCode::Enter));
    assert_eq!(driver.app.screen, Screen::Board);
    assert_eq!(driver.app.card_filter, CardFilter::All);
    assert_eq!(driver.app.sel_col, 0);
    assert_eq!(
        driver.app.board.board.scope_path.as_deref(),
        Some("/Volumes/archive/project")
    );
    driver.handle(Msg::Refresh);
    assert_eq!(
        driver.app.board.board.scope_path.as_deref(),
        Some("/Volumes/archive/project")
    );
}

#[test]
fn create_forms_submit_the_active_board_id() {
    let mut app = demo_app();
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
fn archive_filter_defaults_active_and_cycles_all_then_archived() {
    let mut client = demo_client().unwrap();
    let done = client
        .board_get()
        .unwrap()
        .columns
        .into_iter()
        .find(|column| column.name == "Done")
        .unwrap();
    let card_ids: Vec<i64> = client
        .card_list(Some(done.id))
        .unwrap()
        .iter()
        .map(|card| card.id)
        .collect();
    for id in card_ids {
        client.card_archive(id, true).unwrap();
    }
    let mut app = App::new(client.board_get().unwrap());

    assert_eq!(app.card_filter, CardFilter::Active);
    assert!(app.cards_of(done.id).is_empty());

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::All)]
    ));
    assert_eq!(app.card_filter, CardFilter::All);
    assert_eq!(app.cards_of(done.id).len(), 2);

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::Archived)]
    ));
    assert_eq!(app.card_filter, CardFilter::Archived);
    assert_eq!(app.cards_of(done.id).len(), 2);

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::Active)]
    ));
    assert_eq!(app.card_filter, CardFilter::Active);
}

#[test]
fn archive_shortcut_archives_and_restores_selected_card() {
    let mut app = demo_app();
    let card_id = app.selected_card_id().unwrap();
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { id, archived: true }] if *id == card_id
    ));

    app.board
        .cards
        .iter_mut()
        .find(|card| card.id == card_id)
        .unwrap()
        .archived_at = Some("2026-07-14 12:00:00".into());
    app.card_filter = CardFilter::Archived;
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { id, archived: false }] if *id == card_id
    ));
}

#[test]
fn archived_card_must_be_restored_before_moving() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let done_idx = board
        .columns
        .iter()
        .position(|column| column.name == "Done")
        .unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.column_id == board.columns[done_idx].id)
        .unwrap();
    client.card_archive(card.id, true).unwrap();
    let mut app = App::new(client.board_get().unwrap());
    app.card_filter = CardFilter::All;
    app.sel_col = done_idx;

    let effects = update(&mut app, key(KeyCode::Char('m')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::Board);
    assert!(app.toast.as_ref().is_some_and(|toast| {
        toast.is_error && toast.text.contains("restore archived card before moving")
    }));
}

#[test]
fn deleting_column_accounts_for_archived_cards_hidden_by_filter() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let done_idx = board
        .columns
        .iter()
        .position(|column| column.name == "Done")
        .unwrap();
    let card_ids: Vec<i64> = board
        .cards
        .iter()
        .filter(|card| card.column_id == board.columns[done_idx].id)
        .map(|card| card.id)
        .collect();
    for id in card_ids {
        client.card_archive(id, true).unwrap();
    }
    let mut app = App::new(client.board_get().unwrap());
    app.sel_col = done_idx;
    assert!(app.cards_of(board.columns[done_idx].id).is_empty());

    update(&mut app, key(KeyCode::Char('D')));
    assert_eq!(app.screen, Screen::Picker);
}

#[test]
fn archive_shortcut_rejects_busy_card() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Right)); // Plan's running card
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|toast| {
        toast.is_error && toast.text.contains("cancel it before archiving")
    }));
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
fn card_detail_o_emits_focus_and_driver_quits_only_on_success() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let running = board
        .cards
        .iter()
        .find(|card| card.status == board_core::protocol::CardStatus::Running)
        .unwrap()
        .clone();
    let mut app = App::new(board);
    app.screen = Screen::CardDetail;
    app.detail = Some(client.card_get(running.id).unwrap());
    let effects = update(&mut app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(), [Effect::FocusRun(id)] if *id == running.id));

    let mut success = driver_of(demo_client().unwrap());
    success.set_origin_socket(Some("/tmp/herdr.sock".into()));
    success.handle(key(KeyCode::Right));
    success.handle(key(KeyCode::Enter));
    success.handle(key(KeyCode::Char('o')));
    assert!(success.app.should_quit);

    let mut error = driver_of(demo_client().unwrap());
    error.set_origin_socket(Some("/tmp/herdr.sock".into()));
    error.handle(key(KeyCode::Enter));
    error.handle(key(KeyCode::Char('o')));
    assert!(!error.app.should_quit);
    assert!(error.app.toast.as_ref().is_some_and(|toast| toast.is_error));

    let mut no_herdr = driver_of(demo_client().unwrap());
    no_herdr.set_origin_socket(None);
    no_herdr.handle(key(KeyCode::Right));
    no_herdr.handle(key(KeyCode::Enter));
    no_herdr.handle(key(KeyCode::Char('o')));
    assert!(!no_herdr.app.should_quit);
    assert!(no_herdr
        .app
        .toast
        .as_ref()
        .is_some_and(|toast| toast.text.contains("requires Herdr")));
}

#[test]
fn card_detail_toggles_popup_and_fullscreen() {
    let mut app = demo_app();
    app.screen = Screen::CardDetail;
    assert!(!app.detail_fullscreen);

    update(&mut app, key(KeyCode::Char('f')));
    assert!(app.detail_fullscreen);
    update(&mut app, key(KeyCode::Char('f')));
    assert!(!app.detail_fullscreen);
}

#[test]
fn card_detail_edit_opens_form_and_returns_to_detail() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card_id = board.cards[0].id;
    let detail = client.card_get(card_id).unwrap();
    let mut app = App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;

    let effects = update(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.screen, Screen::CardForm);
    assert!(matches!(
        app.form.as_ref().map(|form| form.kind),
        Some(FormKind::CardEdit { card_id: id }) if id == card_id
    ));
    assert!(matches!(effects.as_slice(), [Effect::LoadFormOptions]));

    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn card_detail_scrolls_comments_and_runs_independently() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == board_core::protocol::CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("extra comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
            .unwrap();
        client.db().start_run(run.id, None, None).unwrap();
        client
            .db()
            .finish_run(run.id, RunOutcome::Ok, Some("done"))
            .unwrap();
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;

    update(&mut app, key(KeyCode::Down));
    assert!(app.detail_comments_scroll > 0);
    assert_eq!(app.detail_runs_scroll, 0);

    let comments_scroll = app.detail_comments_scroll;
    update(&mut app, key(KeyCode::Tab));
    assert_eq!(app.detail_scroll_target, DetailScrollTarget::Runs);
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.detail_comments_scroll, comments_scroll);
    assert!(app.detail_runs_scroll > 0);
}

#[test]
fn opening_detail_starts_comments_and_runs_at_latest() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == board_core::protocol::CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
            .unwrap();
        client.db().start_run(run.id, None, None).unwrap();
        client
            .db()
            .finish_run(run.id, RunOutcome::Ok, Some("done"))
            .unwrap();
    }
    let mut driver = driver_of(client);
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Enter));

    let detail = driver.app.detail.as_ref().unwrap();
    let layout = board_tui::view::detail_layout(&driver.app, driver.app.last_area);
    let comments_visible = layout.comments.height.saturating_sub(1) as usize;
    let runs_visible = layout.runs.height.saturating_sub(1) as usize;
    assert_eq!(
        driver.app.detail_comments_scroll + comments_visible,
        detail.comments.len()
    );
    assert_eq!(
        driver.app.detail_runs_scroll + runs_visible,
        detail.runs.len()
    );
    assert_eq!(
        detail.comments.last().unwrap().body,
        "comment 19",
        "comments remain oldest-to-newest"
    );
}

#[test]
fn shrinking_detail_to_popup_reanchors_history_to_latest() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == board_core::protocol::CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
            .unwrap();
        client.db().start_run(run.id, None, None).unwrap();
        client
            .db()
            .finish_run(run.id, RunOutcome::Ok, Some("done"))
            .unwrap();
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = App::new(board);
    app.last_area = Rect::new(0, 0, 254, 67);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_fullscreen = true;
    app.scroll_detail_to_latest();

    update(&mut app, key(KeyCode::Char('f')));

    let detail = app.detail.as_ref().unwrap();
    let layout = board_tui::view::detail_layout(&app, app.last_area);
    let comments_visible = layout.comments.height.saturating_sub(1) as usize;
    let runs_visible = layout.runs.height.saturating_sub(1) as usize;
    assert_eq!(
        app.detail_comments_scroll + comments_visible,
        detail.comments.len()
    );
    assert_eq!(app.detail_runs_scroll + runs_visible, detail.runs.len());
}

#[test]
fn card_detail_title_action_is_clickable() {
    let mut app = demo_app();
    app.screen = Screen::CardDetail;
    let button = board_tui::view::detail_toggle_rect(&app, app.last_area);

    update(
        &mut app,
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::empty(),
        }),
    );

    assert!(app.detail_fullscreen);
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
fn new_card_defaults_to_pi_and_lists_both_builtins() {
    let mut d = driver_of(demo_client().unwrap());
    d.handle(key(KeyCode::Char('n')));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.current_harness(), "pi");
    assert_eq!(opt_labels(form, FieldId::Harness), vec!["pi", "claude"]);
    assert_eq!(form.caps.as_ref().unwrap().harness, "pi");
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
    form.apply_options(
        Some(board_core::capability::claude_capabilities()),
        None,
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
        None,
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
        None,
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

// -- awaiting / done ----------------------------------------------------------

/// Open the detail of the first card matching `status` in a fresh demo app.
fn demo_app_with_detail(status: CardStatus) -> App {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|c| c.status == status)
        .unwrap_or_else(|| panic!("no demo card with status {}", status.as_str()))
        .clone();
    let detail = client.card_get(card.id).unwrap();
    let mut app = App::new(board);
    app.screen = Screen::CardDetail;
    app.detail = Some(detail);
    app
}

#[test]
fn enter_in_detail_confirms_awaiting_card_via_run_done() {
    let mut app = demo_app_with_detail(CardStatus::Awaiting);
    let card_id = app.detail.as_ref().unwrap().card.id;
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(
        matches!(effects.as_slice(), [Effect::RunDone(id, RunOutcome::Ok)] if *id == card_id),
        "Enter on an awaiting card must emit RunDone(ok) for that card"
    );
    // Stays on the detail screen; the driver reloads it after run.done.
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn enter_in_detail_is_noop_for_done_and_other_statuses() {
    for status in [
        CardStatus::Done,
        CardStatus::Running,
        CardStatus::Failed,
        CardStatus::Idle,
    ] {
        let mut app = demo_app_with_detail(status);
        assert!(
            update(&mut app, key(KeyCode::Enter)).is_empty(),
            "Enter must be a no-op for status {}",
            status.as_str()
        );
    }
}

#[test]
fn archive_guard_blocks_awaiting_card_on_board_and_detail() {
    // Board screen: Review column (idx 3) has the failed card at 0 and the
    // awaiting card ("Tune retry backoff") at 1.
    let mut app = demo_app();
    app.sel_col = 3;
    app.sel_card = 1;
    assert_eq!(app.selected_card_status(), Some(CardStatus::Awaiting));
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|t| t.is_error));

    // Detail screen: same guard.
    let mut app = demo_app_with_detail(CardStatus::Awaiting);
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|t| t.is_error));
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn done_card_is_final_and_can_be_archived() {
    // Done column (idx 5): "Ship v0.1" (idle) at 0, "Write changelog" (done) at 1.
    let mut app = demo_app();
    app.sel_col = 5;
    app.sel_card = 1;
    assert_eq!(app.selected_card_status(), Some(CardStatus::Done));
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { archived: true, .. }]
    ));
}
