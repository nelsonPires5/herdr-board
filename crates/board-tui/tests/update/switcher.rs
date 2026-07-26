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

use board_tui::app::{Screen, SwitcherLevel, SwitcherState};
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
