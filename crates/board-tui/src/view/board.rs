use board_core::engine::format_duration;
use board_core::model::Card;
use board_core::protocol::CardStatus;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::app::{App, CardFilter, Screen, SwitcherLevel};
use crate::widgets::{render_sheet_frame, Zone};

use super::{
    board_layout, centered_rect_abs, main_area, parse_epoch, sheet_area, status_glyph, truncate,
    CompactHeader, LayoutMode,
};

// -- board -------------------------------------------------------------------

pub(super) fn draw_board(app: &App, f: &mut Frame, area: Rect) {
    let layout = board_layout(app, area);
    let focused = app.screen == Screen::Board;
    let compact = app.layout_mode() == LayoutMode::Compact;

    if let Some(header) = &layout.compact_header {
        draw_compact_header(app, f, header);
    }

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
        if !compact {
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
        }

        for (ci, r) in &col.cards {
            let card = app.cards_of(column.id)[*ci];
            let selected = is_sel_col && *ci == app.sel_card && focused;
            draw_card(app, f, card, *r, selected, compact);
        }

        if let Some(sb_rect) = col.scrollbar_rect {
            let mut state = ScrollbarState::new(col.scroll.total.max(1))
                .position(col.scroll.offset)
                .viewport_content_length(col.scroll.visible.max(1));
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .track_symbol(Some("│"))
                .thumb_symbol("█");
            f.render_stateful_widget(scrollbar, sb_rect, &mut state);
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

/// The Compact-mode 2-row header: `‹    [ ⇄ Name  n/n ]    ›`.
fn draw_compact_header(app: &App, f: &mut Frame, header: &CompactHeader) {
    let mut hit_map = app.hit_map.borrow_mut();

    f.render_widget(
        Paragraph::new(Span::styled(
            " ‹ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        header.prev,
    );
    hit_map.push(header.prev, Zone::HeaderPrev);

    let n = app.board.columns.len();
    let column = app.board.columns.get(app.sel_col);
    let label = match column {
        Some(c) => format!("[ ⇄ {}  {}/{} ]", c.name, app.sel_col + 1, n.max(1)),
        None => "[ ⇄ no columns ]".to_string(),
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        header.switch,
    );
    hit_map.push(header.switch, Zone::HeaderSwitch);

    f.render_widget(
        Paragraph::new(Span::styled(
            " › ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        header.next,
    );
    hit_map.push(header.next, Zone::HeaderNext);
}

fn draw_card(app: &App, f: &mut Frame, card: &Card, r: Rect, selected: bool, compact: bool) {
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
    let title_line = Line::from(vec![
        Span::styled("▌", Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(
            if compact {
                card.title.clone()
            } else {
                truncate(&card.title, title_width)
            },
            title_style,
        ),
    ])
    .style(base);

    if compact {
        // Compact cards get 4 rows: up to 2 wrapped title lines + status line.
        let title_area = Rect::new(r.x, r.y, r.width, r.height.saturating_sub(1).max(1));
        let p = Paragraph::new(Text::from(vec![title_line]))
            .wrap(Wrap { trim: false })
            .style(base);
        f.render_widget(p, title_area);
        let status_area = Rect::new(r.x, r.y + r.height.saturating_sub(1), r.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(status_spans)).style(base),
            status_area,
        );
    } else {
        let lines = vec![title_line, Line::from(status_spans).style(base)];
        let p = Paragraph::new(Text::from(lines)).style(base);
        f.render_widget(p, r);
    }
}

// -- Compact-only two-level switcher sheet ------------------------------------

pub(super) fn draw_switcher(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.switcher else { return };
    let mode = app.layout_mode();
    let sheet = sheet_area(mode, 44, 14, area);
    f.render_widget(Clear, sheet);

    let (full_title, compact_title) = match state.level {
        SwitcherLevel::Columns => ("Columns (j/k move · Enter open · Esc close)", "Columns"),
        SwitcherLevel::Boards if state.entered_at_boards => {
            ("Boards (Enter switch · Esc close)", "Boards")
        }
        SwitcherLevel::Boards => ("Boards (Enter switch · Esc back)", "Boards"),
    };
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_sheet_frame(
        f,
        sheet,
        mode == LayoutMode::Compact,
        full_title,
        compact_title,
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );
    let items: Vec<ListItem> = match state.level {
        SwitcherLevel::Columns => {
            let mut rows: Vec<ListItem> = app
                .board
                .columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let count = app.cards_of(c.id).len();
                    let style = if i == state.sel {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Span::styled(format!(" {}  {} ", c.name, count), style))
                })
                .collect();
            let trailing_idx = app.board.columns.len();
            let trailing_style = if state.sel == trailing_idx {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Cyan)
            };
            rows.push(ListItem::new(Span::styled(
                " ⇄  Switch board  → ",
                trailing_style,
            )));
            rows
        }
        SwitcherLevel::Boards => state
            .boards
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let style = if i == state.sel {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(format!(" {} ", label), style))
            })
            .collect(),
    };
    let row_count = items.len().max(1) as u16;
    let list = List::new(items);
    f.render_widget(list, inner);

    // Register clickable rows.
    for row in 0..row_count.min(inner.height) {
        let rect = Rect::new(inner.x, inner.y + row, inner.width, 1);
        let zone = match state.level {
            SwitcherLevel::Columns if (row as usize) == app.board.columns.len() => {
                Zone::SwitcherSwitchBoard
            }
            _ => Zone::SwitcherRow(row as usize),
        };
        hit_map.push(rect, zone);
    }
}
