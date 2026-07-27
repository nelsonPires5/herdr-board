//! Compact-only switcher sheet: entry-point/level pairing and the two `Esc`
//! paths that fall out of it.
//!
//! Two distinct entry points reach `Screen::Switcher`, and they must land on
//! different levels:
//!   - `b` means "switch board" and must open straight at `SwitcherLevel::
//!     Boards` (board names only exist at that level — see the `12-cwd-
//!     boards` e2e regression this fixes: it sent `b` and grepped for a
//!     board name that only a Boards-level list, never a Columns-level one,
//!     contains).
//!   - the header's center-button tap (mouse-only, covered by
//!     `tests/mouse.rs`) opens at `SwitcherLevel::Columns`.
//!
//! `SwitcherState::entered_at_boards` records which entry point happened, so
//! `Esc` from the `Boards` level either closes the sheet outright (opened
//! via `b`, nothing to back out to) or steps back to `Columns` (drilled down
//! from the header tap).

use board_core::client::{BoardClient, FakeBoardClient};
use board_tui::app::{update, App, Effect, Screen, SwitcherLevel, SwitcherState};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

use super::helpers::{driver_of, key};

fn compact_area() -> Rect {
    Rect::new(0, 0, 40, 20)
}

#[test]
fn b_in_compact_opens_switcher_directly_at_boards_level() {
    let mut d = driver_of(super::helpers::demo_client().unwrap());
    d.app.last_area = compact_area();

    d.handle(key(KeyCode::Char('b')));

    assert_eq!(d.app.screen, Screen::Switcher);
    let state = d.app.switcher.as_ref().expect("switcher must be open");
    assert_eq!(
        state.level,
        SwitcherLevel::Boards,
        "`b` must land directly on the Boards level, not Columns"
    );
    assert!(
        !state.boards.is_empty(),
        "the board list must have loaded synchronously"
    );
    assert!(
        state.entered_at_boards,
        "opening via `b` must be recorded as a direct Boards entry"
    );
}

#[test]
fn esc_from_b_opened_boards_level_closes_the_sheet_entirely() {
    let mut d = driver_of(super::helpers::demo_client().unwrap());
    d.app.last_area = compact_area();
    d.handle(key(KeyCode::Char('b')));
    assert_eq!(
        d.app.switcher.as_ref().unwrap().level,
        SwitcherLevel::Boards
    );

    d.handle(key(KeyCode::Esc));

    assert_eq!(
        d.app.screen,
        Screen::Board,
        "Esc must close the sheet outright, not fall back to a Columns view \
         the user never opened"
    );
    assert!(d.app.switcher.is_none());
}

/// The header tap (`Zone::HeaderSwitch`, mouse-only) opens at Columns with
/// `entered_at_boards: false`; drilling from there into Boards via the
/// trailing "switch board" row is exercised end-to-end in `tests/mouse.rs`.
/// This test starts from that same state (constructed directly, since
/// driving it here would require the mouse plumbing that file already
/// covers) and checks only the `Esc`-from-Boards path this module owns.
#[test]
fn esc_from_header_tap_drilled_boards_level_returns_to_columns() {
    let mut d = driver_of(super::helpers::demo_client().unwrap());
    d.app.last_area = compact_area();
    let sel_col = d.app.sel_col;
    d.app.switcher = Some(SwitcherState {
        level: SwitcherLevel::Columns,
        sel: sel_col,
        columns_sel: sel_col,
        boards: Vec::new(),
        entered_at_boards: false,
        return_to: Screen::Board,
    });
    d.app.screen = Screen::Switcher;

    // Drill into Boards via the trailing "switch board" row (index `n`,
    // one past the last column) — activating it records `n` itself as the
    // Columns-level selection to restore, since that's the row that was
    // highlighted when the user drilled down.
    let n = d.app.board.columns.len();
    d.app.switcher.as_mut().unwrap().sel = n;
    d.handle(key(KeyCode::Enter));
    assert_eq!(
        d.app.switcher.as_ref().unwrap().level,
        SwitcherLevel::Boards
    );

    d.handle(key(KeyCode::Esc));

    assert_eq!(
        d.app.screen,
        Screen::Switcher,
        "Esc must step back to Columns, not close the sheet"
    );
    let state = d.app.switcher.as_ref().unwrap();
    assert_eq!(state.level, SwitcherLevel::Columns);
    assert_eq!(
        state.sel, n,
        "the Columns-level selection (the trailing row) active before \
         drilling in must be restored, not reset to the top"
    );
}

// -- the trailing "apply template" row (added after "switch board") ---------

/// Build an `App` sitting on `Screen::Switcher` at the Columns level, as if
/// the header's center button had just been tapped, without going through
/// the mouse plumbing `tests/mouse.rs` already covers.
fn app_at_switcher_columns(board: board_core::protocol::BoardSnapshot) -> App {
    let mut app = App::new(board);
    let sel_col = app.sel_col;
    app.switcher = Some(SwitcherState {
        level: SwitcherLevel::Columns,
        sel: sel_col,
        columns_sel: sel_col,
        boards: Vec::new(),
        entered_at_boards: false,
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

    let mut app = app_at_switcher_columns(board);
    app.switcher.as_mut().unwrap().sel = n; // switch-board row, unchanged index
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadBoardsForSwitcher]
    ));
    assert_eq!(
        app.switcher.as_ref().unwrap().level,
        SwitcherLevel::Columns,
        "LoadBoardsForSwitcher flips the level via `enter_boards_level`, which \
         the driver calls; here we only assert the effect fired and the sheet \
         is still open"
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
