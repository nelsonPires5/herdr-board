//! Compact-only two-level switcher sheet: level 1 (`SwitcherLevel::Columns`)
//! lists the current board's columns (plus a trailing "switch board" row),
//! level 2 (`SwitcherLevel::Boards`) lists boards. Reachable only in
//! `LayoutMode::Compact` (Regular/Wide keep the existing `b` → board
//! `Picker`).
//!
//! Two distinct entry points land on different levels: `b` means "switch
//! board" and opens straight at `Boards` (see `board::board_key`), while
//! tapping the header's center button opens at `Columns` (see
//! `mouse::handle_zone`'s `Zone::HeaderSwitch` arm). `SwitcherState::
//! entered_at_boards` records which one happened, so `Esc` from `Boards`
//! either closes the sheet (opened via `b`, nothing to back out to) or steps
//! back to `Columns` (drilled down from the header tap).

use crossterm::event::{KeyCode, KeyEvent};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Screen, SwitcherLevel, SwitcherState};

/// Rows in the level-1 (columns) list: one per column plus the two trailing
/// rows ("switch board", then "apply template").
fn columns_row_count(app: &App) -> usize {
    app.board.columns.len() + 2
}

pub(super) fn switcher_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.switcher.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    let row_count = match state.level {
        SwitcherLevel::Columns => columns_row_count(app),
        SwitcherLevel::Boards => state.boards.len(),
    };
    if let Some(delta) = nav_delta(k.code) {
        let state = app.switcher.as_mut().unwrap();
        state.sel = step_clamped(state.sel, delta, row_count.saturating_sub(1));
        return vec![];
    }
    match k.code {
        KeyCode::Enter => return activate(app),
        // `q` closes the sheet outright from either level — it is the "get me
        // out of here" key every other screen already honours, and unlike
        // `Esc` it never means "step back one level".
        KeyCode::Char('q') => close(app),
        KeyCode::Esc => {
            let state = app.switcher.as_mut().unwrap();
            match state.level {
                SwitcherLevel::Boards if state.entered_at_boards => {
                    // Opened directly at Boards via `b`; there is no
                    // Columns view to back out to, so `Esc` closes the
                    // sheet outright.
                    close(app);
                }
                SwitcherLevel::Boards => {
                    state.level = SwitcherLevel::Columns;
                    // Restore the Columns-level selection that was active
                    // before we drilled into Boards, rather than resetting
                    // to the top row.
                    state.sel = state.columns_sel;
                }
                SwitcherLevel::Columns => close(app),
            }
        }
        _ => {}
    }
    vec![]
}

/// Dismiss the sheet, landing on the screen it was opened from.
fn close(app: &mut App) {
    let return_to = app
        .switcher
        .take()
        .map(|state| state.return_to)
        .unwrap_or(Screen::Board);
    app.screen = return_to;
}

fn activate(app: &mut App) -> Vec<Effect> {
    let Some(state) = app.switcher.as_ref() else {
        return vec![];
    };
    let level = state.level;
    let sel = state.sel;
    match level {
        SwitcherLevel::Columns => {
            let n = app.board.columns.len();
            if sel == n {
                // Trailing "switch board" row: remember where we were in the
                // Columns list (so `Esc` from Boards can restore it), then
                // fetch the board list.
                if let Some(state) = app.switcher.as_mut() {
                    state.columns_sel = sel;
                }
                return vec![Effect::LoadBoardsForSwitcher];
            }
            if sel == n + 1 {
                // Trailing "apply template" row: same gate/toast/effect as
                // the board `T` key, via the shared helper.
                let effects = super::apply_template(app);
                if !effects.is_empty() {
                    close(app);
                }
                return effects;
            }
            app.sel_col = sel;
            app.clamp_card();
            close(app);
            vec![]
        }
        SwitcherLevel::Boards => {
            let Some(board_id) = state.boards.get(sel).map(|(_, id)| *id) else {
                return vec![];
            };
            close(app);
            vec![Effect::SwitchBoard(board_id)]
        }
    }
}

/// Called by the driver once the board list has loaded, to populate level 2.
pub fn enter_boards_level(
    state: &mut SwitcherState,
    boards: Vec<(String, i64)>,
    current_sel: usize,
) {
    state.level = SwitcherLevel::Boards;
    state.boards = boards;
    state.sel = current_sel;
}
