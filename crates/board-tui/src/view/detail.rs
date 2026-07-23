use board_core::engine::format_duration;
use board_core::protocol::CardDetail;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailScrollTarget};

use super::{
    centered_rect_abs, main_area, parse_epoch, status_glyph, status_label, truncate,
    NARROW_DETAIL_WIDTH,
};

// -- detail ------------------------------------------------------------------

fn detail_panel_area(app: &App, area: Rect) -> Rect {
    let m = main_area(area);
    if app.detail_fullscreen {
        m
    } else {
        centered_rect_abs(120, 30, m)
    }
}

/// Click target for the popup/fullscreen action rendered in the detail title.
pub fn detail_toggle_rect(app: &App, area: Rect) -> Rect {
    let panel = detail_panel_area(app, area);
    let label_w = if app.detail_fullscreen { 11 } else { 16 };
    Rect::new(
        panel.x + panel.width.saturating_sub(label_w + 1),
        panel.y,
        label_w,
        1,
    )
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    text.lines()
        .map(|line| line.chars().count().max(1).div_ceil(width) as u16)
        .sum::<u16>()
        .max(1)
}

/// Size sections by content. Surplus height stays outside their borders; when
/// content exceeds the viewport, rows go to the greatest unmet demand first.
fn detail_section_heights(detail: &CardDetail, width: u16, available: u16) -> ([u16; 3], u16) {
    let desc_lines = wrapped_line_count(&detail.card.description, width);
    // Comments currently render as one truncated row each; size the section by
    // visible list rows so long bodies do not create blank phantom height.
    let comment_lines = (detail.comments.len() as u16).max(1);
    let run_lines = (detail.runs.len() as u16).max(1);
    // One additional row for each section's titled divider.
    let needs = [desc_lines + 1, comment_lines + 1, run_lines + 1];

    let minimum = if available >= 6 { 2 } else { available / 3 };
    let mut heights = [minimum; 3];
    let mut remaining = available.saturating_sub(minimum.saturating_mul(3));
    while remaining > 0 {
        let Some((idx, deficit)) = (0..3)
            .map(|idx| (idx, needs[idx].saturating_sub(heights[idx])))
            .max_by_key(|(_, deficit)| *deficit)
        else {
            break;
        };
        if deficit == 0 {
            break;
        }
        heights[idx] += 1;
        remaining -= 1;
    }
    (heights, remaining)
}

pub struct DetailLayout {
    pub panel: Rect,
    pub status: Rect,
    pub description: Rect,
    pub comments: Rect,
    pub runs: Rect,
}

/// Geometry shared by rendering and independent comments/runs mouse scrolling.
pub fn detail_layout(app: &App, area: Rect) -> DetailLayout {
    let panel = detail_panel_area(app, area);
    let inner = Block::default().borders(Borders::ALL).inner(panel);
    let Some(detail) = &app.detail else {
        return DetailLayout {
            panel,
            status: inner,
            description: inner,
            comments: inner,
            runs: inner,
        };
    };
    // Narrow detail panels dedicate one line to the status/reason and two to
    // metadata. This keeps every value visible without stealing the minimum
    // content rows from description, comments, or runs.
    let status_h = if panel.width < NARROW_DETAIL_WIDTH {
        4
    } else {
        3
    };
    let content_budget = inner.height.saturating_sub(status_h);
    let (section_h, spacer_h) =
        detail_section_heights(detail, inner.width.saturating_sub(1), content_budget);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_h),
            Constraint::Length(section_h[0]),
            Constraint::Length(spacer_h),
            Constraint::Length(section_h[1]),
            Constraint::Length(section_h[2]),
        ])
        .split(inner);
    DetailLayout {
        panel,
        status: chunks[0],
        description: chunks[1],
        comments: chunks[3],
        runs: chunks[4],
    }
}

pub(super) fn detail_section_title(
    name: &str,
    total: usize,
    offset: usize,
    visible: usize,
) -> String {
    let hidden_above = offset > 0;
    let hidden_below = offset.saturating_add(visible.max(1)) < total;
    let arrows = match (hidden_above, hidden_below) {
        (true, true) => " ↑↓",
        (true, false) => " ↑",
        (false, true) => " ↓",
        (false, false) => "",
    };
    format!("{name}{arrows}")
}

fn push_detail_field(
    spans: &mut Vec<Span<'static>>,
    label: &'static str,
    value: String,
    color: Color,
) {
    spans.push(Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(value, Style::default().fg(Color::White)));
    spans.push(Span::raw("   "));
}

pub(super) fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    let Some(detail) = &app.detail else { return };
    let layout = detail_layout(app, area);
    let panel = layout.panel;
    f.render_widget(Clear, panel);
    let card = &detail.card;

    let action = if app.detail_fullscreen {
        "[f Popup]"
    } else {
        "[f Fullscreen]"
    };
    let title_width = panel.width.saturating_sub(2) as usize;
    let left = format!(" Card #{}: {} ", card.id, truncate(&card.title, 48));
    let left = truncate(&left, title_width.saturating_sub(action.len() + 1));
    let gap = title_width.saturating_sub(left.chars().count() + action.len());
    let title = format!("{}{}{}", left, " ".repeat(gap), action);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(title);
    f.render_widget(block, panel);

    let (gl, gc) = status_glyph(card.status);
    let narrow = panel.width < NARROW_DETAIL_WIDTH;
    let mut status_line = vec![Span::styled(
        format!("{} {}", gl, status_label(card)),
        Style::default().fg(gc).add_modifier(Modifier::BOLD),
    )];
    if card.archived_at.is_some() {
        status_line.push(Span::raw("   "));
        status_line.push(Span::styled(
            "▣ ARCHIVED",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let mut runtime_line = Vec::new();
    push_detail_field(
        &mut runtime_line,
        "harness: ",
        card.harness.clone(),
        Color::LightBlue,
    );
    push_detail_field(
        &mut runtime_line,
        "model: ",
        card.model.clone().unwrap_or_else(|| "default".into()),
        Color::LightBlue,
    );
    push_detail_field(
        &mut runtime_line,
        "effort: ",
        card.effort
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| "default".into()),
        Color::LightBlue,
    );
    let mut config_line = Vec::new();
    push_detail_field(
        &mut config_line,
        "permission: ",
        card.permission_mode
            .clone()
            .unwrap_or_else(|| "default".into()),
        Color::LightBlue,
    );
    push_detail_field(
        &mut config_line,
        "session: ",
        card.session.clone().unwrap_or_else(|| "default".into()),
        Color::LightBlue,
    );
    push_detail_field(
        &mut config_line,
        "space: ",
        format!(
            "{}:{}",
            card.space_kind.as_str(),
            card.space_ref.as_deref().unwrap_or("-")
        ),
        Color::LightBlue,
    );
    let status_lines = if narrow {
        vec![
            Line::from(status_line),
            Line::from(runtime_line),
            Line::from(config_line),
        ]
    } else {
        status_line.push(Span::raw("   "));
        status_line.append(&mut runtime_line);
        vec![Line::from(status_line), Line::from(config_line)]
    };
    let status = Paragraph::new(status_lines).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Gray))
            .title("status"),
    );
    f.render_widget(status, layout.status);

    let desc = Paragraph::new(card.description.as_str())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::Gray))
                .title("description"),
        );
    f.render_widget(desc, layout.description);

    let comments: Vec<ListItem> = detail
        .comments
        .iter()
        .skip(app.detail_comments_scroll)
        .map(|c| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", c.author),
                    Style::default().fg(Color::LightCyan),
                ),
                Span::raw(truncate(
                    &c.body,
                    layout.comments.width.saturating_sub(2) as usize,
                )),
            ]))
        })
        .collect();
    let comments = if detail.comments.is_empty() {
        vec![ListItem::new(Span::styled(
            "(no comments)",
            Style::default().fg(Color::Gray),
        ))]
    } else {
        comments
    };
    let comments_active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let comments_total = detail.comments.len();
    let comments_visible = layout.comments.height.saturating_sub(1) as usize;
    let comments_title = detail_section_title(
        "comments",
        comments_total,
        app.detail_comments_scroll,
        comments_visible,
    );
    f.render_widget(
        List::new(comments).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if comments_active {
                    Color::Blue
                } else {
                    Color::Gray
                }))
                .title(comments_title),
        ),
        layout.comments,
    );

    let runs: Vec<ListItem> = detail
        .runs
        .iter()
        .skip(app.detail_runs_scroll)
        .map(|run| {
            let outcome = run.outcome.map(|o| o.as_str()).unwrap_or("active");
            let dur = run_duration(app, run);
            ListItem::new(Line::from(format!(
                "#{} {} · {} · {}",
                run.id, run.harness, outcome, dur
            )))
        })
        .collect();
    let runs = if detail.runs.is_empty() {
        vec![ListItem::new(Span::styled(
            "(no runs)",
            Style::default().fg(Color::Gray),
        ))]
    } else {
        runs
    };
    let runs_active = app.detail_scroll_target == DetailScrollTarget::Runs;
    let runs_total = detail.runs.len();
    let runs_visible = layout.runs.height.saturating_sub(1) as usize;
    let runs_title = detail_section_title("runs", runs_total, app.detail_runs_scroll, runs_visible);
    f.render_widget(
        List::new(runs).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if runs_active {
                    Color::Blue
                } else {
                    Color::Gray
                }))
                .title(runs_title),
        ),
        layout.runs,
    );
}

fn run_duration(app: &App, run: &board_core::model::Run) -> String {
    let start = run.started_at.as_deref().and_then(parse_epoch);
    let end = run.ended_at.as_deref().and_then(parse_epoch);
    match (start, end) {
        (Some(s), Some(e)) => format_duration(Some((e - s).max(0))),
        (Some(s), None) => format_duration(Some((app.now - s).max(0))),
        _ => "-".to_string(),
    }
}
