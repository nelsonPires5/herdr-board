//! The `M` "move column" mini-mode. Mirrors `open_move_picker`'s stage→commit
//! shape: ←/→ reorder the focused column locally, Enter commits a single
//! `column.reorder` (then the driver refetches the canonical order), Esc
//! restores the original order and emits nothing.
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
        KeyCode::Left | KeyCode::Char('h') => shift(app, column_id, -1),
        KeyCode::Right | KeyCode::Char('l') => shift(app, column_id, 1),
        KeyCode::Enter => {
            let position = current_index(app, column_id).unwrap_or(0) as i64;
            app.move_column = None;
            app.screen = Screen::Board;
            vec![Effect::ColumnReorder {
                id: column_id,
                position,
            }]
        }
        // Esc / q: discard the staged reorder and put the column back.
        KeyCode::Esc | KeyCode::Char('q') => {
            if let Some(state) = app.move_column.take() {
                restore(app, column_id, state.original_index);
            }
            app.screen = Screen::Board;
            vec![]
        }
        _ => vec![],
    }
}

fn current_index(app: &App, column_id: i64) -> Option<usize> {
    app.board.columns.iter().position(|c| c.id == column_id)
}

/// Move the column one slot in `delta` direction, locally. Clamps at edges.
fn shift(app: &mut App, column_id: i64, delta: isize) -> Vec<Effect> {
    let Some(idx) = current_index(app, column_id) else {
        return vec![];
    };
    let n = app.board.columns.len() as isize;
    let target = idx as isize + delta;
    if target < 0 || target >= n {
        return vec![];
    }
    let col = app.board.columns.remove(idx);
    app.board.columns.insert(target as usize, col);
    app.sel_col = target as usize;
    vec![]
}

/// Restore the column to its entry position (used by Esc cancel).
fn restore(app: &mut App, column_id: i64, original_index: usize) {
    if let Some(idx) = current_index(app, column_id) {
        if idx != original_index {
            let col = app.board.columns.remove(idx);
            let at = original_index.min(app.board.columns.len());
            app.board.columns.insert(at, col);
        }
    }
    app.sel_col = original_index.min(app.board.columns.len().saturating_sub(1));
}
