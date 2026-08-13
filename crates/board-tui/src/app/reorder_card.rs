//! The `O` "reorder card" mini-mode. Mirrors `move_column`'s stage→commit
//! shape: `j`/`k` (and `↑`/`↓`) stage the card one slot within its column,
//! Enter commits a single same-column `card.move` carrying the staged
//! position (never a column change, never an auto-column dispatch), Esc
//! discards the staged position and emits nothing.
//!
//! The staged position is held in [`ReorderCardState::staged_index`] and
//! applied as a permutation at read time (`App::cards_of`) — `app.board` is
//! the daemon's snapshot and stays read-only, so a `board_changed` refresh
//! landing mid-mode cannot silently discard what the user staged.
//!
//! Moves are *clamped* at the edges (no wrap): staging the first card up is a
//! no-op. The selection tracks the staged card (`sel_card = staged_index`),
//! so the rendered highlight and the committed position agree, and after
//! Enter the selection stays on the reordered card.

use board_core::protocol::CardMoveParams;
use crossterm::event::{KeyCode, KeyEvent};

use super::nav::nav_delta;
use super::{App, Effect, Screen};

pub(super) fn reorder_card_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if let Some(delta) = nav_delta(k.code) {
        return shift(app, delta);
    }
    let Some(state) = app.reorder_card.as_ref() else {
        app.screen = Screen::Board;
        return vec![];
    };
    let card_id = state.card_id;
    let column_id = state.column_id;
    match k.code {
        KeyCode::Enter => {
            let Some(state) = app.reorder_card.as_ref() else {
                return vec![];
            };
            // Nothing staged (Enter straight away): leave the mode without
            // a pointless same-position RPC.
            if state.staged_index == state.original_index {
                app.reorder_card = None;
                app.screen = Screen::Board;
                return vec![];
            }
            let position = state.staged_index as i64;
            let staged_index = state.staged_index;
            app.reorder_card = None;
            app.screen = Screen::Board;
            // The selection follows the card: after the daemon compacts the
            // column, the card sits exactly at `position`.
            app.sel_card = staged_index;
            vec![Effect::CardMove(CardMoveParams {
                id: card_id,
                column_id,
                board_id: None,
                position: Some(position),
            })]
        }
        // Esc / q: discard the staged position. Dropping the state is what
        // puts the card back — `cards_of` stops permuting.
        KeyCode::Esc | KeyCode::Char('q') => {
            if let Some(state) = app.reorder_card.take() {
                let len = app
                    .col_id_at(app.sel_col)
                    .map(|id| app.cards_of(id).len())
                    .unwrap_or(0);
                app.sel_card = state.original_index.min(len.saturating_sub(1));
            }
            app.screen = Screen::Board;
            vec![]
        }
        _ => vec![],
    }
}

/// Stage the card one slot in `delta` direction. Clamps at edges.
fn shift(app: &mut App, delta: isize) -> Vec<Effect> {
    let (staged, len) = {
        let Some(state) = app.reorder_card.as_ref() else {
            return vec![];
        };
        (state.staged_index, app.cards_of(state.column_id).len())
    };
    let target = staged as isize + delta;
    if target < 0 || target >= len as isize {
        return vec![];
    }
    let target = target as usize;
    // Apply the staged index after releasing the state borrow, then move the
    // selection to the staged card so the highlight tracks it and `Enter`
    // leaves the selection on the reordered card.
    if let Some(state) = app.reorder_card.as_mut() {
        state.staged_index = target;
    }
    app.sel_card = target;
    vec![]
}
