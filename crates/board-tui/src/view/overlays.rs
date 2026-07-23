use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;

use super::{centered_rect_abs, truncate, HELP_GUTTER_WIDTH, HELP_KEYS, HELP_KEY_WIDTH};

// -- picker / confirm / help / footer ---------------------------------------

pub(super) fn draw_picker(app: &App, f: &mut Frame, area: Rect) {
    let Some(picker) = &app.picker else { return };
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
    let box_area = centered_rect_abs(content_w, content_h, area);
    f.render_widget(Clear, box_area);
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
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(format!(" {} (Enter/Esc) ", picker.title)),
    );
    f.render_widget(list, box_area);
}

pub(super) fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some(confirm) = &app.confirm else { return };
    let box_area = centered_rect_abs(50, 5, area);
    f.render_widget(Clear, box_area);
    let p = Paragraph::new(vec![
        Line::from(""),
        Line::from(confirm.message.as_str()),
        Line::from(Span::styled(
            "[y] yes    [n] no",
            Style::default().fg(Color::Yellow),
        )),
    ])
    .alignment(Alignment::Center)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(" Confirm "),
    );
    f.render_widget(p, box_area);
}

pub(super) fn draw_help(f: &mut Frame, area: Rect) {
    // Keep help compact on wide terminals, but use all available space when
    // necessary. Two columns need half the entries plus the border.
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = centered_rect_abs(110, content_h, area);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" Help — all keybindings (any key to close) ");
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

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
