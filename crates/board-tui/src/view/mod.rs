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
const MAX_SCOPE_LABEL: usize = 32;
const NARROW_DETAIL_WIDTH: u16 = 100;
const HELP_GUTTER_WIDTH: u16 = 2;
const HELP_KEY_WIDTH: u16 = 13;

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
/// The single source of the `?` overlay contents (Phase E copies this table).
pub const HELP_KEYS: &[(&str, &str)] = &[
    ("←/→ h/l", "focus column"),
    ("↑/↓ k/j", "focus card"),
    ("b", "switch board"),
    ("n", "new card"),
    ("N", "new column"),
    ("e", "edit card"),
    ("E", "edit focused column"),
    ("a", "archive / restore card"),
    ("v", "cycle active/all/archived"),
    ("d", "delete card"),
    ("D", "delete/move column cards"),
    ("m", "move card (column picker)"),
    ("H / L", "shove card left / right"),
    ("Enter", "card detail"),
    ("T", "apply template (empty)"),
    ("r", "refresh board"),
    ("?", "this help"),
    ("q / Esc", "back / quit"),
    ("--", "-- card detail --"),
    ("Enter", "confirm done (awaiting)"),
    ("e", "edit card"),
    ("a", "archive / restore card"),
    ("c", "add comment"),
    ("Tab", "focus comments / runs"),
    ("↑/↓ k/j", "scroll focused section"),
    ("f / click", "toggle popup / fullscreen"),
    ("o", "jump to pane"),
    ("x", "cancel run"),
    ("r", "retry run"),
    ("--", "-- forms --"),
    ("Tab", "next field"),
    ("Shift+Tab", "previous field"),
    ("←/→ Space", "cycle a picker field"),
    ("Ctrl+E", "edit textarea in $EDITOR"),
    ("Enter", "submit"),
    ("Esc", "cancel"),
    ("--", "-- mouse --"),
    ("click", "focus card/column"),
    ("dbl-click", "open card detail"),
    ("drag", "move card/reorder column"),
    ("wheel", "scroll cards"),
];

mod layout;
mod overlays;

pub use detail::{detail_layout, detail_toggle_rect, DetailLayout};
pub use layout::{board_layout, BoardLayout, ColLayout};

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
    let area = f.area();
    board::draw_board(app, f, area);

    match app.screen {
        Screen::Board => {}
        Screen::CardDetail => detail::draw_detail(app, f, area),
        Screen::CardForm | Screen::ColumnForm => {
            if let Some(form) = &app.form {
                form::draw_form(form, f, area);
            }
        }
        Screen::Picker => overlays::draw_picker(app, f, area),
        Screen::Confirm => overlays::draw_confirm(app, f, area),
        Screen::Help => overlays::draw_help(f, area),
    }

    overlays::draw_footer(app, f, area);
}

// -- helpers -----------------------------------------------------------------

fn truncate(s: &str, max: usize) -> String {
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

// -- time --------------------------------------------------------------------

/// Parse a SQLite `datetime('now')` string (`YYYY-MM-DD HH:MM:SS`, UTC) to epoch
/// seconds. Returns `None` on any parse failure.
pub fn parse_epoch(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: i64 = t.next().unwrap_or("0").parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests;
