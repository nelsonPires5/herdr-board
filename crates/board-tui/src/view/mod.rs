//! Rendering: the pure `view(&App, &mut Frame)` plus the board layout used for
//! both drawing and mouse hit-testing. No clocks are read here — timers use the
//! injected `app.now` so snapshots are deterministic.

use board_core::model::{Board, Card};
use board_core::protocol::{AwaitingReason, CardStatus};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Frame;

use crate::app::{App, CardFilter, Screen};

const MIN_COL_W: u16 = 26;
const CARD_H: u16 = 3;
const COMPACT_CARD_H: u16 = 4;
const MAX_SCOPE_LABEL: usize = 32;
const NARROW_DETAIL_WIDTH: u16 = 100;
const HELP_GUTTER_WIDTH: u16 = 2;
const HELP_KEY_WIDTH: u16 = 13;
/// Characters a help key label may occupy. `HELP_KEY_WIDTH` minus the two-space
/// indent every key row carries, so key text and description never collide.
const HELP_KEY_TEXT: usize = HELP_KEY_WIDTH as usize - 2;

/// Responsive breakpoint, derived from terminal width only. Compact drives a
/// single-column mobile-first board + fullscreen sheets; Regular/Wide keep the
/// existing multi-column desktop behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutMode {
    Compact,
    Regular,
    Wide,
}

impl LayoutMode {
    pub fn from_width(w: u16) -> LayoutMode {
        if w < 60 {
            LayoutMode::Compact
        } else if w <= 119 {
            LayoutMode::Regular
        } else {
            LayoutMode::Wide
        }
    }
}

pub fn board_scope_label(board: &Board) -> String {
    let raw = match board.scope_path.as_deref() {
        None => "Global",
        Some(path) => std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(path),
    };
    truncate(&sanitize(raw), MAX_SCOPE_LABEL)
}

pub fn board_picker_label(board: &Board) -> String {
    match board.scope_path.as_deref() {
        None => "Global".into(),
        Some(path) => format!("{} — {}", board_scope_label(board), sanitize(path)),
    }
}

pub fn pane_title(board: &Board, filter: CardFilter) -> String {
    format!("Board [{} · {}]", board_scope_label(board), filter.label())
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '[' => '(',
            ']' => ')',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect()
}

/// Region above the 1-row footer.
fn main_area(area: Rect) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1))
}

mod board;
mod detail;
mod form;
/// The single source of the `?` overlay contents: `(screen, key, description)`.
///
/// Tagging every row with the [`Screen`] whose handler owns the binding is
/// what lets `tests/help.rs` check the table against the real key handlers in
/// `src/app/*.rs` instead of trusting it to be hand-maintained — a row that
/// documents a key nobody handles, or a handled key nobody documents, is a
/// test failure rather than a slow drift.
///
/// Rows whose key is `"--"` are section separators; their description is the
/// heading text and their screen is the section they introduce.
pub const HELP_KEYS: &[(Screen, &str, &str)] = &[
    (Screen::Board, "←/→ h/l", "focus column"),
    (Screen::Board, "↑/↓ k/j", "focus card"),
    (Screen::Board, "b", "switch board"),
    (Screen::Board, "n", "new card"),
    (Screen::Board, "N", "new column"),
    (Screen::Board, "e", "edit card"),
    (Screen::Board, "E", "edit focused column"),
    (Screen::Board, "a", "archive / restore card"),
    (Screen::Board, "v", "cycle active/all/archived"),
    (Screen::Board, "d", "delete card"),
    (Screen::Board, "D", "delete/move column cards"),
    (Screen::Board, "m", "move card (board→column)"),
    (Screen::Board, "M", "move focused column"),
    (Screen::Board, "H / L", "shove card left / right"),
    (Screen::Board, "Enter", "card detail"),
    (Screen::Board, "T", "apply template (empty)"),
    (Screen::Board, "r / R", "refresh board"),
    (Screen::Board, "?", "this help (any screen)"),
    (Screen::Board, "q / Esc", "back / quit"),
    (Screen::CardDetail, "--", "-- card detail --"),
    (Screen::CardDetail, "Enter", "confirm done (awaiting)"),
    (Screen::CardDetail, "e", "edit card / comment"),
    (Screen::CardDetail, "a", "archive / restore card"),
    (Screen::CardDetail, "c", "add comment"),
    (Screen::CardDetail, "d", "delete focused comment"),
    (Screen::CardDetail, "h", "comment history"),
    (Screen::CardDetail, "Tab", "focus comments / runs"),
    (Screen::CardDetail, "↑/↓ k/j", "select comment / run"),
    (Screen::CardDetail, "f / click", "toggle popup / fullscreen"),
    (Screen::CardDetail, "o", "jump to selected run pane"),
    (Screen::CardDetail, "x", "cancel run (asks first)"),
    (Screen::CardDetail, "r", "retry run (asks first)"),
    (Screen::CardDetail, "q / Esc", "back to board"),
    (Screen::CardForm, "--", "-- forms --"),
    (Screen::CardForm, "Tab", "next field"),
    (Screen::CardForm, "Shift+Tab", "previous field"),
    (Screen::CardForm, "←/→ Space", "cycle a picker field"),
    (Screen::CardForm, "Ctrl+E", "edit textarea in $EDITOR"),
    (Screen::CardForm, "Enter", "submit"),
    (Screen::CardForm, "Esc", "cancel"),
    (Screen::Picker, "--", "-- picker / confirm --"),
    (Screen::Picker, "↑/↓ k/j", "move selection"),
    (Screen::Picker, "Enter", "choose"),
    (Screen::Picker, "b", "other board (moving)"),
    (Screen::Confirm, "y / n", "confirm / decline"),
    (Screen::Picker, "q / Esc", "cancel"),
    (Screen::MoveColumn, "--", "-- move column (M) --"),
    (Screen::MoveColumn, "←/→ h/l", "stage the reorder"),
    (Screen::MoveColumn, "Enter", "commit the reorder"),
    (Screen::MoveColumn, "q / Esc", "discard"),
    (Screen::Switcher, "--", "-- sheets --"),
    (Screen::Switcher, "k/j Enter", "switcher: move / open"),
    (Screen::Switcher, "q / Esc", "switcher: close / back"),
    (Screen::CommentHistory, "↑/↓ k/j", "history: scroll"),
    (Screen::CommentHistory, "q / Esc", "history: back to card"),
    (Screen::Help, "↑/↓ k/j", "help: scroll (compact)"),
    (Screen::Help, "q/Esc/any", "help: close"),
    (Screen::Board, "--", "-- mouse --"),
    (Screen::Board, "click", "focus card/column"),
    (Screen::Board, "dbl-click", "open card detail"),
    (Screen::Board, "drag", "move card/reorder column"),
    (Screen::Board, "wheel", "scroll cards"),
];

mod layout;
mod overlays;

pub use detail::{
    comment_row_spans, comment_wrapped_rows, comments_action_bar_shown, comments_viewport,
    detail_layout, detail_toggle_rect, DetailLayout,
};
pub use layout::{board_layout, BoardLayout, ColLayout, CompactHeader, ScrollInfo};
pub use overlays::{
    comment_history_rect, comment_history_wrapped_rows, help_content_width, help_list_rect,
    help_regular_max_scroll, help_wrapped_rows,
};

// -- glyphs ------------------------------------------------------------------

fn status_glyph(status: CardStatus) -> (char, Color) {
    match status {
        CardStatus::Running => ('▶', Color::LightGreen),
        CardStatus::Blocked => ('⏸', Color::LightYellow),
        CardStatus::Failed => ('✗', Color::LightRed),
        CardStatus::Queued => ('⧗', Color::LightCyan),
        // awaiting = agent finished(?) without `board done`; pending review.
        CardStatus::Awaiting => ('?', Color::Yellow),
        // done = completion confirmed; final state.
        CardStatus::Done => ('✓', Color::Green),
        CardStatus::Idle => ('·', Color::Gray),
    }
}

/// Status label for the detail view: `awaiting` explains *why* it is waiting.
fn status_label(card: &Card) -> String {
    match (card.status, card.awaiting_reason) {
        (CardStatus::Awaiting, Some(AwaitingReason::AgentDone)) => {
            "awaiting (agent reported done)".to_string()
        }
        (CardStatus::Awaiting, Some(AwaitingReason::IdleExpired)) => {
            "awaiting (idle timeout)".to_string()
        }
        (status, _) => status.as_str().to_string(),
    }
}

// -- entry point -------------------------------------------------------------

pub fn view(app: &App, f: &mut Frame) {
    app.hit_map.borrow_mut().clear();
    let area = f.area();
    board::draw_board(app, f, area);

    match app.screen {
        Screen::Board => {}
        Screen::CardDetail => detail::draw_detail(app, f, area),
        Screen::CardForm | Screen::ColumnForm => {
            if let Some(form) = &app.form {
                form::draw_form(app, form, f, area);
            }
        }
        Screen::Picker => overlays::draw_picker(app, f, area),
        Screen::MoveColumn => overlays::draw_move_column(app, f, area),
        Screen::Confirm => overlays::draw_confirm(app, f, area),
        Screen::Help => overlays::draw_help(app, f, area),
        Screen::Switcher => board::draw_switcher(app, f, area),
        Screen::CommentHistory => overlays::draw_comment_history(app, f, area),
    }

    overlays::draw_footer(app, f, area);
}

// -- helpers -----------------------------------------------------------------

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else if max == 1 {
        "…".to_string()
    } else {
        let mut out: String = chars[..max - 1].iter().collect();
        out.push('…');
        out
    }
}

fn centered_rect_abs(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// Sheet placement: Compact overlays go fullscreen (over `main_area`, i.e.
/// above the footer row); Regular/Wide keep today's centered floating box.
///
/// Both branches derive from `main_area(area)` (not the raw frame `area`), so
/// the footer row is subtracted exactly once regardless of mode — passing the
/// full frame `area` in here is always correct; do not pre-subtract the
/// footer before calling this.
pub fn sheet_area(mode: LayoutMode, pref_w: u16, pref_h: u16, area: Rect) -> Rect {
    let base = main_area(area);
    match mode {
        LayoutMode::Compact => base,
        LayoutMode::Regular | LayoutMode::Wide => centered_rect_abs(pref_w, pref_h, base),
    }
}

#[cfg(test)]
mod tests;
