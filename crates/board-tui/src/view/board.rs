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
    render_sheet_frame, windowed_rows, ActionButton, ActionStrip, ActionTone, UiAction, Zone,
};

use super::{
    board_action_area, board_action_columns, board_body_area, board_header_area, board_layout,
    centered_rect_abs, main_area, sheet_area, status_glyph, truncate, CompactHeader, LayoutMode,
};

// -- board -------------------------------------------------------------------

pub(super) fn draw_board(app: &App, f: &mut Frame, area: Rect) {
    let layout = board_layout(app, area);
    let focused = app.screen == Screen::Board;
    let compact = app.layout_mode() == LayoutMode::Compact;

    // The board title/running/scope chrome stays visible behind every overlay:
    // sheets draw over `main_area` (below the header), so drawing the header
    // unconditionally keeps the header pinned at the top even while a picker,
    // detail, form, or help sheet is open.
    draw_board_header(app, f, area, layout.compact_header.as_ref());
    if focused {
        draw_board_actions(app, f, area);
    }

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
            draw_card(app, f, card, *r, selected, compact, (col.idx, *ci));
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
        let m = if focused {
            board_body_area(area)
        } else {
            main_area(area)
        };
        let (message, actions) = if app.is_empty_board() {
            ("Board is empty.", "N: new column  ·  T: apply template")
        } else {
            ("No active cards.", "v: show all / archived")
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
    let controls = Rect::new(
        header_area.x,
        header_area.y.saturating_add(1),
        header_area.width,
        1.min(header_area.height.saturating_sub(1)),
    );
    if let Some(header) = compact_header {
        draw_compact_header(app, f, header);
        draw_scope_and_filter(app, f, controls);
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
    draw_scope_and_filter(app, f, controls);
}

fn draw_scope_and_filter(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let compact = app.layout_mode() == LayoutMode::Compact;

    // Left: the board selector dropdown showing the FULL board name (wider than
    // before so it is not truncated).
    if !compact {
        let scope_w = if area.width < 78 {
            (area.width / 2).min(60)
        } else {
            (area.width / 2).min(72)
        };
        let scope = format!(
            "[ Board: {} ▾ ]",
            truncate(&app.board.board.name, scope_w.saturating_sub(13) as usize)
        );
        let scope_rect = Rect::new(area.x, area.y, scope_w, 1);
        f.render_widget(
            Paragraph::new(scope)
                .style(Style::default().fg(Color::White).bg(Color::Rgb(7, 22, 34))),
            scope_rect,
        );
        app.hit_map
            .borrow_mut()
            .push(scope_rect, Zone::Action(UiAction::SwitchBoard));
    }

    // Right: the Visible: filter buttons. Unselected = white text; selected =
    // black-on-white (per the request: no weird grey, buttons white/black).
    let active = app.card_filter == CardFilter::Active;
    let all = app.card_filter == CardFilter::All;
    let archived = app.card_filter == CardFilter::Archived;
    let selected_style = || {
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let unselected_style = || Style::default().fg(Color::White);

    let chips: [(String, bool); 3] = [
        ("Active".to_string(), active),
        ("All".to_string(), all),
        ("Archived".to_string(), archived),
    ];
    let total_w: usize = chips
        .iter()
        .map(|(c, _)| c.chars().count() + 2)
        .sum::<usize>()
        + chips.len().saturating_sub(1) * 2
        + "Visible:".chars().count()
        + 1;

    let mut x = area.right().saturating_sub(total_w as u16).max(area.x);
    let label_rect = Rect::new(x, area.y, "Visible:".chars().count() as u16, 1);
    if compact {
        f.render_widget(
            Paragraph::new(Span::styled("Visible:", Style::default().fg(Color::Gray))),
            label_rect,
        );
        x = x.saturating_add(label_rect.width).saturating_add(1);
    } else {
        // non-compact: skip "Visible:" label, just the three tappable buttons
        x = area
            .right()
            .saturating_sub(
                chips
                    .iter()
                    .map(|(c, _)| c.chars().count() + 2 + 2)
                    .sum::<usize>() as u16,
            )
            .max(area.x);
    }
    for (label, is_sel) in &chips {
        let shown = format!("[{label}]");
        let width = shown.chars().count() as u16;
        if x.saturating_add(width) > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        let style = if *is_sel {
            selected_style()
        } else {
            unselected_style()
        };
        f.render_widget(Paragraph::new(Span::styled(shown, style)), rect);
        let filter = match label.as_str() {
            "Active" => CardFilter::Active,
            "All" => CardFilter::All,
            _ => CardFilter::Archived,
        };
        app.hit_map.borrow_mut().push(rect, Zone::Filter(filter));
        x = x.saturating_add(width).saturating_add(1);
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

/// Compact column navigation: `‹  Name · n/n · cards  ›`.
fn draw_compact_header(app: &App, f: &mut Frame, header: &CompactHeader) {
    let mut hit_map = app.hit_map.borrow_mut();

    f.render_widget(
        Paragraph::new(Span::styled(
            "‹",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        header.prev,
    );
    hit_map.push(header.prev, Zone::HeaderPrev);

    let n = app.board.columns.len();
    let running = running_card_count(app);
    let column = app.display_column(app.sel_col);
    let label = match column {
        Some(c) => format!(
            "[{} · {}/{} · {} cards · ▶{}]",
            c.name,
            app.sel_col + 1,
            n.max(1),
            app.cards_of(c.id).len(),
            running,
        ),
        None => format!("[no columns · ▶{running}]"),
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(&label, header.switch.width as usize),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        header.switch,
    );
    hit_map.push(header.switch, Zone::HeaderSwitch);

    f.render_widget(
        Paragraph::new(Span::styled(
            "›",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        header.next,
    );
    hit_map.push(header.next, Zone::HeaderNext);
}

fn draw_card(
    app: &App,
    f: &mut Frame,
    card: &Card,
    r: Rect,
    selected: bool,
    compact: bool,
    at: (usize, usize),
) {
    let (col_idx, card_idx) = at;
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

    let bg = background;
    if compact {
        // Compact box: one status marker (in the title row) only, plus three
        // left/right data rows:  title|status,  harness|permission,
        // model|effort, and the [Edit] [Delete] controls at the bottom row.
        let row_count = inner.height.saturating_sub(1).max(1); // leave one action row
        let title_rows = row_count.saturating_sub(3).max(1); // rows for title (may wrap)
        let title_prefix = format!("{glyph} #{} ", card.id);

        let title_area = Rect::new(inner.x, inner.y, inner.width, title_rows);
        let title_style = Style::default()
            .fg(if archived {
                Color::DarkGray
            } else {
                Color::White
            })
            .add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    title_prefix,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(card.title.clone(), title_style),
            ]))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(background)),
            title_area,
        );
        // status on the same (last title) line, right-aligned
        let status_rect = Rect::new(
            inner.x + 2,
            inner.y + title_rows.saturating_sub(1),
            inner.width.saturating_sub(2),
            1,
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{glyph} {status_text}"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right)
            .style(Style::default().bg(background)),
            status_rect,
        );

        let mut y = inner.y + title_rows;
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

        // [Edit] [Delete] actions on the last row, right-aligned
        let actions_row = inner.bottom().saturating_sub(1);
        let edit_w = "[Edit]".chars().count() as u16;
        let delete_w = "[Delete]".chars().count() as u16;
        let total_w = edit_w.saturating_add(delete_w).saturating_add(1);
        if total_w <= inner.width {
            let x0 = inner.right().saturating_sub(total_w);
            let edit_rect = Rect::new(x0, actions_row, edit_w, 1);
            let delete_rect = Rect::new(x0 + edit_w + 1, actions_row, delete_w, 1);
            draw_card_controls(app, f, edit_rect, delete_rect, col_idx, card_idx, bg);
        }
        return;
    }

    // --- non-compact (desktop box) ---
    let title_prefix = format!("{glyph} #{} ", card.id);
    let prefix_w = title_prefix.chars().count() as u16;
    let title = Line::from(vec![
        Span::styled(
            title_prefix,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate(&card.title, inner.width.saturating_sub(prefix_w) as usize),
            Style::default()
                .fg(if archived {
                    Color::DarkGray
                } else {
                    Color::White
                })
                .add_modifier(if selected {
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
    let mut status = format!("{glyph} {status_text}");
    if !archived && card.status == CardStatus::Running {
        let started = app
            .active_run_for_card(card.id)
            .and_then(|run| parse_timestamp(&run.started_at))
            .or_else(|| parse_timestamp(&card.updated_at));
        let elapsed = run_elapsed(started, None, app.now).unwrap_or(0);
        status.push_str(&format!(" · {}", format_duration(Some(elapsed))));
    }
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
    let actions_row = inner.bottom().saturating_sub(1);
    let edit_w = "[Edit]".chars().count() as u16;
    let delete_w = "[Delete]".chars().count() as u16;
    let total_w = edit_w.saturating_add(delete_w).saturating_add(1);
    let meta = match (&card.model, card.effort) {
        (Some(_model), Some(eff)) => format!("{} · eff: {}", card.harness, eff.as_str()),
        _ => card.harness.clone(),
    };
    let meta_w = inner.width.saturating_sub(total_w.saturating_add(1));
    if meta_w >= 2 && total_w <= inner.width {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&meta, meta_w as usize),
                Style::default().fg(Color::Gray),
            ))
            .style(Style::default().bg(background)),
            Rect::new(inner.x, actions_row, meta_w, 1),
        );
    }
    if total_w <= inner.width {
        let x0 = inner.right().saturating_sub(total_w);
        draw_card_controls(
            app,
            f,
            Rect::new(x0, actions_row, edit_w, 1),
            Rect::new(x0 + edit_w + 1, actions_row, delete_w, 1),
            col_idx,
            card_idx,
            bg,
        );
    }
}

/// Render the [Edit] [Delete] controls inside a board card and register their
/// hit zones pointing at that exact card. Kept as a free function so it is not
/// a closure borrowing the frame.
fn draw_card_controls(
    app: &App,
    f: &mut Frame,
    edit_rect: Rect,
    delete_rect: Rect,
    col_idx: usize,
    card_idx: usize,
    background: Color,
) {
    f.render_widget(
        Paragraph::new(Span::styled(
            "[Edit]",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(background)),
        edit_rect,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "[Delete]",
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(background)),
        delete_rect,
    );
    let mut hit_map = app.hit_map.borrow_mut();
    hit_map.push(
        edit_rect,
        Zone::CardAction {
            col_idx,
            card_idx,
            action: UiAction::EditCard,
        },
    );
    hit_map.push(
        delete_rect,
        Zone::CardAction {
            col_idx,
            card_idx,
            action: UiAction::DeleteCard,
        },
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
            "[Back]"
        } else {
            "[Close]"
        };
        let width = close_label.chars().count() as u16;
        let rect = Rect::new(sheet.right().saturating_sub(width + 2), sheet.y, width, 1);
        f.render_widget(Paragraph::new(close_label), rect);
        hit_map.push(rect, Zone::SheetClose);
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
            let template_idx = trailing_idx + 1;
            let template_enabled = app.is_empty_board();
            let template_style = if state.sel == template_idx {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if template_enabled {
                Style::default().fg(Color::Cyan)
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
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
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
