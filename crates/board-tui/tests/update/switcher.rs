//! Compact-only column switcher sheet: the header tap opens it at the
//! Columns level (columns + "switch board" + "apply template" rows), and `b`
//! opens the board picker directly at every layout mode.

use board_core::client::{BoardClient, FakeBoardClient};
use board_tui::app::{update, App, Effect, Screen, SwitcherState};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

use super::helpers::{driver_of, key};

fn compact_area() -> Rect {
    Rect::new(0, 0, 40, 20)
}

/// `b` means "switch board": in Compact it must open the BOARD PICKER (not
/// the switcher sheet — the switcher's second level is gone).
#[test]
fn b_in_compact_opens_the_board_picker() {
    let mut d = driver_of(super::helpers::demo_client().unwrap());
    d.app.last_area = compact_area();

    d.handle(key(KeyCode::Char('b')));

    assert_eq!(d.app.screen, Screen::BoardPicker);
    let picker = d.app.picker.as_ref().expect("board picker must be open");
    assert_eq!(picker.purpose, board_tui::app::PickerPurpose::SwitchBoard);
    assert!(
        !picker.rows.is_empty(),
        "the board list must have loaded synchronously"
    );
    assert!(d.app.switcher.is_none(), "`b` must not open the switcher");
}

// -- the trailing "switch board" row (drills into the board picker) ----------

/// Build an `App` sitting on `Screen::Switcher` at the Columns level, as if
/// the header's center button had just been tapped, without going through
/// the mouse plumbing `tests/mouse.rs` already covers.
fn app_at_switcher_columns(board: board_core::protocol::BoardSnapshot) -> App {
    let mut app = App::new(board);
    let sel_col = app.sel_col;
    app.switcher = Some(SwitcherState {
        sel: sel_col,
        return_to: Screen::Board,
    });
    app.screen = Screen::Switcher;
    app
}

fn empty_board() -> board_core::protocol::BoardSnapshot {
    FakeBoardClient::new().unwrap().board_get().unwrap()
}

#[test]
fn switcher_apply_template_row_is_selectable_by_j_past_switch_board_row() {
    let board = super::helpers::demo_client().unwrap().board_get().unwrap();
    let n = board.columns.len();
    let mut app = app_at_switcher_columns(board);
    app.switcher.as_mut().unwrap().sel = n; // the "switch board" row

    update(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.switcher.as_ref().unwrap().sel,
        n + 1,
        "`j` from the switch-board row must land on the new apply-template row"
    );

    // Row count grew by exactly one: there is nothing past the apply-template
    // row, so another `j` must not move further.
    update(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.switcher.as_ref().unwrap().sel,
        n + 1,
        "apply-template must be the last row"
    );
}

#[test]
fn switcher_column_rows_and_switch_board_row_are_unaffected_by_the_new_row() {
    // Regression guard for the off-by-one risk: existing indices (each column
    // row, then the switch-board row) must still activate exactly as before.
    let board = super::helpers::demo_client().unwrap().board_get().unwrap();
    let n = board.columns.len();

    for col_idx in 0..n {
        let mut app = app_at_switcher_columns(board.clone());
        app.switcher.as_mut().unwrap().sel = col_idx;
        let effects = update(&mut app, key(KeyCode::Enter));
        assert!(effects.is_empty());
        assert_eq!(app.screen, Screen::Board);
        assert_eq!(app.sel_col, col_idx);
        assert!(app.switcher.is_none());
    }

    // The trailing "switch board" row now opens the board picker (the sheet
    // stays open underneath so Esc from the picker returns to it).
    let mut app = app_at_switcher_columns(board);
    app.switcher.as_mut().unwrap().sel = n;
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadBoardPicker { project_id: None }]
    ));
    assert_eq!(
        app.screen,
        Screen::Switcher,
        "the sheet stays open under the picker"
    );
    assert!(
        app.switcher.is_some(),
        "the switcher state survives the drill-down"
    );
}

#[test]
fn switcher_apply_template_row_on_empty_board_applies_and_closes_sheet() {
    let mut app = app_at_switcher_columns(empty_board());
    let n = app.board.columns.len();
    app.switcher.as_mut().unwrap().sel = n + 1; // trailing "apply template" row

    let effects = update(&mut app, key(KeyCode::Enter));

    match effects.as_slice() {
        [Effect::TemplateApply(name)] => assert_eq!(name, "pipeline"),
        other => panic!(
            "expected a single TemplateApply effect, got {} effects",
            other.len()
        ),
    }
    assert!(app.switcher.is_none(), "the sheet must close");
    assert_eq!(app.screen, Screen::Board);
}

#[test]
fn switcher_apply_template_row_on_nonempty_board_toasts_and_keeps_sheet_open() {
    let board = super::helpers::demo_client().unwrap().board_get().unwrap();
    let n = board.columns.len();
    let mut app = app_at_switcher_columns(board);
    app.switcher.as_mut().unwrap().sel = n + 1; // trailing "apply template" row

    let effects = update(&mut app, key(KeyCode::Enter));

    assert!(
        effects.is_empty(),
        "a disabled row must not apply the template"
    );
    assert_eq!(
        app.screen,
        Screen::Switcher,
        "the sheet must stay open on the error path"
    );
    assert!(app.switcher.is_some());
    let toast = app.toast.as_ref().expect("an error toast must be raised");
    assert!(toast.is_error);
}

#[test]
fn board_t_key_and_switcher_apply_template_row_yield_the_same_effect() {
    // Single-source-of-truth guard: both paths must route through the same
    // helper and therefore produce the identical `TemplateApply` effect.
    let mut board_screen_app = App::new(empty_board());
    let board_effects = update(&mut board_screen_app, key(KeyCode::Char('T')));

    let mut switcher_app = app_at_switcher_columns(empty_board());
    let n = switcher_app.board.columns.len();
    switcher_app.switcher.as_mut().unwrap().sel = n + 1;
    let switcher_effects = update(&mut switcher_app, key(KeyCode::Enter));

    let extract = |effects: &[Effect]| match effects {
        [Effect::TemplateApply(name)] => name.clone(),
        other => panic!(
            "expected a single TemplateApply effect, got {} effects",
            other.len()
        ),
    };
    assert_eq!(extract(&board_effects), extract(&switcher_effects));
}
