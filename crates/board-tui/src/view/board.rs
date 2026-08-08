use board_core::engine::{format_duration, run_elapsed};
use board_core::model::Card;
use board_core::protocol::{parse_timestamp, CardStatus};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, CardFilter, Screen, SwitcherLevel};
use crate::widgets::{
    button_text, render_button_chip_at, render_button_chip_at_with_modifier, render_sheet_frame,
    windowed_rows, ActionButton, ActionStrip, ActionTone, UiAction, Zone,
};

use super::{
    board_action_area, board_action_columns, board_body_area, board_header_area, board_layout,
    centered_rect_abs, compact_filter_label, compact_filter_options, compact_filter_rows,
    sheet_area, status_glyph, truncate, CompactHeader, LayoutMode,
};

// -- board -------------------------------------------------------------------

pub(super) fn draw_board(app: &App, f: &mut Frame, area: Rect) {
    let layout = board_layout(app, area);
    let focused = app.screen == Screen::Board;
    let compact = app.layout_mode() == LayoutMode::Compact;

    // The board title/running/scope chrome and bottom action row stay visible
    // behind every overlay. Sheets are restricted to the content region, so
    // both rails can be drawn unconditionally here.
    draw_board_header(app, f, area, layout.compact_header.as_ref());
    draw_board_actions(app, f, area);

    for col in &layout.cols {
        let Some(column) = app.display_column(col.idx) else {
            continue;
        };
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
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let card_count = app.cards_of(column.id).len();
        if !compact {
            let title = format!(
                " {} · {} · {} ",
                column.name.to_uppercase(),
                column.trigger.as_str().to_uppercase(),
                card_count,
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .style(Style::default().bg(Color::Rgb(3, 13, 22)))
                .title(Span::styled(
                    title,
                    border_style.add_modifier(Modifier::BOLD),
                ));
            f.render_widget(block, col.rect);
        }

        for (ci, r) in &col.cards {
            let card = app.cards_of(column.id)[*ci];
            let selected = is_sel_col && *ci == app.sel_card && focused;
            draw_card(app, f, card, *r, selected, compact);
        }

        if let Some(sb_rect) = col.scrollbar_rect {
            crate::widgets::vertical_scrollbar(
                f,
                sb_rect,
                col.scroll.total,
                col.scroll.offset,
                col.scroll.visible,
            );
        }
    }

    let visible_cards = app
        .board
        .columns
        .iter()
        .map(|column| app.cards_of(column.id).len())
        .sum::<usize>();
    if app.is_empty_board() || visible_cards == 0 {
        let m = board_body_area(area);
        let (message, actions) = if app.is_empty_board() && app.card_filter == CardFilter::Active {
            ("Board is empty.", "N: new column  ·  T: apply template")
        } else {
            match app.card_filter {
                CardFilter::Active => ("No active cards.", "v: show all / archived"),
                CardFilter::All => ("No cards.", "v: show active / archived"),
                CardFilter::Archived => ("No archived cards.", "v: show active / all"),
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

fn running_card_count(app: &App) -> usize {
    app.board
        .cards
        .iter()
        .filter(|card| card.archived_at.is_none() && card.status == CardStatus::Running)
        .count()
}

fn draw_board_header(app: &App, f: &mut Frame, area: Rect, compact_header: Option<&CompactHeader>) {
    let header_area = board_header_area(area);
    if header_area.is_empty() {
        return;
    }
    f.render_widget(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(67, 91, 105)))
            .style(Style::default().bg(Color::Rgb(3, 13, 22))),
        header_area,
    );

    let identity = Rect::new(header_area.x, header_area.y, header_area.width, 1);
    if compact_header.is_some() {
        // Compact keeps the product identity and board selector on the first
        // row, filters on the second, and the column navigator on the third.
        draw_compact_identity(app, f, identity);
        let filters = Rect::new(
            header_area.x,
            header_area.y.saturating_add(1),
            header_area.width,
            compact_filter_rows(header_area.width).min(header_area.height.saturating_sub(1)),
        );
        draw_visibility_filters(app, f, filters);
        if let Some(header) = compact_header {
            draw_compact_header(app, f, header);
        }
        return;
    }

    // Brand identity (only the product name; the actual board name lives in
    // the dropdown row below), with the live running count on the right.
    let running = format!("● {} running", running_card_count(app));
    let running_w = running.chars().count() as u16;
    f.render_widget(
        Paragraph::new(Span::styled(
            " ◈ herdr-board",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(
            identity.x,
            identity.y,
            identity.width.saturating_sub(running_w.saturating_add(2)),
            1,
        ),
    );
    if running_w <= identity.width {
        f.render_widget(
            Paragraph::new(Span::styled(
                running,
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(
                identity.right().saturating_sub(running_w + 1),
                identity.y,
                running_w,
                1,
            ),
        );
    }
    let controls = Rect::new(
        header_area.x,
        header_area.y.saturating_add(1),
        header_area.width,
        1.min(header_area.height.saturating_sub(1)),
    );
    draw_scope_and_filter(app, f, controls);
}

fn draw_compact_identity(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let brand = " ◈ herdr-board";
    let brand_w = (brand.chars().count() as u16).min(area.width.saturating_sub(1));
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(brand, brand_w as usize),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, brand_w, 1),
    );
    let selector_area = Rect::new(
        area.x.saturating_add(brand_w).saturating_add(1),
        area.y,
        area.width.saturating_sub(brand_w).saturating_sub(1),
        1,
    );
    let label = format!("Board: {} ▾", app.board.board.name);
    render_button_chip_at(
        f,
        selector_area,
        &label,
        &mut app.hit_map.borrow_mut(),
        Zone::Action(UiAction::SwitchBoard),
    );
}

fn draw_scope_and_filter(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let filter_w = visibility_filter_width();
    let scope_w = area.width.saturating_sub(filter_w.saturating_add(1));
    if scope_w > 0 {
        let scope_area = Rect::new(area.x, area.y, scope_w, 1);
        let label = format!("Board: {} ▾", app.board.board.name);
        render_button_chip_at(
            f,
            scope_area,
            &label,
            &mut app.hit_map.borrow_mut(),
            Zone::Action(UiAction::SwitchBoard),
        );
    }
    draw_visibility_filters(app, f, area);
}

fn visibility_filter_width() -> u16 {
    let chips = ["Active", "All", "Archived"];
    let chips_width: usize = chips.iter().map(|label| button_text(label).len()).sum();
    ("Visible:".len() + 1 + chips_width + chips.len().saturating_sub(1)) as u16
}

fn draw_visibility_filters(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }

    // Preserve the established right-aligned one-row rail for all normal
    // widths. Compact only enters the wrapping path when its full set cannot
    // fit on one row.
    if area.height == 1 {
        let chips = [
            ("Active", CardFilter::Active),
            ("All", CardFilter::All),
            ("Archived", CardFilter::Archived),
        ];
        let total_w = visibility_filter_width();
        let mut x = area.right().saturating_sub(total_w).max(area.x);
        let label_w = "Visible:".chars().count() as u16;
        f.render_widget(
            Paragraph::new(Span::styled("Visible:", Style::default().fg(Color::Gray))),
            Rect::new(x, area.y, label_w.min(area.width), 1),
        );
        x = x.saturating_add(label_w).saturating_add(1);
        for (label, filter) in chips {
            let width = button_text(label).chars().count() as u16;
            let modifier = if filter == app.card_filter {
                Modifier::UNDERLINED
            } else {
                Modifier::empty()
            };
            render_button_chip_at_with_modifier(
                f,
                Rect::new(x, area.y, width, 1),
                label,
                &mut app.hit_map.borrow_mut(),
                Zone::Filter(filter),
                modifier,
            );
            x = x.saturating_add(width).saturating_add(1);
        }
        return;
    }

    // Compact keeps the transparent `[ NAME ]` chips but wraps the rail as a
    // unit when one row cannot contain every filter. The row count is shared
    // with `board_header_height`, so the final Archived chip is never silently
    // clipped out of the header.
    let chips = if area.width < 60 {
        compact_filter_options(area.width)
    } else {
        [
            ("Active", CardFilter::Active),
            ("All", CardFilter::All),
            ("Archived", CardFilter::Archived),
        ]
    };
    let label = if area.width < 60 {
        compact_filter_label(area.width)
    } else {
        "Visible:"
    };
    let label_w = label.chars().count() as u16;
    f.render_widget(
        Paragraph::new(Span::styled(label, Style::default().fg(Color::Gray))),
        Rect::new(area.x, area.y, label_w.min(area.width), 1),
    );

    let mut row = 0u16;
    let mut used = label_w.saturating_add(1).min(area.width);
    let mut needs_gap = false;
    for (label, filter) in chips {
        let width = button_text(label).chars().count() as u16;
        let gap = u16::from(needs_gap && used > 0);
        if used.saturating_add(gap).saturating_add(width) > area.width {
            row = row.saturating_add(1);
            if row >= area.height {
                // This is only reachable when the frame itself is too short
                // to support its responsive header; normal supported mobile
                // sizes allocate exactly `compact_filter_rows` rows.
                continue;
            }
            used = 0;
            needs_gap = false;
        }
        let gap = u16::from(needs_gap && used > 0);
        let x = area.x.saturating_add(used).saturating_add(gap);
        let modifier = if filter == app.card_filter {
            Modifier::UNDERLINED
        } else {
            Modifier::empty()
        };
        render_button_chip_at_with_modifier(
            f,
            Rect::new(x, area.y.saturating_add(row), width, 1),
            label,
            &mut app.hit_map.borrow_mut(),
            Zone::Filter(filter),
            modifier,
        );
        used = used.saturating_add(gap).saturating_add(width);
        needs_gap = true;
    }
}

fn draw_board_actions(app: &App, f: &mut Frame, area: Rect) {
    const CARD_BUTTONS: [ActionButton<'static>; 9] = [
        ActionButton {
            label: "+ New card",
            compact_label: "+ Card",
            action: UiAction::NewCard,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Open card",
            compact_label: "Open",
            action: UiAction::OpenCard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Archive card",
            compact_label: "Archive",
            action: UiAction::ArchiveCard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "+ New column",
            compact_label: "+ Column",
            action: UiAction::NewColumn,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Edit column",
            compact_label: "Edit col",
            action: UiAction::EditColumn,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Delete column",
            compact_label: "Del col",
            action: UiAction::DeleteColumn,
            tone: ActionTone::Destructive,
        },
        ActionButton {
            label: "Move column",
            compact_label: "Move col",
            action: UiAction::MoveColumn,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Template",
            compact_label: "Template",
            action: UiAction::ApplyTemplate,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "? Help",
            compact_label: "?",
            action: UiAction::Help,
            tone: ActionTone::Normal,
        },
    ];
    const EMPTY_BUTTONS: [ActionButton<'static>; 4] = [
        ActionButton {
            label: "+ New column",
            compact_label: "+ Column",
            action: UiAction::NewColumn,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Template",
            compact_label: "Template",
            action: UiAction::ApplyTemplate,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Switch board",
            compact_label: "Board",
            action: UiAction::SwitchBoard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "? Help",
            compact_label: "?",
            action: UiAction::Help,
            tone: ActionTone::Normal,
        },
    ];
    let area = board_action_area(area);
    if area.is_empty() {
        return;
    }
    let buttons: &[ActionButton<'static>] = if app.board.columns.is_empty() {
        &EMPTY_BUTTONS
    } else {
        &CARD_BUTTONS
    };
    let columns = board_action_columns(area.width) as usize;
    let mut hit_map = app.hit_map.borrow_mut();
    for (row, chunk) in buttons.chunks(columns).enumerate() {
        if row >= area.height as usize {
            break;
        }
        ActionStrip { buttons: chunk }.render(
            f,
            Rect::new(area.x, area.y + row as u16, area.width, 1),
            &mut hit_map,
        );
    }
}

/// Compact column navigation: `[ ‹ ]  [ column · n/n · cards  ▶n ]  [ › ]`.
fn draw_compact_header(app: &App, f: &mut Frame, header: &CompactHeader) {
    let mut hit_map = app.hit_map.borrow_mut();
    render_button_chip_at(f, header.prev, "‹", &mut hit_map, Zone::HeaderPrev);

    let n = app.board.columns.len();
    let running = running_card_count(app);
    let column = app.display_column(app.sel_col);
    let label = match column {
        Some(c) => format!(
            "{} · {}/{} · {} cards · ▶{}",
            c.name,
            app.sel_col + 1,
            n.max(1),
            app.cards_of(c.id).len(),
            running,
        ),
        None => format!("no columns · ▶{running}"),
    };
    render_button_chip_at(f, header.switch, &label, &mut hit_map, Zone::HeaderSwitch);
    render_button_chip_at(f, header.next, "›", &mut hit_map, Zone::HeaderNext);
}

fn draw_card(app: &App, f: &mut Frame, card: &Card, r: Rect, selected: bool, compact: bool) {
    let archived = card.archived_at.is_some();
    let (glyph, color) = if archived {
        ('▣', Color::DarkGray)
    } else {
        status_glyph(card.status)
    };
    let background = if selected {
        Color::Rgb(8, 33, 51)
    } else {
        Color::Rgb(7, 22, 34)
    };
    let border = if selected {
        Color::LightCyan
    } else if archived {
        Color::DarkGray
    } else {
        color
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(background));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.is_empty() {
        return;
    }

    let status_text = if archived {
        "archived".to_string()
    } else {
        card.status.as_str().to_string()
    };
    let permission = card
        .permission_mode
        .clone()
        .unwrap_or_else(|| "-".to_string());
    let model = card.model.clone().unwrap_or_else(|| "-".to_string());
    let effort = card
        .effort
        .map(|e| e.as_str().to_string())
        .unwrap_or_else(|| "default".to_string());
    let mut status = format!("{glyph} {status_text}");
    if !archived && card.status == CardStatus::Running {
        let started = app
            .active_run_for_card(card.id)
            .and_then(|run| parse_timestamp(&run.started_at))
            .or_else(|| parse_timestamp(&card.updated_at));
        let elapsed = run_elapsed(started, None, app.now).unwrap_or(0);
        status.push_str(&format!(" · {}", format_duration(Some(elapsed))));
    }

    let bg = background;
    if compact {
        // Compact cards keep the title/id on its own row(s), followed by one
        // status row and the two left/right metadata rows. Board-card
        // Edit/Delete controls intentionally do not appear; keyboard `e`/`d`
        // remains available.
        let title_rows = inner.height.saturating_sub(3).max(1); // leave status + two metadata rows
        let title_prefix = format!("#{} ", card.id);

        let title_area = Rect::new(inner.x, inner.y, inner.width, title_rows);
        // The first row is deliberately neutral. Semantic status color is
        // reserved for the status row (and the existing card border); it must
        // never leak into the card id or title.
        let title_style = Style::default().fg(Color::White).add_modifier(if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    title_prefix,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(card.title.clone(), title_style),
            ]))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(background)),
            title_area,
        );
        // Status owns the semantic glyph on the row immediately after the
        // title, keeping the title/id presentation free of status markers.
        let status_rect = Rect::new(inner.x, inner.y + title_rows, inner.width, 1);
        f.render_widget(
            Paragraph::new(Span::styled(
                status,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right)
            .style(Style::default().bg(background)),
            status_rect,
        );

        let mut y = inner.y + title_rows + 1;
        draw_card_pair_row(
            f,
            Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
            &card.harness,
            &format!("perm: {permission}"),
            bg,
        );
        y += 1;
        draw_card_pair_row(
            f,
            Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
            &model,
            &format!("effort: {effort}"),
            bg,
        );
        return;
    }

    // --- non-compact (desktop box) ---
    let title_prefix = format!("#{} ", card.id);
    let prefix_w = title_prefix.chars().count() as u16;
    let title = Line::from(vec![
        // Keep the desktop title/id row neutral for the same reason as the
        // Compact branch: only the following status row carries semantics.
        Span::styled(
            title_prefix,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate(&card.title, inner.width.saturating_sub(prefix_w) as usize),
            Style::default().fg(Color::White).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
    ]);
    f.render_widget(
        Paragraph::new(title).style(Style::default().bg(background)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height >= 2 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                status,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().bg(background)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
    let metadata_row = inner.bottom().saturating_sub(1);
    let meta = match (&card.model, card.effort) {
        (Some(_model), Some(eff)) => format!("{} · eff: {}", card.harness, eff.as_str()),
        _ => card.harness.clone(),
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(&meta, inner.width as usize),
            Style::default().fg(Color::Gray),
        ))
        .style(Style::default().bg(background)),
        Rect::new(inner.x, metadata_row, inner.width, 1),
    );
}

/// One left/right data row inside a card: white text on the left, grey on the
/// right, each truncated to half the row.
fn draw_card_pair_row(f: &mut Frame, area: Rect, left: &str, right: &str, bg: Color) {
    let half = area.width.saturating_sub(2) as usize / 2;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                crate::view::truncate(left, half.max(1)),
                Style::default().fg(Color::White),
            ),
            Span::raw(" "),
            Span::styled(
                crate::view::truncate(right, half.max(1)),
                Style::default().fg(Color::Gray),
            ),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(bg)),
        area,
    );
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
    hit_map.push(sheet, Zone::Shield);
    let inner = render_sheet_frame(
        f,
        sheet,
        mode == LayoutMode::Compact,
        full_title,
        compact_title,
        Style::default().fg(Color::LightBlue),
        &mut hit_map,
    );
    if mode != LayoutMode::Compact {
        let close_label = if state.level == SwitcherLevel::Boards && !state.entered_at_boards {
            "Back"
        } else {
            "Close"
        };
        let width = button_text(close_label).chars().count() as u16;
        let rect = Rect::new(sheet.right().saturating_sub(width + 2), sheet.y, width, 1);
        render_button_chip_at(f, rect, close_label, &mut hit_map, Zone::SheetClose);
    }
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
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Span::styled(format!(" {}  {} ", c.name, count), style))
                })
                .collect();
            let trailing_idx = app.board.columns.len();
            let trailing_style = if state.sel == trailing_idx {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(Color::White)
            };
            rows.push(ListItem::new(Span::styled(
                " ⇄  Switch board  → ",
                trailing_style,
            )));
            let template_idx = trailing_idx + 1;
            let template_enabled = app.is_empty_board();
            let template_style = if state.sel == template_idx {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else if template_enabled {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            rows.push(ListItem::new(Span::styled(
                " ⊞  Apply template  ",
                template_style,
            )));
            rows
        }
        SwitcherLevel::Boards => state
            .boards
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let style = if i == state.sel {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Span::styled(format!(" {} ", label), style))
            })
            .collect(),
    };
    let total = items.len();
    let heights = vec![1u16; total];
    let selected = state.sel.min(total.saturating_sub(1));
    let (start, end) = windowed_rows(&heights, selected, inner.height);
    let overflowing = end.saturating_sub(start) < total;
    let rows_area = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(u16::from(overflowing)),
        inner.height,
    );
    f.render_widget(
        List::new(
            items
                .into_iter()
                .skip(start)
                .take(end - start)
                .collect::<Vec<_>>(),
        ),
        rows_area,
    );

    // Zones carry absolute model indices even after the selection-follow window moves.
    for (visible_row, absolute) in (start..end).enumerate() {
        let rect = Rect::new(
            rows_area.x,
            rows_area.y + visible_row as u16,
            rows_area.width,
            1,
        );
        let zone = match state.level {
            SwitcherLevel::Columns if absolute == app.board.columns.len() => {
                Zone::SwitcherSwitchBoard
            }
            SwitcherLevel::Columns if absolute == app.board.columns.len() + 1 => {
                Zone::SwitcherApplyTemplate
            }
            _ => Zone::SwitcherRow(absolute),
        };
        hit_map.push(rect, zone);
    }
    if overflowing {
        crate::widgets::vertical_scrollbar(
            f,
            Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height),
            total,
            start,
            end - start,
        );
    }
}
