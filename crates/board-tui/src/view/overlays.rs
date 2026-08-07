use board_core::model::CommentHistory;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, PickerPurpose, Screen};
use crate::widgets::{
    render_sheet_frame, windowed_rows, ActionButton, ActionStrip, ActionTone, HitMap, UiAction,
    Zone,
};

use super::{
    detail::wrapped_row_count, sheet_area, truncate, LayoutMode, HELP_GUTTER_WIDTH, HELP_KEYS,
    HELP_KEY_TEXT, HELP_KEY_WIDTH,
};

// -- shared sheet chrome -----------------------------------------------------

/// Draw the existing sheet frame and add the same discoverable close affordance
/// to centered sheets that compact sheets already have.  The action remains
/// `SheetClose -> Esc`; this is chrome only, never a second reducer path.
fn render_overlay_frame(
    f: &mut Frame,
    box_area: Rect,
    compact: bool,
    titles: (&str, &str),
    border_style: Style,
    close_label: &str,
    hit_map: &mut HitMap,
) -> Rect {
    let centered_title;
    let frame_title = if compact {
        titles.0
    } else {
        // Keep dynamic titles from painting through the trailing close control.
        // Fixed action labels win; only hostile/dynamic title data ellipsizes.
        let close_width = close_label.chars().count().saturating_add(2);
        let max_title = box_area
            .width
            .saturating_sub(2) // corners
            .saturating_sub(close_width as u16)
            .saturating_sub(3) as usize; // title padding + visual gap
        centered_title = truncate(titles.0, max_title);
        &centered_title
    };
    hit_map.push(box_area, Zone::Shield);
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        frame_title,
        titles.1,
        border_style,
        hit_map,
    );
    if !compact && box_area.width > close_label.chars().count() as u16 + 4 {
        let text = format!("[{close_label}]");
        let width = text.chars().count() as u16;
        let rect = Rect::new(
            box_area.right().saturating_sub(width + 2),
            box_area.y,
            width,
            1,
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                text,
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
    // A wrapped option may need more than one row.  Let centered sheets grow
    // to their content preference; `sheet_area` still clamps to main_area.
    let desired_rows: usize = picker
        .options
        .iter()
        .map(|(name, _)| wrapped_row_count(name, content_w.saturating_sub(3).max(1)))
        .sum();
    let content_h = (desired_rows as u16).saturating_add(2).max(5);
    let box_area = sheet_area(mode, content_w, content_h, area);
    f.render_widget(Clear, box_area);

    let visual_title = picker
        .title
        .replace("Move card to which column?", "Move card to column")
        .replace(" · b = other board", " · b other board");
    let compact_move_title = visual_title
        .split(" ·")
        .next()
        .unwrap_or(visual_title.as_str());
    let compact_title = match picker.purpose {
        PickerPurpose::SwitchBoard => "Switch board",
        PickerPurpose::MoveCardPickBoard { .. } => "Move to board",
        PickerPurpose::MoveCardPickColumn { .. } => compact_move_title,
        PickerPurpose::DeleteColumnMoveTo { .. } => "Move column cards",
    };
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        compact,
        (&visual_title, compact_title),
        Style::default().fg(Color::LightBlue),
        "Close",
        &mut hit_map,
    );
    let other_board = matches!(picker.purpose, PickerPurpose::MoveCardPickColumn { .. });
    let picker_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(u16::from(other_board)),
    );
    if other_board && inner.height > 0 {
        let action_area = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let buttons = [ActionButton {
            label: "[Other board]",
            compact_label: "Other board",
            action: UiAction::PickerOtherBoard,
            tone: ActionTone::Normal,
        }];
        ActionStrip { buttons: &buttons }.render(f, action_area, &mut hit_map);
    }

    if picker.options.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "(no choices available)",
                Style::default().fg(Color::DarkGray),
            )),
            picker_area,
        );
        return;
    }

    // Always reserve a gutter when overflow is possible.  This makes wrapping
    // and row windows identical before/after selection or resize.
    let preliminary: Vec<u16> = picker
        .options
        .iter()
        .map(|(name, _)| {
            wrapped_row_count(name, picker_area.width.saturating_sub(2).max(1)).max(1) as u16
        })
        .collect();
    let overflow =
        preliminary.iter().map(|&h| h as usize).sum::<usize>() > picker_area.height as usize;
    let text_w = picker_area
        .width
        .saturating_sub(if overflow { 1 } else { 0 })
        .max(1);
    let heights: Vec<u16> = picker
        .options
        .iter()
        .map(|(name, _)| {
            // One cell is the selection marker; text itself remains whole-row wrapped.
            wrapped_row_count(name, text_w.saturating_sub(1).max(1)).max(1) as u16
        })
        .collect();
    let (start, end) = windowed_rows(&heights, picker.sel, picker_area.height);
    let constraints = heights[start..end]
        .iter()
        .copied()
        .map(Constraint::Length)
        .collect::<Vec<_>>();
    let row_area = Rect::new(picker_area.x, picker_area.y, text_w, picker_area.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(row_area);
    for (visible, option_idx) in (start..end).enumerate() {
        let (name, _) = &picker.options[option_idx];
        let selected = option_idx == picker.sel;
        let line = Line::from(vec![
            Span::styled(
                if selected { "›" } else { " " },
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                name.as_str(),
                if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).wrap(Wrap { trim: false }),
            rows[visible],
        );
        hit_map.push(rows[visible], Zone::PickerRow(option_idx));
    }
    if overflow {
        crate::widgets::vertical_scrollbar(
            f,
            Rect::new(
                picker_area.right().saturating_sub(1),
                picker_area.y,
                1,
                picker_area.height,
            ),
            heights.iter().map(|&h| h as usize).sum(),
            heights[..start].iter().map(|&h| h as usize).sum(),
            picker_area.height as usize,
        );
    }
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
    let box_area = sheet_area(mode, 58, 5, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Move column", "Move column"),
        Style::default().fg(Color::Magenta),
        "Close",
        &mut hit_map,
    );
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let move_controls = Layout::horizontal([
        Constraint::Length(9.min(chunks[0].width / 3)),
        Constraint::Min(1),
        Constraint::Length(10.min(chunks[0].width / 3)),
    ])
    .split(chunks[0]);
    f.render_widget(
        Paragraph::new("[← Left]").alignment(Alignment::Center),
        move_controls[0],
    );
    f.render_widget(
        Paragraph::new(format!("reorder {name}"))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        move_controls[1],
    );
    f.render_widget(
        Paragraph::new("[Right →]").alignment(Alignment::Center),
        move_controls[2],
    );
    hit_map.push(move_controls[0], Zone::Action(UiAction::StageColumnLeft));
    hit_map.push(move_controls[2], Zone::Action(UiAction::StageColumnRight));
    let buttons = [
        ActionButton {
            label: "[Confirm]",
            compact_label: "Confirm",
            action: UiAction::CommitColumnMove,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "[Cancel]",
            compact_label: "Cancel",
            action: UiAction::CancelColumnMove,
            tone: ActionTone::Normal,
        },
    ];
    ActionStrip { buttons: &buttons }.render(f, chunks[1], &mut hit_map);
}

pub(super) fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some(confirm) = &app.confirm else { return };
    let mode = app.layout_mode();
    let target_w = if mode == LayoutMode::Compact {
        area.width
    } else {
        50
    };
    let message_w = target_w.saturating_sub(4).max(1);
    let message_rows = wrapped_row_count(&confirm.message, message_w).max(1) as u16;
    let box_area = sheet_area(mode, 50, message_rows.saturating_add(4).max(5), area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Confirm", "Confirm"),
        Style::default().fg(Color::Red),
        "Close",
        &mut hit_map,
    );
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let message_area = Rect::new(
        chunks[0].x,
        chunks[0].y + chunks[0].height.saturating_sub(message_rows) / 2,
        chunks[0].width,
        message_rows.min(chunks[0].height),
    );
    f.render_widget(
        Paragraph::new(confirm.message.as_str())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        message_area,
    );
    let buttons = [
        ActionButton {
            label: "[Yes]",
            compact_label: "Yes",
            action: UiAction::ConfirmYes,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "[No]",
            compact_label: "No",
            action: UiAction::ConfirmNo,
            tone: ActionTone::Normal,
        },
    ];
    ActionStrip { buttons: &buttons }.render(f, chunks[1], &mut hit_map);
}

/// Inner content rect of the help sheet's wrapped single-column list, minus
/// the trailing hint row. Regular widths deliberately use this same geometry.
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

pub fn help_content_width(list_rect: Rect) -> u16 {
    list_rect.width.saturating_sub(1).max(1)
}

pub fn help_wrapped_rows(width: u16) -> usize {
    HELP_KEYS
        .iter()
        .map(|(_, k, d)| {
            if *k == "--" {
                1
            } else {
                wrapped_row_count(&format!("{:<11} {}", k, d), width)
            }
        })
        .sum()
}

pub fn help_regular_max_scroll(app: &App, area: Rect) -> usize {
    if app.layout_mode() != LayoutMode::Wide {
        let rect = help_list_rect(app, area);
        return help_wrapped_rows(help_content_width(rect))
            .saturating_sub(rect.height.max(1) as usize);
    }
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    let visible = box_area.height.saturating_sub(2) as usize;
    HELP_KEYS.len().div_ceil(2).saturating_sub(visible)
}

pub(super) fn draw_help(app: &App, f: &mut Frame, area: Rect) {
    if app.layout_mode() != LayoutMode::Wide {
        draw_help_wrapped(app, f, area);
        return;
    }
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        false,
        ("Help — all keybindings (j/k scroll)", "Help"),
        Style::default().fg(Color::LightBlue),
        "Close",
        &mut hit_map,
    );
    let scroll = app.help_scroll.min(help_regular_max_scroll(app, area)) as u16;
    let mid = HELP_KEYS.len().div_ceil(2);
    let gutter = HELP_GUTTER_WIDTH.min(inner.width.saturating_sub(2));
    let columns_width = inner.width.saturating_sub(gutter);
    let left_width = columns_width / 2;
    let left = Rect::new(inner.x, inner.y, left_width, inner.height);
    let right = Rect::new(
        inner.x + left_width + gutter,
        inner.y,
        columns_width - left_width,
        inner.height,
    );
    render_help_column(f, left, &HELP_KEYS[..mid], scroll);
    render_help_column(f, right, &HELP_KEYS[mid..], scroll);
}

fn draw_help_wrapped(app: &App, f: &mut Frame, area: Rect) {
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area(app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let block_inner = render_overlay_frame(
        f,
        box_area,
        app.layout_mode() == LayoutMode::Compact,
        ("Help — all keybindings (j/k scroll)", "Help"),
        Style::default().fg(Color::LightBlue),
        "Close",
        &mut hit_map,
    );
    let list_rect = Rect::new(
        block_inner.x,
        block_inner.y,
        block_inner.width,
        block_inner.height.saturating_sub(1),
    );
    let hint_row = Rect::new(
        block_inner.x,
        block_inner.bottom().saturating_sub(1),
        block_inner.width,
        1,
    );
    let content_w = help_content_width(list_rect);
    let lines: Vec<Line> = HELP_KEYS
        .iter()
        .map(|(_, k, d)| {
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
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        Rect::new(list_rect.x, list_rect.y, content_w, list_rect.height),
    );
    if total_rows > visible_rows {
        let sb = Rect::new(
            list_rect.right().saturating_sub(1),
            list_rect.y,
            1,
            list_rect.height,
        );
        crate::widgets::vertical_scrollbar(f, sb, total_rows, scroll, visible_rows);
        if sb.height > 0 {
            let up = Rect::new(sb.x, sb.y, 1, 1);
            f.render_widget(Paragraph::new("▲"), up);
            hit_map.push(up, Zone::HelpScrollUp);
        }
        if sb.height > 1 {
            let down = Rect::new(sb.x, sb.bottom() - 1, 1, 1);
            f.render_widget(Paragraph::new("▼"), down);
            hit_map.push(down, Zone::HelpScrollDown);
        }
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            "j/k scroll · Esc close",
            Style::default().fg(Color::DarkGray),
        )),
        hint_row,
    );
}

fn render_help_column(f: &mut Frame, area: Rect, keys: &[(Screen, &str, &str)], scroll: u16) {
    debug_assert_eq!(HELP_KEY_WIDTH as usize, HELP_KEY_TEXT + 2);
    let lines: Vec<Line> = keys
        .iter()
        .map(|(_, k, d)| {
            if *k == "--" {
                Line::from(Span::styled(
                    format!(" {} ", d.trim_matches(|c| c == '-' || c == ' ')),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!(
                            "  {:<width$}",
                            truncate(k, HELP_KEY_TEXT),
                            width = HELP_KEY_TEXT
                        ),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(*d),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

// -- comment history sheet ---------------------------------------------------

const COMMENT_HISTORY_W: u16 = 90;
const COMMENT_HISTORY_H: u16 = 24;

pub fn comment_history_rect(app: &App, area: Rect) -> Rect {
    let box_area = sheet_area(
        app.layout_mode(),
        COMMENT_HISTORY_W,
        COMMENT_HISTORY_H,
        area,
    );
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(box_area);
    Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(1).max(1),
        inner.height,
    )
}

fn comment_history_header(idx: usize, entry: &CommentHistory) -> String {
    let mut header = format!("#{} {}", idx + 1, entry.created_at);
    if entry.deleted_at.is_some() {
        header.push_str(" · deleted");
    }
    header
}

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
    let box_area = sheet_area(mode, COMMENT_HISTORY_W, COMMENT_HISTORY_H, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Comment history (j/k scroll)", "History"),
        Style::default().fg(Color::LightBlue),
        "Close",
        &mut hit_map,
    );
    let content = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(1).max(1),
        inner.height,
    );
    if state.entries.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "(no history)",
                Style::default().fg(Color::Gray),
            )),
            content,
        );
        return;
    }
    let mut lines = Vec::with_capacity(state.entries.len() * 2);
    for (i, e) in state.entries.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            comment_history_header(i, e),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(e.body.as_str()));
    }
    let total_rows = comment_history_wrapped_rows(&state.entries, content.width);
    let visible = content.height.max(1) as usize;
    let scroll = state.scroll.min(total_rows.saturating_sub(visible));
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        content,
    );
    if total_rows > visible {
        let sb = Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height);
        crate::widgets::vertical_scrollbar(f, sb, total_rows, scroll, visible);
        if sb.height > 0 {
            let up = Rect::new(sb.x, sb.y, 1, 1);
            f.render_widget(Paragraph::new("▲"), up);
            hit_map.push(up, Zone::HistoryScrollUp);
        }
        if sb.height > 1 {
            let down = Rect::new(sb.x, sb.bottom() - 1, 1, 1);
            f.render_widget(Paragraph::new("▼"), down);
            hit_map.push(down, Zone::HistoryScrollDown);
        }
    }
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
    let (hint, help_clickable) = match app.screen {
        Screen::CardForm | Screen::ColumnForm => ("Tab fields · Enter save · Esc cancel", false),
        Screen::Help => ("j/k scroll · Esc close", false),
        Screen::Board => ("drag card to move · double-click to open", false),
        Screen::CardDetail
        | Screen::Picker
        | Screen::MoveColumn
        | Screen::Confirm
        | Screen::Switcher
        | Screen::CommentHistory => ("? help", true),
    };
    let shown = truncate(hint, area.width as usize);
    let width = shown.chars().count() as u16;
    f.render_widget(
        Paragraph::new(Span::styled(shown, Style::default().fg(Color::DarkGray))),
        rect,
    );
    if help_clickable && width > 0 {
        app.hit_map.borrow_mut().push(
            Rect::new(rect.x, rect.y, width, 1),
            Zone::Action(UiAction::Help),
        );
    }
}
