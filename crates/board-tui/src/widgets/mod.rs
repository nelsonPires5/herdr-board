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
    /// Legacy board-card action marker. The Board renderer deliberately never
    /// registers this zone now that card Edit/Delete are keyboard-only; it is
    /// retained so stale hit-map clients can fail closed without a type break.
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

/// Semantic intent retained by callers; it deliberately does not recolor a
/// button. Every rendered action uses the same transparent white chip style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionTone {
    Normal,
    Primary,
    Destructive,
}

/// One visual control backed by an existing semantic action.
#[derive(Clone, Copy)]
pub struct ActionButton<'a> {
    pub label: &'a str,
    pub compact_label: &'a str,
    pub action: UiAction,
    pub tone: ActionTone,
}

/// Button text is deliberately one shared shape everywhere in the TUI.
/// Keeping the spaces inside the brackets makes the chip's bounds unambiguous
/// without turning its full hit target into a filled button box.
pub fn button_text(label: &str) -> String {
    format!("[ {label} ]")
}

fn fit_button_text(label: &str, max_width: u16) -> Option<String> {
    let full = button_text(label);
    if full.chars().count() as u16 <= max_width {
        return Some(full);
    }
    let name_width = (max_width as usize).saturating_sub(4);
    (name_width > 0).then(|| button_text(&crate::view::truncate(label, name_width)))
}

fn action_button_text(button: &ActionButton<'_>, max_width: u16) -> Option<String> {
    // Prefer a complete compact name over a truncated full name. A chip that
    // says `[ + Card ]` is clearer than `[ + New ca… ]`, and it still follows
    // the exact bracket contract at every width.
    let full = button_text(button.label);
    if full.chars().count() as u16 <= max_width {
        return Some(full);
    }
    let compact = button_text(button.compact_label);
    if compact.chars().count() as u16 <= max_width {
        return Some(compact);
    }
    fit_button_text(button.compact_label, max_width)
}

fn chip_style(modifier: Modifier) -> Style {
    // Chips are deliberately transparent: the surrounding card, border, or
    // sheet remains visible through the hit target. White is the one action
    // color; status colors belong to status glyphs/borders, not controls.
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD | modifier)
}

fn render_chip_text(
    f: &mut Frame,
    area: Rect,
    text: &str,
    hit_map: &mut HitMap,
    zone: Zone,
    modifier: Modifier,
) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let width = text.chars().count() as u16;
    if width == 0 || width > area.width {
        hit_map.push(area, zone);
        return None;
    }
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y,
        width,
        1,
    );
    // Render only the exact chip text. No background is assigned, so the
    // button never paints a white/colored rectangle over its hit zone.
    f.render_widget(
        Paragraph::new(Span::styled(text, chip_style(modifier))),
        rect,
    );
    // Preserve the broad hit target even though only the visible chip is
    // painted. This keeps mouse parity for touch-sized action cells.
    hit_map.push(area, zone);
    Some(rect)
}

fn render_button_chip_with_modifier(
    f: &mut Frame,
    area: Rect,
    label: &str,
    hit_map: &mut HitMap,
    zone: Zone,
    modifier: Modifier,
) -> Option<Rect> {
    let text = fit_button_text(label, area.width)?;
    render_chip_text(f, area, &text, hit_map, zone, modifier)
}

/// Render one exact `[ NAME ]` chip inside `area`. The hit target remains the
/// supplied area so existing mouse behavior does not shrink when the visual
/// control becomes compact.
pub fn render_button_chip_at(
    f: &mut Frame,
    area: Rect,
    label: &str,
    hit_map: &mut HitMap,
    zone: Zone,
) -> Option<Rect> {
    render_button_chip_with_modifier(f, area, label, hit_map, zone, Modifier::empty())
}

/// Render a chip with a non-background focus/selection modifier. Underline
/// and bold keep state visible without turning the chip into a colored button.
pub fn render_button_chip_at_with_modifier(
    f: &mut Frame,
    area: Rect,
    label: &str,
    hit_map: &mut HitMap,
    zone: Zone,
    modifier: Modifier,
) -> Option<Rect> {
    render_button_chip_with_modifier(f, area, label, hit_map, zone, modifier)
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
            let Some(text) = action_button_text(button, rect.width) else {
                hit_map.push(rect, Zone::Action(button.action));
                continue;
            };
            render_chip_text(
                f,
                rect,
                &text,
                hit_map,
                Zone::Action(button.action),
                Modifier::empty(),
            );
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
            let Some(text) = action_button_text(button, rect.width) else {
                hit_map.push(rect, Zone::Action(button.action));
                continue;
            };
            render_chip_text(
                f,
                rect,
                &text,
                hit_map,
                Zone::Action(button.action),
                Modifier::empty(),
            );
        }
    }

    /// Render one already-wrapped detail-action row using the unambiguous
    /// compact labels. The row allocates each control its intrinsic chip
    /// width first, then spreads surplus cells across the hit targets. This
    /// keeps `[ Edit ]`/`[ Archive ]` readable without turning the row into a
    /// filled rail, while preserving generous click zones.
    pub fn render_compact(&self, f: &mut Frame, area: Rect, hit_map: &mut HitMap) {
        if self.buttons.is_empty() || area.is_empty() {
            return;
        }
        let min_widths: Vec<u16> = self
            .buttons
            .iter()
            .map(|button| button_text(button.compact_label).chars().count() as u16)
            .collect();
        let gaps = self.buttons.len().saturating_sub(1) as u16;
        let minimum = min_widths.iter().copied().fold(gaps, u16::saturating_add);
        if minimum > area.width {
            // This is only a last-resort sub-mobile viewport. Keep the
            // semantic zones alive and let the shared fitter choose the best
            // bounded representation rather than dropping a control.
            self.render(f, area, hit_map);
            return;
        }

        let extra = area.width - minimum;
        let per_cell = extra / self.buttons.len() as u16;
        let remainder = extra % self.buttons.len() as u16;
        let mut x = area.x;
        for (idx, (button, min_width)) in self.buttons.iter().zip(min_widths).enumerate() {
            let width = min_width + per_cell + u16::from((idx as u16) < remainder);
            let rect = Rect::new(x, area.y, width, 1);
            let Some(text) = fit_button_text(button.compact_label, width) else {
                hit_map.push(rect, Zone::Action(button.action));
                x = x.saturating_add(width);
                if idx + 1 < self.buttons.len() {
                    x = x.saturating_add(1);
                }
                continue;
            };
            render_chip_text(
                f,
                rect,
                &text,
                hit_map,
                Zone::Action(button.action),
                Modifier::empty(),
            );
            x = x.saturating_add(width);
            if idx + 1 < self.buttons.len() {
                x = x.saturating_add(1);
            }
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

    /// Render equal action cells while painting only their exact chips.
    /// `area` should be a single row.
    pub fn render(&self, f: &mut Frame, area: Rect, hit_map: &mut HitMap) {
        if area.is_empty() {
            return;
        }
        let save_w = area.width / 2;
        let save = Rect::new(area.x, area.y, save_w, 1);
        let cancel = Rect::new(area.x + save_w, area.y, area.width - save_w, 1);
        render_button_chip_at(f, save, self.save_label, hit_map, Zone::BarSave);
        render_button_chip_at(f, cancel, self.cancel_label, hit_map, Zone::BarCancel);
    }
}

const CLOSE_W: u16 = 5; // "[ X ]"
const CLOSE_GAP: u16 = 1; // blank column between the close button and the corner

/// Draw a sheet's bordered frame (`Borders::ALL` + title) and, in Compact,
/// register a `[ X ]` close button that never collides with the corner or the
/// title text.
///
/// Defect 3 fix: Compact sheets previously reused the Regular/Wide title
/// verbatim (long, hint-laden) and drew the close button directly over the
/// last 5 columns of the border row — which included the corner cell itself,
/// erasing it. Here Compact gets its own short, hint-free `compact_title`,
/// truncated to leave room for the corner + a gap + the close button; the
/// close button is placed one gap column inside the corner so it never
/// overwrites it. Regular/Wide keep the existing full title; their overlay
/// callers place the same exact `[ X ]` chip in the title row.
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
        // Compact sheets use the same exact chip as every other button. The
        // old compact close glyph had no interior spacing and was easy to confuse
        // with a border corner at narrow widths.
        render_button_chip_at(f, rect, "X", hit_map, Zone::SheetClose);
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
    fn action_strip_paints_exact_transparent_white_button_chips() {
        let buttons = [
            ActionButton {
                label: "Create a new card",
                compact_label: "New",
                action: UiAction::NewCard,
                tone: ActionTone::Primary,
            },
            ActionButton {
                label: "Delete this selected card",
                compact_label: "Delete",
                action: UiAction::DeleteCard,
                tone: ActionTone::Destructive,
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        let mut hit_map = HitMap::default();
        terminal
            .draw(|f| {
                f.render_widget(
                    Block::default().style(Style::default().bg(Color::Rgb(7, 22, 34))),
                    f.area(),
                );
                ActionStrip { buttons: &buttons }.render(f, f.area(), &mut hit_map)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let expected_bg = Color::Rgb(7, 22, 34);
        let button_cells: Vec<_> = (0..40)
            .map(|x| &buffer[(x, 0)])
            .filter(|cell| cell.fg == Color::White)
            .collect();
        assert_eq!(
            button_cells.len(),
            button_text("New").chars().count() + button_text("Delete").chars().count()
        );
        assert!(button_cells.iter().all(|cell| cell.bg == expected_bg));
        assert!((0..40)
            .map(|x| &buffer[(x, 0)])
            .all(|cell| cell.bg == expected_bg));
    }

    #[test]
    fn selected_chip_uses_underline_without_a_background() {
        let width = 20;
        let label = "Active";
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        let mut hit_map = HitMap::default();
        terminal
            .draw(|f| {
                f.render_widget(
                    Block::default().style(Style::default().bg(Color::Rgb(7, 22, 34))),
                    f.area(),
                );
                render_button_chip_at_with_modifier(
                    f,
                    f.area(),
                    label,
                    &mut hit_map,
                    Zone::Filter(CardFilter::Active),
                    Modifier::UNDERLINED,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let chip_x = (width - button_text(label).chars().count() as u16) / 2;
        for x in chip_x..chip_x + button_text(label).chars().count() as u16 {
            let cell = &buffer[(x, 0)];
            assert_eq!(cell.fg, Color::White);
            assert_eq!(cell.bg, Color::Rgb(7, 22, 34));
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
        }
    }

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
