use board_core::engine::{format_duration, run_elapsed};
use board_core::protocol::{parse_timestamp, CardDetail};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailScrollTarget};

use super::{main_area, sheet_area, status_glyph, status_label, truncate, NARROW_DETAIL_WIDTH};

// -- detail ------------------------------------------------------------------

fn detail_panel_area(app: &App, area: Rect) -> Rect {
    if app.detail_fullscreen {
        main_area(area)
    } else {
        sheet_area(app.layout_mode(), 120, 30, area)
    }
}

fn detail_control_labels(panel_width: u16, fullscreen: bool) -> (&'static str, &'static str) {
    if panel_width < 32 {
        ("[×]", if fullscreen { "[Pop]" } else { "[Full]" })
    } else {
        (
            "[Close]",
            if fullscreen {
                "[Popup]"
            } else {
                "[Fullscreen]"
            },
        )
    }
}

/// Click target for the popup/fullscreen action rendered in the detail title.
pub fn detail_toggle_rect(app: &App, area: Rect) -> Rect {
    let panel = detail_panel_area(app, area);
    let (_, toggle_label) = detail_control_labels(panel.width, app.detail_fullscreen);
    let label_w = toggle_label.chars().count() as u16;
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
    let comment_lines = comment_wrapped_rows(detail, width) as u16;
    let run_lines = (detail.runs.len() as u16).max(1);
    let bar_row = if comments_active && !detail.comments.is_empty() {
        1
    } else {
        0
    };
    // Full-card sections: title row + bottom border (+ in-card action bar for
    // the histories), so each needs 4 rows to expose one content row.
    let needs = [desc_lines + 2, comment_lines + 2 + bar_row, run_lines + 2];

    // Floor: keep at least one content row for Runs (the primary read action)
    // whenever 4 rows fit; Comments gets the same floor once there is room.
    // Description collapses to a hint on very short screens.
    let mut heights = [1u16; 3];
    if available >= 4 {
        heights[2] = heights[2].max(4);
    }
    if available >= 9 {
        heights[1] = heights[1].max(4);
    }
    // Never exceed the available budget: shed Desc first, then Comments, then
    // Runs, so the Layout constraints cannot overflow the inner area on tiny
    // terminals. Bounded loop: each pass removes exactly one row (down to 0),
    // so it always terminates even when `available` is 0.
    for _ in 0..128 {
        if heights.iter().copied().sum::<u16>() <= available {
            break;
        }
        if heights[0] > 0 {
            heights[0] -= 1;
        } else if heights[1] > 0 {
            heights[1] -= 1;
        } else {
            heights[2] -= 1;
        }
    }
    let mut remaining = available.saturating_sub(heights.iter().copied().sum::<u16>());
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
    pub configuration: Rect,
    pub session: Rect,
    pub description: Rect,
    pub card_actions: Rect,
    pub comments: Rect,
    pub comment_actions: Rect,
    pub runs: Rect,
    pub run_actions: Rect,
}

impl DetailLayout {
    fn empty(panel: Rect, inner: Rect) -> Self {
        Self {
            panel,
            status: inner,
            configuration: Rect::default(),
            session: Rect::default(),
            description: Rect::default(),
            card_actions: Rect::default(),
            comments: Rect::default(),
            comment_actions: Rect::default(),
            runs: Rect::default(),
            run_actions: Rect::default(),
        }
    }
}

/// Responsive detail geometry. Wide terminals use the approved three-pane
/// hierarchy; Regular and Compact retain one vertical reading order.
pub fn detail_layout(app: &App, area: Rect) -> DetailLayout {
    let panel = detail_panel_area(app, area);
    let inner = Block::default().borders(Borders::ALL).inner(panel);
    let Some(detail) = &app.detail else {
        return DetailLayout::empty(panel, inner);
    };

    if app.layout_mode() == super::LayoutMode::Wide {
        let usable = inner.width.saturating_sub(2);
        let left_w = usable.saturating_mul(35) / 100;
        let center_w = usable.saturating_mul(40) / 100;
        let right_w = usable.saturating_sub(left_w).saturating_sub(center_w);
        let columns = [
            Rect::new(inner.x, inner.y, left_w, inner.height),
            Rect::new(
                inner.x.saturating_add(left_w).saturating_add(1),
                inner.y,
                center_w,
                inner.height,
            ),
            Rect::new(
                inner
                    .x
                    .saturating_add(left_w)
                    .saturating_add(center_w)
                    .saturating_add(2),
                inner.y,
                right_w,
                inner.height,
            ),
        ];
        let left = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(columns[0]);
        // Comment / run histories are single full-height cards; the action
        // bar lives on their last inner row (in-card buttons).
        let runs = columns[1];
        let comments = columns[2];
        let run_actions = Rect::new(runs.x, runs.bottom().saturating_sub(2), runs.width, 1);
        let comment_actions = Rect::new(
            comments.x,
            comments.bottom().saturating_sub(2),
            comments.width,
            1,
        );
        return DetailLayout {
            panel,
            status: left[0],
            configuration: left[1],
            session: left[2],
            description: left[3],
            card_actions: left[4],
            comments,
            comment_actions,
            runs,
            run_actions,
        };
    }

    // Eight rows hold real metadata, three rows hold contextual controls. The
    // remaining budget is shared by Description, Comments, and Runs, each with
    // a non-zero content row at the 40x20 acceptance size.
    let section_budget = inner.height.saturating_sub(10);
    let comments_active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let (section_h, spacer) = detail_section_heights(
        detail,
        inner.width.saturating_sub(1),
        section_budget,
        comments_active,
    );
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(section_h[0].saturating_add(spacer)),
        Constraint::Length(1),
        Constraint::Length(section_h[1]),
        Constraint::Length(section_h[2]),
    ])
    .split(inner);
    let comments = chunks[5];
    let runs = chunks[6];
    let comment_actions = Rect::new(
        comments.x,
        comments.bottom().saturating_sub(2),
        comments.width,
        1,
    );
    let run_actions = Rect::new(runs.x, runs.bottom().saturating_sub(2), runs.width, 1);
    DetailLayout {
        panel,
        status: chunks[0],
        configuration: chunks[1],
        session: chunks[2],
        description: chunks[3],
        card_actions: chunks[4],
        comments,
        comment_actions,
        runs,
        run_actions,
    }
}

/// Selected-comment actions are meaningful only while Comments owns focus.
pub fn comments_action_bar_shown(app: &App, layout: &DetailLayout) -> bool {
    app.detail.as_ref().is_some_and(|detail| {
        app.detail_scroll_target == DetailScrollTarget::Comments
            && !detail.comments.is_empty()
            && layout.comments.height >= 3
            && !layout.comment_actions.is_empty()
    })
}

/// The comments viewport below its divider. The contextual bar has its own
/// rectangle and therefore never steals or overlaps a history row.
pub fn comments_viewport(_app: &App, layout: &DetailLayout) -> (Rect, usize) {
    // title row + in-card action bar + bottom border => three reserved rows
    let visible = layout.comments.height.saturating_sub(3);
    (
        Rect::new(
            layout.comments.x,
            layout.comments.y.saturating_add(1),
            layout.comments.width,
            visible,
        ),
        visible as usize,
    )
}

/// Visible run rows inside the runs card: title row + bottom border + the
/// in-card action bar are reserved, leaving the rest for rows.
pub fn runs_viewport_height(layout: &DetailLayout) -> usize {
    layout.runs.height.saturating_sub(3) as usize
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

fn section_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    let style = if focused {
        Color::LightBlue
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(style))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ))
}

fn metadata_line(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

/// Click target for the explicit close action in the detail title.
pub fn detail_close_rect(app: &App, area: Rect) -> Rect {
    let panel = detail_panel_area(app, area);
    let (close_label, _) = detail_control_labels(panel.width, app.detail_fullscreen);
    let width = close_label.chars().count() as u16;
    let toggle = detail_toggle_rect(app, area);
    Rect::new(toggle.x.saturating_sub(width + 1), panel.y, width, 1)
}

pub(super) fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    use crate::widgets::{ActionButton, ActionStrip, ActionTone, UiAction, Zone};

    let Some(detail) = &app.detail else { return };
    let layout = detail_layout(app, area);
    let panel = layout.panel;
    let card = &detail.card;
    f.render_widget(Clear, panel);

    let (close_label, toggle_label) = detail_control_labels(panel.width, app.detail_fullscreen);
    let controls = format!("{close_label} {toggle_label}");
    let title_width = panel.width.saturating_sub(2) as usize;
    let title_limit = if panel.width < NARROW_DETAIL_WIDTH {
        32
    } else {
        48
    };
    let left = format!(
        " Card #{}: {} ",
        card.id,
        truncate(&card.title, title_limit)
    );
    let left = truncate(&left, title_width.saturating_sub(controls.len() + 1));
    let gap = title_width.saturating_sub(left.chars().count() + controls.len());
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::LightBlue))
            .title(format!("{}{}{}", left, " ".repeat(gap), controls)),
        panel,
    );
    {
        let mut hit_map = app.hit_map.borrow_mut();
        hit_map.push(
            detail_close_rect(app, area),
            Zone::Action(UiAction::CloseDetail),
        );
        hit_map.push(
            detail_toggle_rect(app, area),
            Zone::Action(UiAction::ToggleDetail),
        );
    }

    let (glyph, color) = status_glyph(card.status);
    let mut status = format!("{glyph} {}", status_label(card));
    if card.archived_at.is_some() {
        status.push_str(" · ARCHIVED");
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            status,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
        .block(section_block("Status", false)),
        layout.status,
    );

    let model = card.model.clone().unwrap_or_else(|| "default".into());
    let effort = card
        .effort
        .map(|v| v.as_str().to_owned())
        .unwrap_or_else(|| "default".into());
    let permission = card
        .permission_mode
        .clone()
        .unwrap_or_else(|| "default".into());
    f.render_widget(
        Paragraph::new(metadata_line(
            "Harness · Model: ",
            format!(
                "{} · {} · effort {} · perm {}",
                card.harness, model, effort, permission
            ),
        ))
        .block(section_block("Task Configuration", false)),
        layout.configuration,
    );

    let session = card.session.clone().unwrap_or_else(|| "default".into());
    let space = format!(
        "{}:{}",
        card.space_kind.as_str(),
        card.space_ref.as_deref().unwrap_or("-")
    );
    f.render_widget(
        Paragraph::new(metadata_line(
            "Herdr session: ",
            format!("{session} · Space: {space}"),
        ))
        .block(section_block("Session", false)),
        layout.session,
    );

    f.render_widget(
        Paragraph::new(card.description.as_str())
            .wrap(Wrap { trim: false })
            .block(section_block("Description", false)),
        layout.description,
    );

    let mut card_buttons = vec![
        ActionButton {
            label: "Edit card",
            compact_label: "Edit",
            action: UiAction::EditCard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: if card.archived_at.is_some() {
                "Restore card"
            } else {
                "Archive card"
            },
            compact_label: if card.archived_at.is_some() {
                "Restore"
            } else {
                "Archive"
            },
            action: UiAction::ArchiveCard,
            tone: ActionTone::Destructive,
        },
    ];
    if card.status == board_core::protocol::CardStatus::Awaiting {
        card_buttons.push(ActionButton {
            label: "Confirm done",
            compact_label: "Confirm",
            action: UiAction::ConfirmAwaiting,
            tone: ActionTone::Primary,
        });
    }
    ActionStrip {
        buttons: &card_buttons,
    }
    .render(f, layout.card_actions, &mut app.hit_map.borrow_mut());

    draw_comments(app, f, detail, &layout);
    draw_runs(app, f, detail, &layout);

    draw_comment_actions(app, f, layout.comment_actions);

    let run_buttons = [
        ActionButton {
            label: "Open pane",
            compact_label: "Open",
            action: UiAction::FocusRunPane,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Retry run",
            compact_label: "Retry",
            action: UiAction::RetryRun,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Cancel run",
            compact_label: "Cancel",
            action: UiAction::CancelRun,
            tone: ActionTone::Destructive,
        },
    ];
    ActionStrip {
        buttons: &run_buttons,
    }
    .render(f, layout.run_actions, &mut app.hit_map.borrow_mut());
}

fn draw_comments(app: &App, f: &mut Frame, detail: &CardDetail, layout: &DetailLayout) {
    let active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let (viewport, visible) = comments_viewport(app, layout);
    let total = comment_wrapped_rows(detail, layout.comments.width);
    let title = detail_section_title("Comments", total, app.detail_comments_scroll, visible);
    if detail.comments.is_empty() {
        f.render_widget(
            Paragraph::new("(no comments)")
                .style(Style::default().fg(Color::Gray))
                .block(section_block(&title, active)),
            layout.comments,
        );
        return;
    }
    let sel = active.then(|| app.detail_comment_sel.min(detail.comments.len() - 1));
    let lines = detail
        .comments
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (gutter, style) = focus_row_marker(sel == Some(i));
            Line::from(vec![
                gutter,
                Span::styled(
                    format!("[{}] ", c.author),
                    Style::default().fg(Color::LightCyan),
                ),
                Span::styled(c.body.clone(), style),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_comments_scroll as u16, 0))
            .block(section_block(&title, active)),
        layout.comments,
    );
    let spans = comment_row_spans(detail, layout.comments.width);
    let mut hit_map = app.hit_map.borrow_mut();
    for (i, &(start, len)) in spans.iter().enumerate() {
        let lo = start.max(app.detail_comments_scroll);
        let hi = (start + len).min(app.detail_comments_scroll + visible);
        if hi > lo {
            hit_map.push(
                Rect::new(
                    viewport.x,
                    viewport.y + (lo - app.detail_comments_scroll) as u16,
                    viewport.width,
                    (hi - lo) as u16,
                ),
                crate::widgets::Zone::CommentRow(i),
            );
        }
    }
}

fn draw_comment_actions(app: &App, f: &mut Frame, area: Rect) {
    use crate::widgets::{UiAction, Zone};
    if area.is_empty() {
        return;
    }
    let selected_actions = comments_action_bar_shown(app, &detail_layout(app, app.last_area));
    let count = if selected_actions { 4 } else { 1 };
    let rects = Layout::horizontal(
        (0..count)
            .map(|_| Constraint::Ratio(1, count as u32))
            .collect::<Vec<_>>(),
    )
    .split(area);
    let labels = [
        ("Add comment", "Add"),
        ("Edit comment", "Edit"),
        ("Delete comment", "Delete"),
        ("History", "History"),
    ];
    let immutable = app.focused_comment_is_system();
    let mut hit_map = app.hit_map.borrow_mut();
    for (idx, rect) in rects.iter().copied().enumerate() {
        let (full, compact) = labels[idx];
        let label = if rect.width < full.chars().count() as u16 + 1 {
            compact
        } else {
            full
        };
        let dimmed = immutable && matches!(idx, 1 | 2);
        let style = if dimmed {
            Style::default().fg(Color::DarkGray)
        } else if idx == 0 {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else if idx == 2 {
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        f.render_widget(
            Paragraph::new(label)
                .alignment(ratatui::layout::Alignment::Center)
                .style(style),
            rect,
        );
        let zone = match idx {
            0 => Zone::Action(UiAction::AddComment),
            1 => Zone::CommentEdit,
            2 => Zone::CommentDelete,
            3 => Zone::CommentHistory,
            _ => unreachable!(),
        };
        hit_map.push(rect, zone);
    }
}

fn draw_runs(app: &App, f: &mut Frame, detail: &CardDetail, layout: &DetailLayout) {
    let active = app.detail_scroll_target == DetailScrollTarget::Runs;
    let selected = (!detail.runs.is_empty()).then(|| app.detail_run_sel.min(detail.runs.len() - 1));
    let visible = layout.runs.height.saturating_sub(3) as usize;
    let title = detail_section_title("Runs", detail.runs.len(), app.detail_runs_scroll, visible);
    let width = (layout.runs.width as usize).saturating_sub(1);
    let items = if detail.runs.is_empty() {
        vec![ListItem::new(Span::styled(
            "(no runs)",
            Style::default().fg(Color::Gray),
        ))]
    } else {
        detail
            .runs
            .iter()
            .enumerate()
            .skip(app.detail_runs_scroll)
            .take(visible)
            .map(|(i, run)| {
                let (gutter, style) = focus_row_marker(active && selected == Some(i));
                ListItem::new(Line::from(vec![
                    gutter,
                    Span::styled(truncate(&run_row_text(app, run), width), style),
                ]))
            })
            .collect()
    };
    f.render_widget(
        List::new(items).block(section_block(&title, active)),
        layout.runs,
    );
    if !detail.runs.is_empty() {
        let mut hit_map = app.hit_map.borrow_mut();
        for (row, idx) in (app.detail_runs_scroll..detail.runs.len())
            .take(visible)
            .enumerate()
        {
            hit_map.push(
                Rect::new(
                    layout.runs.x,
                    layout.runs.y + 1 + row as u16,
                    layout.runs.width,
                    1,
                ),
                crate::widgets::Zone::RunRow(idx),
            );
        }
    }
}

/// The 1-char focus gutter (`▸` on the focused/selected row) plus the body
/// style that goes with it. The single definition shared by the comments list
/// and the runs list, so both mark their cursor identically.
///
/// The arrow is **bright** blue (`LightBlue`, the terminal's intense blue),
/// matching the blue used for the focused section's divider and the status
/// labels. Plain `Color::Blue` would be the dark navy of the 256-colour palette
/// and would lose contrast against a dark terminal background, which is the
/// opposite of the point.
fn focus_row_marker(focused: bool) -> (Span<'static>, Style) {
    if focused {
        (
            Span::styled(
                "▸",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )
    } else {
        (Span::raw(" "), Style::default())
    }
}

/// One run row, deliberately minimal: the **run number**, the **harness**, the
/// **status** (`outcome`, or `active` while the run is still open) and **how
/// long it ran** — or has been running, since `run_duration` measures an open
/// run against `app.now`.
///
/// Nothing else. The column is already implied by the card, the harness
/// **conversation id** and the herdr **session name** are carried by the
/// detail's status fields (never in the same slot as each other — the confusion
/// `run.focus`'s separate `session` / `session_id` fields exist to prevent),
/// and a `pane ✓|-` marker would now be actively misleading: a run whose pane
/// is gone is reopened by resuming its conversation, so a missing pane no longer
/// predicts whether `o` works.
fn run_row_text(app: &App, run: &board_core::model::Run) -> String {
    let outcome = run.outcome.map(|o| o.as_str()).unwrap_or("active");
    format!(
        "#{} {} · {} · {}",
        run.id,
        run.harness,
        outcome,
        run_duration(app, run)
    )
}

/// Formatting only: `run_elapsed` owns "open runs measure against `now`,
/// closed runs against their own end, out-of-order clamps to 0", and a run
/// that never started has no duration to show.
fn run_duration(app: &App, run: &board_core::model::Run) -> String {
    let started = run.started_at.as_deref().and_then(parse_timestamp);
    let ended = run.ended_at.as_deref().and_then(parse_timestamp);
    match run_elapsed(started, ended, app.now) {
        Some(secs) => format_duration(Some(secs)),
        None => "-".to_string(),
    }
}
