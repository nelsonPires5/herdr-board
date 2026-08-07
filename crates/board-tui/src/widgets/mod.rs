//! Minimal hit-testing + button-bar widgets for the mobile-first prototype.
//!
//! `HitMap` is rebuilt every frame in `view()` and stashed on `App` (in a
//! `RefCell`) so the mouse handler can look up what the last frame drew at a
//! given cell without duplicating layout math. Keep this additive: existing
//! board/detail hit-testing (`view::board_layout`, `view::detail_layout`)
//! is untouched.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::CardFilter;

/// The one vertical scrollbar every overflowing list in the TUI draws: board
/// columns, the form's field list, and the Compact help sheet. Same track/thumb
/// glyphs everywhere, so the three call sites cannot drift apart.
///
/// `total`/`visible` are content lengths in rows, `position` the current top
/// offset; each is floored at 1 because `ScrollbarState` divides by them.
pub fn vertical_scrollbar(
    f: &mut Frame,
    rect: Rect,
    total: usize,
    position: usize,
    visible: usize,
) {
    let mut state = ScrollbarState::new(total.max(1))
        .position(position)
        .viewport_content_length(visible.max(1));
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .track_symbol(Some("│"))
        .thumb_symbol("█");
    f.render_stateful_widget(scrollbar, rect, &mut state);
}

/// Semantic user actions exposed by visual controls.
///
/// A zone never executes I/O. `app::mouse` validates the active screen and
/// translates the action to the exact existing key/reducer path, preserving
/// all guards, pickers, confirmations, effects, return screens, and toasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiAction {
    Help,
    Quit,
    SwitchBoard,
    NewCard,
    NewColumn,
    EditCard,
    EditColumn,
    ArchiveCard,
    CycleFilter,
    DeleteCard,
    DeleteColumn,
    MoveCard,
    MoveColumn,
    ShoveCardLeft,
    ShoveCardRight,
    OpenCard,
    ApplyTemplate,
    Refresh,
    ConfirmAwaiting,
    AddComment,
    DeleteComment,
    CommentHistory,
    ToggleDetail,
    FocusRunPane,
    CancelRun,
    RetryRun,
    CloseDetail,
    SubmitForm,
    CancelForm,
    EditInExternalEditor,
    ChoosePickerRow,
    PickerOtherBoard,
    CancelPicker,
    ConfirmYes,
    ConfirmNo,
    StageColumnLeft,
    StageColumnRight,
    CommitColumnMove,
    CancelColumnMove,
    ChooseSwitcherRow,
    CloseSwitcher,
    CloseCommentHistory,
    CloseHelp,
}

/// Interactive zones registered by the new Compact-mode widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// A screen-validated action routed through the existing key reducer.
    Action(UiAction),
    /// One of the three directly selectable board visibility filters.
    Filter(CardFilter),
    /// An action rendered inside a specific board card.
    CardAction {
        col_idx: usize,
        card_idx: usize,
        action: UiAction,
    },
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
    /// A visible form field, by stable `form.fields` index.
    FormField(usize),
    FormChoicePrev(usize),
    FormChoiceNext(usize),
    FormEditor(usize),
    /// A visible picker option, by absolute model index.
    PickerRow(usize),
    HelpScrollUp,
    HelpScrollDown,
    HistoryScrollUp,
    HistoryScrollDown,
    /// Modal background shield; child zones are pushed later and win.
    Shield,
    /// `ButtonBar` save action.
    BarSave,
    /// `ButtonBar` cancel action.
    BarCancel,
    /// Sheet title-bar close `×`.
    SheetClose,
    /// A rendered comment row in the card detail's comments section, by index
    /// into `CardDetail::comments`.
    CommentRow(usize),
    /// A rendered run row in the card detail runs section, by index.
    RunRow(usize),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTone {
    Normal,
    Primary,
    Destructive,
}

/// One visual control backed by an existing semantic action.
pub struct ActionButton<'a> {
    pub label: &'a str,
    pub compact_label: &'a str,
    pub action: UiAction,
    pub tone: ActionTone,
}

/// Equal-width, click-first action row used by board/detail/forms/sheets.
///
/// The bar only draws and registers zones. The reducer path is selected later
/// by `app::mouse`, so a click cannot bypass an existing guard or confirmation.
pub struct ActionBar<'a> {
    pub buttons: &'a [ActionButton<'a>],
}

impl<'a> ActionBar<'a> {
    pub fn render(&self, f: &mut Frame, area: Rect, hit_map: &mut HitMap) {
        if self.buttons.is_empty() || area.is_empty() {
            return;
        }
        let count = self.buttons.len() as u32;
        let constraints = self
            .buttons
            .iter()
            .map(|_| Constraint::Ratio(1, count))
            .collect::<Vec<_>>();
        let rects = Layout::horizontal(constraints).spacing(1).split(area);
        for (button, rect) in self.buttons.iter().zip(rects.iter().copied()) {
            if rect.is_empty() {
                continue;
            }
            let compact = rect.width < button.label.chars().count() as u16 + 4;
            let label = if compact {
                button.compact_label
            } else {
                button.label
            };
            // Button aesthetic: white background, black text; the semantic
            // color lives only on the label text inside the brackets.
            let (_label_fg, modifier) = match button.tone {
                ActionTone::Normal => (Color::Black, Modifier::empty()),
                ActionTone::Primary => (Color::Rgb(0, 90, 200), Modifier::BOLD),
                ActionTone::Destructive => (Color::Rgb(190, 30, 30), Modifier::BOLD),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Gray))
                .style(Style::default().bg(Color::White));
            let text = format!("[ {label} ]");
            f.render_widget(
                Paragraph::new(Span::styled(
                    text,
                    Style::default().fg(Color::Black).add_modifier(modifier),
                ))
                .style(Style::default().bg(Color::White))
                .block(block),
                rect,
            );
            hit_map.push(rect, Zone::Action(button.action));
        }
    }
}

/// Dense one-row companion to [`ActionBar`] for secondary operations.
/// Every segment remains a complete hit target; narrow layouts use the
/// shortcut-sized compact label rather than clipping the full action name.
pub struct ActionStrip<'a> {
    pub buttons: &'a [ActionButton<'a>],
}

impl<'a> ActionStrip<'a> {
    pub fn render(&self, f: &mut Frame, area: Rect, hit_map: &mut HitMap) {
        if self.buttons.is_empty() || area.is_empty() {
            return;
        }
        let count = self.buttons.len() as u32;
        let rects = Layout::horizontal(
            self.buttons
                .iter()
                .map(|_| Constraint::Ratio(1, count))
                .collect::<Vec<_>>(),
        )
        .split(area);
        for (button, rect) in self.buttons.iter().zip(rects.iter().copied()) {
            if rect.is_empty() {
                continue;
            }
            let full = format!("[ {} ]", button.label);
            let compact = format!("[{}]", button.compact_label);
            let label = if full.chars().count() as u16 <= rect.width {
                full
            } else {
                crate::view::truncate(&compact, rect.width as usize)
            };
            // White background, black text; the semantic tone is carried only
            // by the label characters inside the brackets.
            let label_style = match button.tone {
                ActionTone::Normal => Style::default().fg(Color::Black),
                ActionTone::Primary => Style::default()
                    .fg(Color::Rgb(0, 90, 200))
                    .add_modifier(Modifier::BOLD),
                ActionTone::Destructive => Style::default()
                    .fg(Color::Rgb(190, 30, 30))
                    .add_modifier(Modifier::BOLD),
            };
            f.render_widget(
                Paragraph::new(Span::styled(label, label_style))
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(Style::default().bg(Color::White)),
                rect,
            );
            hit_map.push(rect, Zone::Action(button.action));
        }
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

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn action_bar_registers_one_semantic_zone_per_visible_button() {
        let buttons = [
            ActionButton {
                label: "New card",
                compact_label: "New",
                action: UiAction::NewCard,
                tone: ActionTone::Primary,
            },
            ActionButton {
                label: "Delete card",
                compact_label: "Delete",
                action: UiAction::DeleteCard,
                tone: ActionTone::Destructive,
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        let mut hit_map = HitMap::default();
        terminal
            .draw(|f| ActionBar { buttons: &buttons }.render(f, f.area(), &mut hit_map))
            .unwrap();

        assert_eq!(hit_map.hit(1, 1), Some(Zone::Action(UiAction::NewCard)));
        assert_eq!(hit_map.hit(21, 1), Some(Zone::Action(UiAction::DeleteCard)));
    }
}
