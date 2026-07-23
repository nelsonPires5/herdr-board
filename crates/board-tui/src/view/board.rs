use board_core::engine::format_duration;
use board_core::model::Card;
use board_core::protocol::CardStatus;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, CardFilter, Screen};

use super::{board_layout, centered_rect_abs, main_area, parse_epoch, status_glyph, truncate};

// -- board -------------------------------------------------------------------

pub(super) fn draw_board(app: &App, f: &mut Frame, area: Rect) {
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
