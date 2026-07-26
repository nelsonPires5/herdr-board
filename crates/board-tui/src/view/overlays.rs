use board_core::model::CommentHistory;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::App;
use crate::widgets::{render_sheet_frame, windowed_rows};

use super::{
    detail::wrapped_row_count, sheet_area, truncate, LayoutMode, HELP_GUTTER_WIDTH, HELP_KEYS,
    HELP_KEY_WIDTH,
};

// -- picker / confirm / help / footer ---------------------------------------

pub(super) fn draw_picker(app: &App, f: &mut Frame, area: Rect) {
    let Some(picker) = &app.picker else { return };
    let mode = app.layout_mode();
    let compact = mode == LayoutMode::Compact;
    let content_w = picker
        .options
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max(picker.title.chars().count() + 12)
        .saturating_add(4)
        .clamp(30, 100) as u16;
    let content_h = (picker.options.len() as u16).saturating_add(2).max(5);
    let box_area = sheet_area(mode, content_w, content_h, area);
    f.render_widget(Clear, box_area);

    let mut hit_map = app.hit_map.borrow_mut();
    let full_title = format!("{} (Enter/Esc)", picker.title);
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        &full_title,
        &picker.title,
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );
    drop(hit_map);

    if compact {
        // Defect 5 fix: options wrap instead of truncating, with the
        // selected option always fully in view (same whole-row windowing as
        // the Compact form fields — see `widgets::windowed_rows`).
        let heights: Vec<u16> = picker
            .options
            .iter()
            .map(|(name, _)| wrapped_row_count(name, inner.width).max(1) as u16)
            .collect();
        let (start, end) = windowed_rows(&heights, picker.sel, inner.height);
        let constraints: Vec<Constraint> = heights[start..end]
            .iter()
            .map(|h| Constraint::Length(*h))
            .collect();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);
        for (row_idx, opt_idx) in (start..end).enumerate() {
            let (name, _) = &picker.options[opt_idx];
            let style = if opt_idx == picker.sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let p = Paragraph::new(name.as_str())
                .style(style)
                .wrap(Wrap { trim: false });
            f.render_widget(p, rows[row_idx]);
        }
        return;
    }

    let items: Vec<ListItem> = picker
        .options
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let style = if i == picker.sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(format!(" {} ", name), style))
        })
        .collect();
    f.render_widget(List::new(items), inner);
}

pub(super) fn draw_move_column(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.move_column else {
        return;
    };
    let name = app
        .board
        .columns
        .iter()
        .find(|c| c.id == state.column_id)
        .map(|c| c.name.as_str())
        .unwrap_or("column");
    let mode = app.layout_mode();
    let box_area = sheet_area(mode, 58, 3, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_sheet_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        "Move column",
        "Move column",
        Style::default().fg(Color::Magenta),
        &mut hit_map,
    );
    drop(hit_map);
    let p = Paragraph::new(Line::from(format!(
        " ←/→ reorder {name} · Enter confirm · Esc cancel "
    )))
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

pub(super) fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some(confirm) = &app.confirm else { return };
    let mode = app.layout_mode();
    let box_area = sheet_area(mode, 50, 5, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_sheet_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        "Confirm",
        "Confirm",
        Style::default().fg(Color::Red),
        &mut hit_map,
    );
    drop(hit_map);
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(confirm.message.as_str()),
        Line::from(Span::styled(
            "[y] yes    [n] no",
            Style::default().fg(Color::Yellow),
        )),
    ])
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: false });
    f.render_widget(p, inner);
}

/// Inner content rect of the help sheet's Compact single-column list, minus
/// the trailing hint row. Shared by `draw_help` and `app::help::help_key` so
/// scroll clamping always agrees with what got drawn.
pub fn help_list_rect(app: &App, area: Rect) -> Rect {
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    let inner = Rect::new(
        box_area.x + 1,
        box_area.y + 1,
        box_area.width.saturating_sub(2),
        box_area.height.saturating_sub(2),
    );
    Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    )
}

/// Text column width available inside `help_list_rect` (1 column reserved for
/// the scrollbar, whether or not it ends up rendered — keeps row-count math
/// identical between draw and scroll clamping regardless of overflow).
pub fn help_content_width(list_rect: Rect) -> u16 {
    list_rect.width.saturating_sub(1).max(1)
}

/// Total wrapped rows the Compact single-column help list needs at `width`.
pub fn help_wrapped_rows(width: u16) -> usize {
    HELP_KEYS
        .iter()
        .map(|(k, d)| {
            if *k == "--" {
                1
            } else {
                wrapped_row_count(&format!("{:<11} {}", k, d), width)
            }
        })
        .sum()
}

pub(super) fn draw_help(app: &App, f: &mut Frame, area: Rect) {
    if app.layout_mode() == LayoutMode::Compact {
        draw_help_compact(app, f, area);
        return;
    }
    // Keep help compact on wide terminals, but use all available space when
    // necessary. Two columns need half the entries plus the border.
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_sheet_frame(
        f,
        box_area,
        false,
        "Help — all keybindings (any key to close)",
        "Help",
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );
    drop(hit_map);

    let mid = HELP_KEYS.len().div_ceil(2);
    let gutter = HELP_GUTTER_WIDTH.min(inner.width.saturating_sub(2));
    let columns_width = inner.width.saturating_sub(gutter);
    let left_width = columns_width / 2;
    let right_width = columns_width.saturating_sub(left_width);
    let left = Rect::new(inner.x, inner.y, left_width, inner.height);
    let right = Rect::new(
        inner.x.saturating_add(left_width).saturating_add(gutter),
        inner.y,
        right_width,
        inner.height,
    );
    render_help_column(f, left, &HELP_KEYS[..mid]);
    render_help_column(f, right, &HELP_KEYS[mid..]);
}

/// Defect 4 fix: Compact help as a single column, one entry per row (wrapping
/// to a second row instead of ellipsizing), with `j`/`k` vertical scroll and a
/// scrollbar — the two-column HELP_KEY_WIDTH layout left ~4 usable chars per
/// description at 40 cols, so every entry showed as `…`.
fn draw_help_compact(app: &App, f: &mut Frame, area: Rect) {
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let block_inner = render_sheet_frame(
        f,
        box_area,
        true,
        "Help — all keybindings (any key to close)",
        "Help",
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );
    drop(hit_map);

    let list_rect = Rect::new(
        block_inner.x,
        block_inner.y,
        block_inner.width,
        block_inner.height.saturating_sub(1),
    );
    let hint_row = Rect::new(
        block_inner.x,
        block_inner.y + block_inner.height.saturating_sub(1),
        block_inner.width,
        1,
    );
    let content_w = help_content_width(list_rect);

    let lines: Vec<Line> = HELP_KEYS
        .iter()
        .map(|(k, d)| {
            if *k == "--" {
                Line::from(Span::styled(
                    format!(" {} ", d.trim_matches(|c| c == '-' || c == ' ')),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(format!("{:<11} ", k), Style::default().fg(Color::Yellow)),
                    Span::raw(*d),
                ])
            }
        })
        .collect();

    let total_rows = help_wrapped_rows(content_w);
    let visible_rows = list_rect.height.max(1) as usize;
    let max_scroll = total_rows.saturating_sub(visible_rows);
    let scroll = app.help_scroll.min(max_scroll);

    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    f.render_widget(
        p,
        Rect::new(list_rect.x, list_rect.y, content_w, list_rect.height),
    );

    if total_rows > visible_rows {
        let sb_rect = Rect::new(
            list_rect.x + list_rect.width.saturating_sub(1),
            list_rect.y,
            1,
            list_rect.height,
        );
        let mut state = ScrollbarState::new(total_rows.max(1))
            .position(scroll)
            .viewport_content_length(visible_rows.max(1));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        f.render_stateful_widget(scrollbar, sb_rect, &mut state);
    }

    f.render_widget(
        Paragraph::new(Span::styled(
            "j/k scroll · Esc close",
            Style::default().fg(Color::DarkGray),
        )),
        hint_row,
    );
}

fn render_help_column(f: &mut Frame, area: Rect, keys: &[(&str, &str)]) {
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, d)| {
            if *k == "--" {
                Line::from(Span::styled(
                    format!(" {} ", d.trim_matches(|c| c == '-' || c == ' ')),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                let description_width = area.width.saturating_sub(HELP_KEY_WIDTH) as usize;
                Line::from(vec![
                    Span::styled(format!("  {:<11}", k), Style::default().fg(Color::Yellow)),
                    Span::raw(truncate(d, description_width)),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

// -- comment history sheet ---------------------------------------------------

const COMMENT_HISTORY_W: u16 = 90;
const COMMENT_HISTORY_H: u16 = 24;

/// Inner content rect of the comment-history sheet, independent of drawing —
/// mirrors what `draw_comment_history` renders into so scroll clamping
/// (`app::comment_history_key`) agrees with what got drawn.
pub fn comment_history_rect(app: &App, area: Rect) -> Rect {
    let box_area = sheet_area(
        app.layout_mode(),
        COMMENT_HISTORY_W,
        COMMENT_HISTORY_H,
        area,
    );
    ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(box_area)
}

/// One entry's header line: `#<n> <created_at>`, plus `· deleted` when the
/// entry records a deletion.
fn comment_history_header(idx: usize, entry: &CommentHistory) -> String {
    let mut header = format!("#{} {}", idx + 1, entry.created_at);
    if entry.deleted_at.is_some() {
        header.push_str(" · deleted");
    }
    header
}

/// Total wrapped rows the comment-history sheet's entries occupy at `width`:
/// one header line plus the wrapped body, per entry, oldest → newest. Shared
/// by `draw_comment_history` and `app::comment_history_key` so scroll
/// clamping matches what is drawn.
pub fn comment_history_wrapped_rows(entries: &[CommentHistory], width: u16) -> usize {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            wrapped_row_count(&comment_history_header(i, e), width)
                + wrapped_row_count(&e.body, width)
        })
        .sum::<usize>()
        .max(1)
}

pub(super) fn draw_comment_history(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.comment_history else {
        return;
    };
    let mode = app.layout_mode();
    let compact = mode == LayoutMode::Compact;
    let box_area = sheet_area(mode, COMMENT_HISTORY_W, COMMENT_HISTORY_H, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        "Comment history (j/k scroll · Esc close)",
        "History",
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );
    drop(hit_map);

    if state.entries.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "(no history)",
                Style::default().fg(Color::Gray),
            )),
            inner,
        );
        return;
    }

    let width = inner.width.max(1);
    let mut lines: Vec<Line> = Vec::with_capacity(state.entries.len() * 2);
    for (i, e) in state.entries.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            comment_history_header(i, e),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(e.body.as_str()));
    }
    let total_rows = comment_history_wrapped_rows(&state.entries, width);
    let visible = inner.height.max(1) as usize;
    let scroll = state.scroll.min(total_rows.saturating_sub(visible));
    let p = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    f.render_widget(p, inner);
}

pub(super) fn draw_footer(app: &App, f: &mut Frame, area: Rect) {
    let y = area.y + area.height.saturating_sub(1);
    let rect = Rect::new(area.x, y, area.width, 1);
    if let Some(toast) = &app.toast {
        let style = if toast.is_error {
            Style::default().fg(Color::White).bg(Color::Red)
        } else {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&format!(" {} ", toast.text), area.width as usize),
                style,
            )),
            rect,
        );
        return;
    }
    let hint = "? help";
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(hint, area.width as usize),
            Style::default().fg(Color::DarkGray),
        )),
        rect,
    );
}
