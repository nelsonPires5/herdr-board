//! Shared navigation primitives: the `↑/↓` + `k/j` decoder every list screen
//! reads, the clamped cursor step they all apply, the board's wrapping
//! column/card selection, and the one archive guard the three "move this card"
//! entry points share.
//!
//! Nothing here binds a key on its own — `nav_delta` only *decodes* one, and
//! each screen still decides what a step means for it. That is why the help
//! coverage test treats this file as global rather than owning a section.

use crossterm::event::KeyCode;

use super::App;

/// The universal vertical-navigation decoder: `-1` for `↑`/`k`, `1` for
/// `↓`/`j`, `None` for anything else.
///
/// Six handlers (board, picker, switcher, card detail, help, comment history)
/// used to spell this match out themselves, which is six chances for the two
/// key spellings to drift apart on one screen only.
pub(super) fn nav_delta(code: KeyCode) -> Option<isize> {
    match code {
        KeyCode::Up | KeyCode::Char('k') => Some(-1),
        KeyCode::Down | KeyCode::Char('j') => Some(1),
        _ => None,
    }
}

/// Move a cursor or scroll offset by `delta`, saturating at `0` and — when
/// moving forward — at `max` (inclusive). Never wraps.
///
/// Clamping only in the forward direction is deliberate and matches what every
/// call site did by hand: an offset can legitimately sit past `max` after the
/// terminal is resized smaller, and scrolling *up* out of that state one row at
/// a time is exactly what the user asked for.
///
/// This is a free function rather than a "selection list" struct because
/// `Picker`/`SwitcherState` are constructed field-by-field by the external test
/// crates; wrapping their `sel` in a new type would be an API break, not code
/// motion.
pub(super) fn step_clamped(cur: usize, delta: isize, max: usize) -> usize {
    if delta < 0 {
        cur.saturating_sub(delta.unsigned_abs())
    } else {
        cur.saturating_add(delta as usize).min(max)
    }
}

impl App {
    pub(super) fn clamp_card(&mut self) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            self.sel_card = 0;
        } else if self.sel_card >= len {
            self.sel_card = len - 1;
        }
    }

    pub(super) fn move_col(&mut self, delta: isize) {
        let n = self.board.columns.len();
        if n == 0 {
            return;
        }
        self.sel_col = (self.sel_col as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_card();
    }

    pub(super) fn move_card(&mut self, delta: isize) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        self.sel_card = (self.sel_card as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Whether the selected card refuses to be moved because it is archived,
    /// raising the one shared explanation toast when it does.
    ///
    /// The single gate for all three "move this card" entry points — the `m`
    /// picker, the `H`/`L` shove, and the start of a mouse drag — so they
    /// cannot word the refusal differently or forget it. No selection is not a
    /// refusal: the callers already treat that as "nothing to do".
    pub(super) fn reject_archived_move(&mut self) -> bool {
        let archived = self
            .selected_card()
            .is_some_and(|card| card.archived_at.is_some());
        if archived {
            self.set_toast("restore archived card before moving", true);
        }
        archived
    }
}

/// Post-mutation helper: after the board is refetched the selection may point
/// past the end of a shrunk column; clamp it. Also used by the driver.
pub fn clamp_selection(app: &mut App) {
    if app.sel_col >= app.board.columns.len() {
        app.sel_col = app.board.columns.len().saturating_sub(1);
    }
    app.clamp_card();
}
