//! Compact-only column switcher sheet: the current board's columns plus a
//! trailing "switch board" row (which opens the board picker) and an "apply
//! template" row. Reachable only in `LayoutMode::Compact` via the header's
//! center-button tap (see `mouse::handle_zone`'s `Zone::HeaderSwitch` arm);
//! `b` opens the board picker directly at every layout mode.

use crossterm::event::{KeyCode, KeyEvent};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Screen};

pub(super) fn switcher_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.switcher.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    let row_count = app.board.columns.len() + 2;
    if let Some(delta) = nav_delta(k.code) {
        state.sel = step_clamped(state.sel, delta, row_count.saturating_sub(1));
        return vec![];
    }
    match k.code {
        KeyCode::Enter => return activate(app),
        KeyCode::Esc | KeyCode::Char('q') => close(app),
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
    let sel = state.sel;
    let n = app.board.columns.len();
    if sel == n {
        // Trailing "switch board" row: open the board picker for the current
        // project. The sheet stays open underneath, so the picker's `return_to`
        // (read from `app.screen` by `load_board_picker`) is the switcher —
        // Esc from the picker drills back to the columns, Enter lands on the
        // chosen board (which clears the sheet via `replace_board`).
        return vec![Effect::LoadBoardPicker { project_id: None }];
    }
    if sel == n + 1 {
        // Trailing "apply template" row: same gate/toast/effect as the board
        // `T` key, via the shared helper.
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
