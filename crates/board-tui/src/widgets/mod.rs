//! Minimal hit-testing + button-bar widgets for the mobile-first prototype.
//!
//! `HitMap` is rebuilt every frame in `view()` and stashed on `App` (in a
//! `RefCell`) so the mouse handler can look up what the last frame drew at a
//! given cell without duplicating layout math. Keep this additive: existing
//! board/detail hit-testing (`view::board_layout`, `view::detail_layout`)
//! is untouched.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Interactive zones registered by the new Compact-mode widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// Compact board header: previous column `‹`.
    HeaderPrev,
    /// Compact board header: next column `›`.
    HeaderNext,
    /// Compact board header: center button opening the column switcher.
    HeaderSwitch,
    /// A row in the switcher sheet (column list or board list), by index.
    SwitcherRow(usize),
    /// The switcher's trailing "switch board" row (level 1 only).
    SwitcherSwitchBoard,
    /// The switcher's trailing "apply template" row (level 1 only), after
    /// `SwitcherSwitchBoard`.
    SwitcherApplyTemplate,
    /// `ButtonBar` save action.
    BarSave,
    /// `ButtonBar` cancel action.
    BarCancel,
    /// Sheet title-bar close `×`.
    SheetClose,
    /// A rendered comment row in the card detail's comments section, by index
    /// into `CardDetail::comments`.
    CommentRow(usize),
    /// Card detail comments action bar: edit the focused comment.
    CommentEdit,
    /// Card detail comments action bar: delete the focused comment.
    CommentDelete,
    /// Card detail comments action bar: view the focused comment's history.
    CommentHistory,
}

/// Rects registered during the current frame's draw, consulted by the mouse
/// handler on the next input event.
#[derive(Default)]
pub struct HitMap {
    zones: Vec<(Rect, Zone)>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.zones.clear();
    }

    pub fn push(&mut self, rect: Rect, zone: Zone) {
        self.zones.push((rect, zone));
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<Zone> {
        // Last-pushed wins: overlays are drawn after the board, so later
        // registrations should shadow earlier ones at the same cell.
        for (rect, zone) in self.zones.iter().rev() {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(*zone);
            }
        }
        None
    }
}

/// `[ Save ]  [ Cancel ]` button row, registering its rects in `hit_map`.
pub struct ButtonBar<'a> {
    pub save_label: &'a str,
    pub cancel_label: &'a str,
}

impl<'a> ButtonBar<'a> {
    pub fn new(save_label: &'a str, cancel_label: &'a str) -> Self {
        ButtonBar {
            save_label,
            cancel_label,
        }
    }

    /// Render at the top-left of `area` and register `BarSave`/`BarCancel`
    /// zones. `area` should be a single row.
    pub fn render(&self, f: &mut Frame, area: Rect, hit_map: &mut HitMap) {
        let save_text = format!("[ {} ]", self.save_label);
        let cancel_text = format!("[ {} ]", self.cancel_label);
        let save_w = save_text.chars().count() as u16;
        let cancel_w = cancel_text.chars().count() as u16;

        let save_rect = Rect::new(area.x, area.y, save_w.min(area.width), 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                save_text,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
            save_rect,
        );
        hit_map.push(save_rect, Zone::BarSave);

        let cancel_x = area.x.saturating_add(save_w).saturating_add(2);
        if cancel_x < area.x + area.width {
            let cancel_rect = Rect::new(
                cancel_x,
                area.y,
                cancel_w.min((area.x + area.width).saturating_sub(cancel_x)),
                1,
            );
            f.render_widget(
                Paragraph::new(Span::styled(cancel_text, Style::default().fg(Color::Red))),
                cancel_rect,
            );
            hit_map.push(cancel_rect, Zone::BarCancel);
        }
    }
}

const CLOSE_W: u16 = 3; // "[×]"
const CLOSE_GAP: u16 = 1; // blank column between the close button and the corner

/// Draw a sheet's bordered frame (`Borders::ALL` + title) and, in Compact,
/// register a `[×]` close button that never collides with the corner or the
/// title text.
///
/// Defect 3 fix: Compact sheets previously reused the Regular/Wide title
/// verbatim (long, hint-laden) and drew the close button directly over the
/// last 3 columns of the border row — which included the corner cell itself,
/// erasing it. Here Compact gets its own short, hint-free `compact_title`,
/// truncated to leave room for the corner + a gap + the close button; the
/// close button is placed one gap column inside the corner so it never
/// overwrites it. Regular/Wide keep the existing full title and no close
/// button (the hint text already spells out `Esc`).
///
/// Returns `block.inner(box_area)`.
pub fn render_sheet_frame(
    f: &mut Frame,
    box_area: Rect,
    compact: bool,
    full_title: &str,
    compact_title: &str,
    border_style: Style,
    hit_map: &mut HitMap,
) -> Rect {
    let title = if compact {
        let reserve = CLOSE_W + CLOSE_GAP; // the corner itself is separate and never touched
        let max_chars = box_area
            .width
            .saturating_sub(2) // both corners
            .saturating_sub(reserve)
            .saturating_sub(2) as usize; // leading + trailing space
        format!(" {} ", crate::view::truncate(compact_title, max_chars))
    } else {
        format!(" {} ", full_title)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    if compact && box_area.width >= CLOSE_W + CLOSE_GAP + 2 {
        let x = box_area.x + box_area.width - 1 - CLOSE_GAP - CLOSE_W;
        let rect = Rect::new(x, box_area.y, CLOSE_W, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                "[×]",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            rect,
        );
        hit_map.push(rect, Zone::SheetClose);
    }

    inner
}

/// Pick the widest contiguous run of *whole* rows (positions into some
/// caller-defined list, e.g. form fields or picker options) that contains
/// `focus_pos` and fits within `avail` rows, expanding forward/backward
/// alternately. Never splits a row's rect across the boundary, so whatever is
/// rendered from `[start, end)` is guaranteed to fit — no overlap, nothing
/// clipped mid-row.
pub fn windowed_rows(heights: &[u16], focus_pos: usize, avail: u16) -> (usize, usize) {
    let n = heights.len();
    if n == 0 {
        return (0, 0);
    }
    let focus_pos = focus_pos.min(n - 1);
    let mut start = focus_pos;
    let mut end = focus_pos + 1;
    let mut total = heights[focus_pos].min(avail);
    loop {
        let mut extended = false;
        if end < n && total + heights[end] <= avail {
            total += heights[end];
            end += 1;
            extended = true;
        }
        if start > 0 && total + heights[start - 1] <= avail {
            total += heights[start - 1];
            start -= 1;
            extended = true;
        }
        if !extended {
            break;
        }
    }
    (start, end)
}
