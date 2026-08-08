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
const CARD_H: u16 = 5;
const COMPACT_CARD_H: u16 = 6;
const BOARD_ACTION_COUNT: u16 = 9;

fn board_action_columns(width: u16) -> u16 {
    match width {
        0..=59 => 3,
        60..=119 => 5,
        _ => BOARD_ACTION_COUNT,
    }
}

fn board_action_rows(width: u16) -> u16 {
    BOARD_ACTION_COUNT.div_ceil(board_action_columns(width))
}

/// Filter labels stay explicit when the one-line rail has room. At the
/// narrowest supported widths the unambiguous prefixes keep all three filters
/// visible without wrapping the Compact header into extra rows.
pub(super) fn compact_filter_options(width: u16) -> [(&'static str, CardFilter); 3] {
    if width >= 48 {
        [
            ("Active", CardFilter::Active),
            ("All", CardFilter::All),
            ("Archived", CardFilter::Archived),
        ]
    } else if width >= 23 {
        [
            ("Act", CardFilter::Active),
            ("All", CardFilter::All),
            ("Arc", CardFilter::Archived),
        ]
    } else if width >= 19 {
        [
            ("A", CardFilter::Active),
            ("All", CardFilter::All),
            ("R", CardFilter::Archived),
        ]
    } else {
        [
            ("A", CardFilter::Active),
            ("L", CardFilter::All),
            ("R", CardFilter::Archived),
        ]
    }
}

/// Compact has exactly three content rows — identity, board/visibility
/// controls, and the column navigator — followed by its divider. Regular and
/// Wide put all header controls on one content row and use the second row only
/// for the divider.
pub fn board_header_height(width: u16) -> u16 {
    if width < 60 {
        4
    } else {
        2
    }
}
const MAX_SCOPE_LABEL: usize = 32;
const NARROW_DETAIL_WIDTH: u16 = 100;
const HELP_GUTTER_WIDTH: u16 = 2;

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

/// Board content between persistent header/action chrome. The footer hint
/// row is gone: an idle board has no permanently reserved blank row. A
/// transient toast may reserve one row immediately above the action rail.
pub(crate) fn board_content_area_for(app: &App, area: Rect) -> Rect {
    board_content_area_with_toast(area, app.toast.is_some())
}

fn board_content_area_with_toast(area: Rect, toast: bool) -> Rect {
    let action_h = board_action_rows(area.width).min(area.height);
    let header_h = board_header_height(area.width).min(area.height.saturating_sub(action_h));
    let toast_h = u16::from(toast && area.height > header_h.saturating_add(action_h));
    let reserved = header_h.saturating_add(toast_h).saturating_add(action_h);
    Rect::new(
        area.x,
        area.y.saturating_add(header_h),
        area.width,
        area.height.saturating_sub(reserved),
    )
}

/// Board-only top chrome, above the card viewport.
fn board_header_area(area: Rect) -> Rect {
    let action_h = board_action_rows(area.width).min(area.height);
    let height = board_header_height(area.width).min(area.height.saturating_sub(action_h));
    Rect::new(area.x, area.y, area.width, height)
}

/// Card viewport between the board header and action rail.
pub(crate) fn board_body_area_for(app: &App, area: Rect) -> Rect {
    board_content_area_for(app, area)
}

/// Board-only click-first action row at the bottom of the frame.
fn board_action_area(area: Rect) -> Rect {
    let height = board_action_rows(area.width).min(area.height);
    Rect::new(
        area.x,
        area.bottom().saturating_sub(height),
        area.width,
        height,
    )
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
    (Screen::CardForm, "Shift+Enter", "newline in textarea"),
    (Screen::CardForm, "Ctrl+J", "newline in textarea"),
    (Screen::CardForm, "f", "toggle popup / fullscreen"),
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
    detail_layout, detail_toggle_rect, runs_viewport_height, DetailLayout,
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

/// Sheet placement: Compact overlays fill the board content region;
/// Regular/Wide keep a centered floating box inside that same region.
///
/// The persistent top and bottom board chrome is deliberately outside the
/// returned rectangle. Passing the full frame `area` is always correct.
pub fn sheet_area(mode: LayoutMode, pref_w: u16, pref_h: u16, area: Rect) -> Rect {
    sheet_area_with_toast(mode, pref_w, pref_h, area, false)
}

pub(crate) fn sheet_area_for_app(
    app: &App,
    mode: LayoutMode,
    pref_w: u16,
    pref_h: u16,
    area: Rect,
) -> Rect {
    sheet_area_with_toast(mode, pref_w, pref_h, area, app.toast.is_some())
}

fn sheet_area_with_toast(
    mode: LayoutMode,
    pref_w: u16,
    pref_h: u16,
    area: Rect,
    toast: bool,
) -> Rect {
    let base = board_content_area_with_toast(area, toast);
    match mode {
        LayoutMode::Compact => base,
        LayoutMode::Regular | LayoutMode::Wide => centered_rect_abs(pref_w, pref_h, base),
    }
}

#[cfg(test)]
mod tests;
