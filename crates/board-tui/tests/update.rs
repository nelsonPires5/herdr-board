//! Unit tests for the pure reducer: navigation wrapping, form field cycling,
//! and drag-state transitions.

use board_core::client::BoardClient;
use board_tui::app::{update, App, Effect, Msg, Screen};
use board_tui::forms::{FieldId, Form};
use board_tui::testkit::demo_client;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::empty()))
}

fn demo_app() -> App {
    let mut c = demo_client().unwrap();
    App::new(c.board_get().unwrap())
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

    // Focus starts at Title (0). Tab advances; worktree_base is hidden while
    // space != worktree, so the visible-field walk skips it.
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
        FieldId::WorktreeBase
    );
}

#[test]
fn worktree_base_visibility_follows_space_kind() {
    let mut form = Form::card_create(1);
    // Find the space-kind choice field and cycle it to "worktree".
    let space_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::SpaceKind)
        .unwrap();
    let wt_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::WorktreeBase)
        .unwrap();
    assert!(!form.field_visible(wt_idx)); // hidden by default (workspace)
                                          // workspace -> cwd -> worktree
    form.fields[space_idx].cycle(2);
    assert!(form.field_visible(wt_idx));
}

#[test]
fn choice_cycling_wraps() {
    let mut form = Form::card_create(1);
    let eff_idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::Effort)
        .unwrap();
    // 6 options (none/low/medium/high/xhigh/max); cycle back one from 0 -> last.
    form.fields[eff_idx].cycle(-1);
    assert_eq!(form.fields[eff_idx].display(), "max");
    form.fields[eff_idx].cycle(1);
    assert_eq!(form.fields[eff_idx].display(), "none");
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
