use board_core::engine::format_duration;
use board_core::protocol::CardDetail;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailScrollTarget};

use super::{
    main_area, parse_epoch, sheet_area, status_glyph, status_label, truncate, NARROW_DETAIL_WIDTH,
};

// -- detail ------------------------------------------------------------------

fn detail_panel_area(app: &App, area: Rect) -> Rect {
    if app.detail_fullscreen {
        main_area(area)
    } else {
        sheet_area(app.layout_mode(), 120, 30, area)
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

/// Greedy word-wrap row count for a single comment string `"[author] body"`,
/// approximating ratatui `Wrap { trim: false }`: each rendered line holds as
/// many space-separated words as fit in `width` (by `chars().count()`), an
/// over-long word is hard-broken, and a blank source line still occupies one
/// row. Used for scroll clamping and section sizing so the scroll offset never
/// runs past the real rendered content. Also reused by `layout` to size
/// Compact board cards (1 vs. 2 title rows).
pub(super) fn wrapped_row_count(text: &str, width: u16) -> usize {
    let width = (width as usize).max(1);
    text.lines()
        .map(|line| {
            if line.is_empty() {
                return 1;
            }
            let mut rows = 1;
            let mut col = 0usize;
            let mut start_of_line = true;
            for word in line.split(' ') {
                let wl = word.chars().count();
                let sep = if start_of_line { 0 } else { 1 };
                if col + sep + wl <= width {
                    col += sep + wl;
                } else {
                    rows += 1;
                    col = wl.min(width);
                    if wl > width {
                        // Hard-break an over-long word across further rows.
                        rows += (wl - width) / width;
                        col = wl % width;
                    }
                }
                start_of_line = false;
            }
            rows
        })
        .sum::<usize>()
        .max(1)
}

/// Per-comment `(start_row, row_count)` in the wrapped comments block, using
/// exactly the measurement the renderer draws: a 1-char focus gutter (`▸` on
/// the focused comment, a space otherwise — uniform width either way) then
/// `"[author] body"`. Exposed so the app layer can map a comment index to its
/// rows (scroll clamping, focus-follow) and mouse can map a clicked row back
/// to a comment index.
pub fn comment_row_spans(detail: &CardDetail, width: u16) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(detail.comments.len());
    let mut row = 0usize;
    for c in &detail.comments {
        let n = wrapped_row_count(&format!(" [{}] {}", c.author, c.body), width);
        out.push((row, n));
        row += n;
    }
    out
}

/// Total wrapped rows the comment bodies occupy at `width`, one block per
/// comment (the gutter + `"[author] body"`). Exposed so the app layer can
/// clamp comment scrolling (row-based) to the real rendered height.
pub fn comment_wrapped_rows(detail: &CardDetail, width: u16) -> usize {
    comment_row_spans(detail, width)
        .iter()
        .map(|&(_, n)| n)
        .sum::<usize>()
        .max(1)
}

/// Size sections by content. Surplus height stays outside their borders; when
/// content exceeds the viewport, rows go to the greatest unmet demand first.
///
/// `comments_active` accounts for the action bar's row in the comments
/// section's demand (`needs[1]`) when it would render (focused + non-empty),
/// so the section can grow to fit it rather than clip.
fn detail_section_heights(
    detail: &CardDetail,
    width: u16,
    available: u16,
    comments_active: bool,
) -> ([u16; 3], u16) {
    let desc_lines = wrapped_line_count(&detail.card.description, width);
    // Comments word-wrap across multiple rows; size the section by the summed
    // wrapped height so long bodies get the rows they need instead of clipping.
    let comment_lines = comment_wrapped_rows(detail, width) as u16;
    let run_lines = (detail.runs.len() as u16).max(1);
    let bar_row = if comments_active && !detail.comments.is_empty() {
        1
    } else {
        0
    };
    // One additional row for each section's titled divider.
    let needs = [desc_lines + 1, comment_lines + 1 + bar_row, run_lines + 1];

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
    let comments_active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let (section_h, spacer_h) = detail_section_heights(
        detail,
        inner.width.saturating_sub(1),
        content_budget,
        comments_active,
    );
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

/// Whether the `[Edit] [Del] [Hist]` action bar renders on the comments
/// section's last row: only when comments are focused, there is at least one
/// comment to act on, and the section is tall enough to spare a row for it.
pub fn comments_action_bar_shown(app: &App, layout: &DetailLayout) -> bool {
    let Some(detail) = &app.detail else {
        return false;
    };
    app.detail_scroll_target == DetailScrollTarget::Comments
        && !detail.comments.is_empty()
        && layout.comments.height >= 3
}

/// The comments section's content viewport (below its title row, above the
/// action bar row when shown) plus the visible row count used for scroll
/// clamping. Shared by the renderer, `App::scroll_detail`,
/// `App::scroll_detail_to_latest`, and `App::follow_comment_focus` so the
/// bar's row-stealing arithmetic is computed in exactly one place.
pub fn comments_viewport(app: &App, layout: &DetailLayout) -> (Rect, usize) {
    let bar_row = comments_action_bar_shown(app, layout) as u16;
    let visible = layout.comments.height.saturating_sub(1 + bar_row);
    let rect = Rect::new(
        layout.comments.x,
        layout.comments.y + 1,
        layout.comments.width,
        visible,
    );
    (rect, visible as usize)
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

    let comments_active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let comments_content_w = layout.comments.width;
    let (viewport, comments_visible) = comments_viewport(app, &layout);
    let show_bar = comments_action_bar_shown(app, &layout);
    // The action bar (when shown) claims the section's last row; render the
    // comments `Paragraph` into a rect one row shorter so its own content
    // never overlaps the bar.
    let bar_rows: u16 = show_bar as u16;
    let paragraph_rect = Rect::new(
        layout.comments.x,
        layout.comments.y,
        layout.comments.width,
        layout.comments.height.saturating_sub(bar_rows),
    );
    let comments_widget = if detail.comments.is_empty() {
        Paragraph::new(Text::from(Line::from(Span::styled(
            "(no comments)",
            Style::default().fg(Color::Gray),
        ))))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if comments_active {
                    Color::Blue
                } else {
                    Color::Gray
                }))
                .title("comments"),
        )
    } else {
        // Render every comment as one `Line` carrying a 1-char focus gutter
        // (`▸` on the focused comment, a space otherwise), a styled
        // `[author] ` prefix, and the raw body, then let the wrapped
        // `Paragraph` word-break the combined line at the panel border.
        // ratatui keeps the gutter+author prefix on the first rendered row
        // and wraps the body onto later rows, so each comment reads as one
        // styled block.
        let sel = comments_active.then(|| app.detail_comment_sel.min(detail.comments.len() - 1));
        let lines: Vec<Line> = detail
            .comments
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let focused = sel == Some(i);
                let gutter_style = if focused {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let body_style = if focused {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(if focused { "▸" } else { " " }, gutter_style),
                    Span::styled(
                        format!("[{}] ", c.author),
                        Style::default().fg(Color::LightCyan),
                    ),
                    Span::styled(c.body.clone(), body_style),
                ])
            })
            .collect();
        let comments_total = comment_wrapped_rows(detail, comments_content_w);
        let comments_title = detail_section_title(
            "comments",
            comments_total,
            app.detail_comments_scroll,
            comments_visible,
        );
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_comments_scroll as u16, 0))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(if comments_active {
                        Color::Blue
                    } else {
                        Color::Gray
                    }))
                    .title(comments_title),
            )
    };
    f.render_widget(comments_widget, paragraph_rect);

    if !detail.comments.is_empty() {
        let spans = comment_row_spans(detail, comments_content_w);
        let mut hit_map = app.hit_map.borrow_mut();
        for (i, &(start, len)) in spans.iter().enumerate() {
            let row_lo = start.max(app.detail_comments_scroll);
            let row_hi = (start + len).min(app.detail_comments_scroll + comments_visible);
            if row_hi <= row_lo {
                continue;
            }
            let y = viewport.y + (row_lo - app.detail_comments_scroll) as u16;
            let h = (row_hi - row_lo) as u16;
            hit_map.push(
                Rect::new(viewport.x, y, viewport.width, h),
                crate::widgets::Zone::CommentRow(i),
            );
        }
        if show_bar {
            let bar_rect = Rect::new(
                layout.comments.x,
                layout.comments.y + layout.comments.height.saturating_sub(1),
                layout.comments.width,
                1,
            );
            drop(hit_map);
            draw_comment_action_bar(app, f, bar_rect);
        }
    }

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

/// The comments section's `[Edit] [Del] [Hist]` action bar, drawn on the
/// section's last row when `comments_action_bar_shown` allows it. `[Edit]`
/// and `[Del]` render dimmed (but still tappable — the tap routes to the same
/// "system comments are immutable" toast as the `e`/`d` keys, never a dead
/// zone) when the focused comment is a system comment; `[Hist]` is unaffected
/// since history stays available for system comments.
fn draw_comment_action_bar(app: &App, f: &mut Frame, area: Rect) {
    let immutable = app.focused_comment_is_system();
    let mut hit_map = app.hit_map.borrow_mut();
    let labels: [(&str, crate::widgets::Zone, bool); 3] = [
        ("[Edit]", crate::widgets::Zone::CommentEdit, immutable),
        ("[Del]", crate::widgets::Zone::CommentDelete, immutable),
        ("[Hist]", crate::widgets::Zone::CommentHistory, false),
    ];
    let mut x = area.x;
    for (label, zone, dimmed) in labels {
        let w = label.chars().count() as u16;
        if x.saturating_add(w) > area.x + area.width {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        let style = if dimmed {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
        f.render_widget(Paragraph::new(Span::styled(label, style)), rect);
        hit_map.push(rect, zone);
        x = x.saturating_add(w).saturating_add(1);
    }
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
