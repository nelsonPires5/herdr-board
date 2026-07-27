//! The `M` "move column" mini-mode. Mirrors `open_move_picker`'s stage→commit
//! shape: ←/→ reorder the focused column locally, Enter commits a single
//! `column.reorder` (then the driver refetches the canonical order), Esc
//! discards the staged order and emits nothing.
//!
//! The staged order is held in [`MoveColumnState::staged_index`] and applied as
//! a permutation at read time (`App::display_column`) — `app.board` is the
//! daemon's snapshot and stays read-only, so a `board_changed` refresh landing
//! mid-mode cannot silently discard what the user staged.
//!
//! Reorders are *clamped* at the edges (no wrap): moving the last column right
//! is a no-op. The selection tracks the moving column so the board's sliding
//! window follows it.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Effect, Screen};

pub(super) fn move_column_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(state) = app.move_column.as_ref() else {
        app.screen = Screen::Board;
        return vec![];
    };
    let column_id = state.column_id;
    match k.code {
        KeyCode::Left | KeyCode::Char('h') => shift(app, -1),
        KeyCode::Right | KeyCode::Char('l') => shift(app, 1),
        KeyCode::Enter => {
            let position = state.staged_index as i64;
            app.move_column = None;
            app.screen = Screen::Board;
            vec![Effect::ColumnReorder {
                id: column_id,
                position,
            }]
        }
        // Esc / q: discard the staged reorder. Nothing to undo in the snapshot
        // — dropping the state is what puts the column back.
        KeyCode::Esc | KeyCode::Char('q') => {
            if let Some(state) = app.move_column.take() {
                app.sel_col = state
                    .original_index
                    .min(app.board.columns.len().saturating_sub(1));
            }
            app.screen = Screen::Board;
            vec![]
        }
        _ => vec![],
    }
}

/// Stage the column one slot in `delta` direction. Clamps at edges.
fn shift(app: &mut App, delta: isize) -> Vec<Effect> {
    let n = app.board.columns.len() as isize;
    let Some(state) = app.move_column.as_mut() else {
        return vec![];
    };
    let target = state.staged_index as isize + delta;
    if target < 0 || target >= n {
        return vec![];
    }
    state.staged_index = target as usize;
    app.sel_col = target as usize;
    vec![]
}
