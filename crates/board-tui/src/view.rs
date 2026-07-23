//! Rendering: the pure `view(&App, &mut Frame)` plus the board layout used for
//! both drawing and mouse hit-testing. No clocks are read here — timers use the
//! injected `app.now` so snapshots are deterministic.

use board_core::engine::format_duration;
use board_core::model::{Board, Card};
use board_core::protocol::{AwaitingReason, CardDetail, CardStatus};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, CardFilter, DetailScrollTarget, Screen};
use crate::forms::{FieldKind, Form};

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

// -- layout / hit-testing ----------------------------------------------------

pub struct ColLayout {
    pub idx: usize,
    pub rect: Rect,
    pub cards: Vec<(usize, Rect)>,
}

pub struct BoardLayout {
    pub cols: Vec<ColLayout>,
}

impl BoardLayout {
    pub fn hit_card(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        for c in &self.cols {
            for (ci, r) in &c.cards {
                if x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
                    return Some((c.idx, *ci));
                }
            }
        }
        None
    }
    pub fn hit_header(&self, x: u16, y: u16) -> Option<usize> {
        for c in &self.cols {
            if y == c.rect.y && x >= c.rect.x && x < c.rect.x + c.rect.width {
                return Some(c.idx);
            }
        }
        None
    }
    pub fn hit_any_column(&self, x: u16) -> Option<usize> {
        for c in &self.cols {
            if x >= c.rect.x && x < c.rect.x + c.rect.width {
                return Some(c.idx);
            }
        }
        None
    }
}

/// Region above the 1-row footer.
fn main_area(area: Rect) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1))
}

/// Compute visible-column and card geometry. Pure function of `app` + `area`,
/// so mouse handling can recompute the exact rects the last frame used.
pub fn board_layout(app: &App, area: Rect) -> BoardLayout {
    let main = main_area(area);
    let n = app.board.columns.len();
    let mut cols = Vec::new();
    if n == 0 || main.width == 0 {
        return BoardLayout { cols };
    }
    // Fill the entire viewport. Keep columns readable via a minimum width;
    // when every column fits, distribute all remaining cells across them.
    // When they do not all fit, the selected column drives a full-width window.
    let capacity = (main.width / MIN_COL_W).max(1) as usize;
    let visible = capacity.min(n);
    let start = app
        .sel_col
        .saturating_add(1)
        .saturating_sub(visible)
        .min(n.saturating_sub(visible));
    let base_w = main.width / visible as u16;
    let remainder = main.width % visible as u16;
    let mut x = main.x;
    for i in 0..visible {
        let idx = start + i;
        let w = base_w + u16::from((i as u16) < remainder);
        let rect = Rect::new(x, main.y, w, main.height);
        x = x.saturating_add(w);
        let col = &app.board.columns[idx];
        let cards = app.cards_of(col.id);
        let mut card_rects = Vec::new();
        let inner_y = rect.y + 1;
        let inner_h = rect.height.saturating_sub(2);
        for (ci, _) in cards.iter().enumerate() {
            let cy = inner_y + (ci as u16) * CARD_H;
            if cy >= inner_y + inner_h {
                break;
            }
            let h = CARD_H.min(inner_y + inner_h - cy);
            card_rects.push((
                ci,
                Rect::new(rect.x + 1, cy, rect.width.saturating_sub(2), h),
            ));
        }
        cols.push(ColLayout {
            idx,
            rect,
            cards: card_rects,
        });
    }
    BoardLayout { cols }
}

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
    draw_board(app, f, area);

    match app.screen {
        Screen::Board => {}
        Screen::CardDetail => draw_detail(app, f, area),
        Screen::CardForm | Screen::ColumnForm => {
            if let Some(form) = &app.form {
                draw_form(form, f, area);
            }
        }
        Screen::Picker => draw_picker(app, f, area),
        Screen::Confirm => draw_confirm(app, f, area),
        Screen::Help => draw_help(f, area),
    }

    draw_footer(app, f, area);
}

// -- board -------------------------------------------------------------------

fn draw_board(app: &App, f: &mut Frame, area: Rect) {
    let layout = board_layout(app, area);
    let focused = app.screen == Screen::Board;

    for col in &layout.cols {
        let column = &app.board.columns[col.idx];
        let is_sel_col = col.idx == app.sel_col;
        let hover = app
            .drag
            .as_ref()
            .map(|d| d.hover_col == col.idx)
            .unwrap_or(false);
        let border_style = if hover {
            Style::default().fg(Color::Magenta)
        } else if is_sel_col && focused {
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let card_count = app.cards_of(column.id).len();
        let title = format!(
            " {} · {} · {} ",
            column.name,
            card_count,
            column.trigger.as_str()
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);
        f.render_widget(block, col.rect);

        for (ci, r) in &col.cards {
            let card = app.cards_of(column.id)[*ci];
            let selected = is_sel_col && *ci == app.sel_card && focused;
            draw_card(app, f, card, *r, selected);
        }
    }

    let visible_cards = app
        .board
        .columns
        .iter()
        .map(|column| app.cards_of(column.id).len())
        .sum::<usize>();
    if app.is_empty_board() || visible_cards == 0 {
        let m = main_area(area);
        let (message, actions) = if app.is_empty_board() {
            ("Board is empty.", "N: new column  ·  T: apply template")
        } else {
            match app.card_filter {
                CardFilter::Active => ("No active cards.", "v: show all / archived"),
                CardFilter::All => ("No cards.", "n: new card"),
                CardFilter::Archived => ("No archived cards.", "v: change view"),
            }
        };
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                message,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(actions),
        ])
        .alignment(Alignment::Center);
        let box_area = centered_rect_abs(40, 5, m);
        f.render_widget(hint, box_area);
    }
}

fn draw_card(app: &App, f: &mut Frame, card: &Card, r: Rect, selected: bool) {
    let archived = card.archived_at.is_some();
    let (glyph, color) = if archived {
        ('▣', Color::DarkGray)
    } else {
        status_glyph(card.status)
    };
    // Selection gets its own background instead of REVERSED. This preserves
    // status foreground colors (especially idle) and avoids color inversion.
    let base = if selected {
        Style::default().fg(Color::White).bg(Color::Rgb(30, 41, 59))
    } else if archived {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let title_style = if selected {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    };

    let mut status_spans = if archived {
        vec![
            Span::raw("  "),
            Span::styled("▣ ARCHIVED", Style::default().fg(Color::DarkGray)),
        ]
    } else {
        vec![
            Span::raw("  "),
            Span::styled(glyph.to_string(), Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(card.status.as_str(), Style::default().fg(color)),
        ]
    };
    if !archived && card.status == CardStatus::Running {
        // Card updates (comments, moves, etc.) change `updated_at` while a run
        // remains open. Prefer the board-scoped active-run summary so the
        // timer measures execution time rather than unrelated card activity;
        // the card timestamp is a compatibility fallback for old snapshots.
        let start = app
            .active_run_for_card(card.id)
            .and_then(|run| parse_epoch(&run.started_at))
            .or_else(|| parse_epoch(&card.updated_at));
        let elapsed = start.map(|s| (app.now - s).max(0)).unwrap_or(0);
        status_spans.push(Span::raw(format!(" · {}", format_duration(Some(elapsed)))));
    }
    status_spans.push(Span::styled(
        format!(" · {}", card.harness),
        Style::default().fg(Color::Gray),
    ));
    if let Some(model) = &card.model {
        status_spans.push(Span::styled(
            format!("/{}", model),
            Style::default().fg(Color::Gray),
        ));
    }

    let title_width = r.width.saturating_sub(2) as usize;
    let lines = vec![
        Line::from(vec![
            Span::styled("▌", Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(truncate(&card.title, title_width), title_style),
        ])
        .style(base),
        Line::from(status_spans).style(base),
    ];
    let p = Paragraph::new(Text::from(lines)).style(base);
    f.render_widget(p, r);
}

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

fn detail_section_title(name: &str, total: usize, offset: usize, visible: usize) -> String {
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

fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
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

// -- form --------------------------------------------------------------------

fn draw_form(form: &Form, f: &mut Frame, area: Rect) {
    // Content-sized on large terminals, while still shrinking to small ones.
    let visible: Vec<usize> = (0..form.fields.len())
        .filter(|i| form.field_visible(*i))
        .collect();
    let content_h = visible
        .iter()
        .map(|i| if form.fields[*i].multiline { 4 } else { 2 })
        .sum::<u16>()
        .saturating_add(2);
    let box_area = centered_rect_abs(96, content_h, area);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(format!(
            " {} (Tab: field · Enter: save · Esc: cancel) ",
            form.title()
        ));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    // Build per-field rows; multiline fields get more height.
    let constraints: Vec<Constraint> = visible
        .iter()
        .map(|i| {
            if form.fields[*i].multiline {
                Constraint::Length(4)
            } else {
                Constraint::Length(2)
            }
        })
        .collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (row_idx, &fi) in visible.iter().enumerate() {
        let field = &form.fields[fi];
        let is_focus = fi == form.focus;
        let label_style = if is_focus {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let area = rows[row_idx];
        let value_text = match &field.kind {
            FieldKind::Choice { .. } => format!("< {} >", field.display()),
            FieldKind::Text(ta) => {
                let mut t = ta.lines().join("  ⏎  ");
                if is_focus {
                    t.push('▏');
                }
                t
            }
        };
        let hint = if field.multiline {
            "  (Ctrl+E: $EDITOR)"
        } else {
            ""
        };
        let mut lines = vec![Line::from(Span::styled(
            format!("{}{}", field.label, hint),
            label_style,
        ))];
        let val_style = if is_focus {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            truncate(&value_text, area.width as usize),
            val_style,
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), area);
    }
}

// -- picker / confirm / help / footer ---------------------------------------

fn draw_picker(app: &App, f: &mut Frame, area: Rect) {
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

fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
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

fn draw_help(f: &mut Frame, area: Rect) {
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

fn draw_footer(app: &App, f: &mut Frame, area: Rect) {
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
mod tests {
    use super::{
        board_picker_label, detail_section_title, pane_title, HELP_GUTTER_WIDTH, HELP_KEYS,
        HELP_KEY_WIDTH,
    };
    use crate::app::CardFilter;
    use board_core::model::Board;

    #[test]
    fn pane_titles_include_scope_filter_and_sanitize_long_labels() {
        let global = Board {
            id: 1,
            name: "Global".into(),
            scope_path: None,
        };
        assert_eq!(
            pane_title(&global, CardFilter::Active),
            "Board [Global · ACTIVE]"
        );

        let scoped = Board {
            id: 2,
            name: "/tmp/repo".into(),
            scope_path: Some("/tmp/a[unsafe]/abcdefghijklmnopqrstuvwxyz0123456789".into()),
        };
        let title = pane_title(&scoped, CardFilter::Archived);
        assert!(title.starts_with("Board [abcdefghijklmnopqrstuvwxyz01234"));
        assert!(title.ends_with("… · ARCHIVED]"));
        assert!(!title.contains('[') || title.starts_with("Board ["));
        assert_eq!(
            board_picker_label(&scoped),
            "abcdefghijklmnopqrstuvwxyz01234… — /tmp/a(unsafe)/abcdefghijklmnopqrstuvwxyz0123456789"
        );
    }

    #[test]
    fn detail_titles_show_only_overflow_arrows() {
        assert_eq!(detail_section_title("comments", 3, 0, 3), "comments");
        assert_eq!(detail_section_title("comments", 8, 0, 3), "comments ↓");
        assert_eq!(detail_section_title("comments", 8, 2, 3), "comments ↑↓");
        assert_eq!(detail_section_title("runs", 8, 5, 3), "runs ↑");
    }

    #[test]
    fn help_descriptions_fit_each_80_column_panel_column() {
        let inner_width = 80_u16 - 2;
        let column_width = (inner_width - HELP_GUTTER_WIDTH) / 2;
        let description_width = column_width - HELP_KEY_WIDTH;
        for (key, description) in HELP_KEYS {
            if *key != "--" {
                assert!(
                    description.chars().count() <= description_width as usize,
                    "{key} description does not fit: {description}"
                );
            }
        }
    }
}
