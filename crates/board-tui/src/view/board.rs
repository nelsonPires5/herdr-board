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
    board_action_area, board_action_columns, board_body_area_for, board_header_area, board_layout,
    centered_rect_abs, compact_filter_options, status_glyph, truncate, CompactHeader, LayoutMode,
};

// -- board -------------------------------------------------------------------

pub(super) fn draw_board(app: &App, f: &mut Frame, area: Rect) {
    let layout = board_layout(app, area);
    // The reorder mini-mode keeps the board's selection chrome: the cyan card
    // outline must follow the card being staged, and the column border stays
    // highlighted so the in-column scope of the move stays visible.
    let focused = app.screen == Screen::Board || app.screen == Screen::ReorderCard;
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
        let m = board_body_area_for(app, area);
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
        // Compact is deliberately three rows: brand/count, board + filters,
        // then the column navigator. The divider is the fourth header row.
        draw_compact_identity(app, f, identity);
        let controls = Rect::new(
            header_area.x,
            header_area.y.saturating_add(1),
            header_area.width,
            1.min(header_area.height.saturating_sub(1)),
        );
        draw_compact_controls(app, f, controls);
        if let Some(header) = compact_header {
            draw_compact_header(app, f, header);
        }
        return;
    }

    // Regular/Wide keep every header control on one balanced line. The left
    // group owns the product identity and the *global* running count; the
    // centered board chip and right-aligned filters each receive a disjoint
    // hit area, so long board names can only ellipsize inside their own slot.
    draw_desktop_header(app, f, identity);
}

fn running_label(app: &App) -> String {
    format!("● {} running", running_card_count(app))
}

fn draw_compact_identity(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let brand = " ◈ herdr-board";
    let brand_w = (brand.chars().count() as u16).min(area.width);
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(brand, brand_w as usize),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, brand_w, 1),
    );
    let running = running_label(app);
    let running_x = area.x.saturating_add(brand_w).saturating_add(1);
    if running_x < area.right() {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(&running, area.right().saturating_sub(running_x) as usize),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(running_x, area.y, area.right().saturating_sub(running_x), 1),
        );
    }
}

fn desktop_filter_options(width: u16) -> [(&'static str, CardFilter); 3] {
    if width >= 72 {
        [
            ("Active", CardFilter::Active),
            ("All", CardFilter::All),
            ("Archived", CardFilter::Archived),
        ]
    } else {
        // At the narrow Regular breakpoint, removing the redundant words is
        // what leaves room for a readable centered board dropdown.
        [
            ("A", CardFilter::Active),
            ("All", CardFilter::All),
            ("R", CardFilter::Archived),
        ]
    }
}

fn filter_chips_width(chips: &[(&str, CardFilter)]) -> u16 {
    chips
        .iter()
        .map(|(label, _)| button_text(label).chars().count() as u16)
        .sum::<u16>()
        .saturating_add(chips.len().saturating_sub(1) as u16)
}

/// Fit a board dropdown label without sacrificing its affordance. The shared
/// chip fitter truncates an arbitrary label from the end, which would turn a
/// long `name ▾` into `name…` and hide the fact that the control opens a menu.
fn board_dropdown_label(name: &str, max_width: u16) -> String {
    let label_width = max_width.saturating_sub(4) as usize; // `[ ` + ` ]`
    if label_width == 0 {
        return String::new();
    }

    const CHEVRON: &str = " ▾";
    let full = format!("{name}{CHEVRON}");
    if full.chars().count() <= label_width {
        return full;
    }
    if label_width == 1 {
        return "▾".into();
    }

    let name_width = label_width.saturating_sub(CHEVRON.chars().count());
    format!("{}{}", truncate(name, name_width), CHEVRON)
}

fn draw_desktop_header(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let chips = desktop_filter_options(area.width);
    let filters_w = filter_chips_width(&chips).min(area.width);
    let filters_x = area.right().saturating_sub(filters_w);
    let brand = " ◈ herdr-board";
    let brand_w = brand.chars().count() as u16;
    let running = running_label(app);
    let running_w = running.chars().count() as u16;
    let left_w = brand_w
        .saturating_add(1)
        .saturating_add(running_w)
        .min(filters_x.saturating_sub(area.x));

    f.render_widget(
        Paragraph::new(Span::styled(
            truncate(brand, brand_w.min(left_w) as usize),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(area.x, area.y, brand_w.min(left_w), 1),
    );
    let running_x = area.x.saturating_add(brand_w).saturating_add(1);
    if running_x < area.x.saturating_add(left_w) {
        f.render_widget(
            Paragraph::new(Span::styled(
                truncate(
                    &running,
                    area.x.saturating_add(left_w).saturating_sub(running_x) as usize,
                ),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect::new(
                running_x,
                area.y,
                area.x.saturating_add(left_w).saturating_sub(running_x),
                1,
            ),
        );
    }

    let center_x = area.x.saturating_add(left_w).saturating_add(1);
    let center_right = filters_x.saturating_sub(1).max(center_x);
    let center = Rect::new(center_x, area.y, center_right.saturating_sub(center_x), 1);
    let board_label = board_dropdown_label(&app.board.board.name, center.width);
    render_button_chip_at(
        f,
        center,
        &board_label,
        &mut app.hit_map.borrow_mut(),
        Zone::Action(UiAction::SwitchBoard),
    );
    draw_filter_chips(app, f, Rect::new(filters_x, area.y, filters_w, 1), &chips);
}

fn draw_compact_controls(app: &App, f: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }
    let chips = compact_filter_options(area.width);
    let filters_w = filter_chips_width(&chips).min(area.width);
    let filters_x = area.right().saturating_sub(filters_w);
    let board_w = filters_x.saturating_sub(area.x).saturating_sub(1);
    if board_w > 0 {
        let label = board_dropdown_label(&app.board.board.name, board_w);
        render_button_chip_at(
            f,
            Rect::new(area.x, area.y, board_w, 1),
            &label,
            &mut app.hit_map.borrow_mut(),
            Zone::Action(UiAction::SwitchBoard),
        );
    }
    draw_filter_chips(app, f, Rect::new(filters_x, area.y, filters_w, 1), &chips);
}

fn draw_filter_chips(app: &App, f: &mut Frame, area: Rect, chips: &[(&str, CardFilter)]) {
    if area.is_empty() {
        return;
    }
    let mut x = area.x;
    for (label, filter) in chips {
        let width = button_text(label).chars().count() as u16;
        let modifier = if *filter == app.card_filter {
            Modifier::UNDERLINED
        } else {
            Modifier::empty()
        };
        render_button_chip_at_with_modifier(
            f,
            Rect::new(x, area.y, width.min(area.right().saturating_sub(x)), 1),
            label,
            &mut app.hit_map.borrow_mut(),
            Zone::Filter(*filter),
            modifier,
        );
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

/// Compact column navigation: `[ ‹ ]  [ column (M/A) · n/n · cards ]  [ › ]`.
/// The global running count belongs to row one; it is intentionally absent
/// from this per-column navigator.
fn draw_compact_header(app: &App, f: &mut Frame, header: &CompactHeader) {
    let mut hit_map = app.hit_map.borrow_mut();
    render_button_chip_at(f, header.prev, "‹", &mut hit_map, Zone::HeaderPrev);

    let n = app.board.columns.len();
    let column = app.display_column(app.sel_col);
    let label = match column {
        Some(c) => {
            let trigger = c
                .trigger
                .as_str()
                .chars()
                .next()
                .unwrap_or('M')
                .to_ascii_uppercase();
            format!(
                "{} ({trigger}) · {}/{} · {} cards",
                c.name,
                app.sel_col + 1,
                n.max(1),
                app.cards_of(c.id).len(),
            )
        }
        None => "no columns".to_string(),
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
    // Daemon-stamped display labels when set; `-` (the standard empty-cell
    // mark, not a label) when the card has no override — compact tiles must
    // not truncate the long `default …` markers into misleading fragments.
    let permission = if card.permission_mode.is_some() {
        card.labels.permission.clone()
    } else {
        "-".to_string()
    };
    let model = if card.model.is_some() {
        card.labels.model.clone()
    } else {
        "-".to_string()
    };
    let effort = if card.effort.is_some() {
        card.labels.effort.clone()
    } else {
        "-".to_string()
    };
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

/// Keep a switcher row's leading/trailing marker cells while fitting its
/// dynamic label to the list viewport. In particular, the trailing space is
/// retained so an ellipsis is visibly separated from the sheet border.
fn truncate_switcher_row(row: &str, max_width: u16) -> String {
    let max_width = max_width as usize;
    let chars: Vec<char> = row.chars().collect();
    if chars.len() <= max_width {
        return row.to_string();
    }
    if max_width >= 3 && chars.first() == Some(&' ') && chars.last() == Some(&' ') {
        let content: String = chars[1..chars.len() - 1].iter().collect();
        return format!(" {} ", truncate(&content, max_width - 2));
    }
    truncate(row, max_width)
}

pub(super) fn draw_switcher(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.switcher else { return };
    let mode = app.layout_mode();
    let sheet = super::sheet_area_for_app(app, mode, 44, 14, area);
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
            "X"
        };
        let width = button_text(close_label).chars().count() as u16;
        let rect = Rect::new(sheet.right().saturating_sub(width + 2), sheet.y, width, 1);
        render_button_chip_at(f, rect, close_label, &mut hit_map, Zone::SheetClose);
    }
    let rows: Vec<(String, Style)> = match state.level {
        SwitcherLevel::Columns => {
            let mut rows: Vec<(String, Style)> = app
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
                    (format!(" {}  {} ", c.name, count), style)
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
            rows.push((" ⇄  Switch board  → ".into(), trailing_style));
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
            rows.push((" ⊞  Apply template  ".into(), template_style));
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
                (format!(" {} ", label), style)
            })
            .collect(),
    };
    let total = rows.len();
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
    let items = rows
        .into_iter()
        .map(|(row, style)| {
            ListItem::new(Span::styled(
                truncate_switcher_row(&row, rows_area.width),
                style,
            ))
        })
        .skip(start)
        .take(end - start)
        .collect::<Vec<_>>();
    f.render_widget(List::new(items), rows_area);

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
