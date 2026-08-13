use board_core::model::CommentHistory;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, PickerPurpose, Screen};
use crate::widgets::{
    button_text, render_button_chip_at, render_sheet_frame, windowed_rows, ActionButton,
    ActionStrip, ActionTone, HitMap, UiAction, Zone,
};

use super::{
    detail::wrapped_row_count, sheet_area_for_app, truncate, LayoutMode, HELP_GUTTER_WIDTH,
    HELP_KEYS,
};

// -- shared sheet chrome -----------------------------------------------------

/// Draw the existing sheet frame and add the same discoverable close affordance
/// to centered sheets that compact sheets already have.  The action remains
/// `SheetClose -> Esc`; this is chrome only, never a second reducer path.
fn render_overlay_frame(
    f: &mut Frame,
    box_area: Rect,
    compact: bool,
    titles: (&str, &str),
    border_style: Style,
    close_label: &str,
    hit_map: &mut HitMap,
) -> Rect {
    let centered_title;
    let frame_title = if compact {
        titles.0
    } else {
        // Keep dynamic titles from painting through the trailing close control.
        // Fixed action labels win; only hostile/dynamic title data ellipsizes.
        let close_width = button_text(close_label).chars().count();
        let max_title = box_area
            .width
            .saturating_sub(2) // corners
            .saturating_sub(close_width as u16)
            .saturating_sub(3) as usize; // title padding + visual gap
        centered_title = truncate(titles.0, max_title);
        &centered_title
    };
    hit_map.push(box_area, Zone::Shield);
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        frame_title,
        titles.1,
        border_style,
        hit_map,
    );
    if !compact && box_area.width > button_text(close_label).chars().count() as u16 + 4 {
        let width = button_text(close_label).chars().count() as u16;
        let rect = Rect::new(
            box_area.right().saturating_sub(width + 2),
            box_area.y,
            width,
            1,
        );
        render_button_chip_at(f, rect, close_label, hit_map, Zone::SheetClose);
    }
    inner
}

// -- picker / confirm / help / transient toast -----------------------------

pub(super) fn draw_picker(app: &App, f: &mut Frame, area: Rect) {
    let Some(picker) = &app.picker else { return };
    let mode = app.layout_mode();
    let compact = mode == LayoutMode::Compact;
    let content_w = picker
        .options
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max(picker.title.chars().count() + 12)
        .saturating_add(4)
        .clamp(30, 100) as u16;
    // A wrapped option may need more than one row.  Let centered sheets grow
    // to their content preference; `sheet_area` still clamps to the board
    // content region.
    let desired_rows: usize = picker
        .options
        .iter()
        .map(|(name, _)| wrapped_row_count(name, content_w.saturating_sub(3).max(1)))
        .sum();
    let content_h = (desired_rows as u16).saturating_add(2).max(5);
    let box_area = sheet_area_for_app(app, mode, content_w, content_h, area);
    f.render_widget(Clear, box_area);

    let visual_title = picker
        .title
        .replace("Move card to which column?", "Move card to column")
        .replace(" · b = other board", " · b other board");
    let compact_move_title = visual_title
        .split(" ·")
        .next()
        .unwrap_or(visual_title.as_str());
    let compact_title = match picker.purpose {
        PickerPurpose::SwitchBoard => "Switch board",
        PickerPurpose::MoveCardPickBoard { .. } => "Move to board",
        PickerPurpose::MoveCardPickColumn { .. } => compact_move_title,
        PickerPurpose::DeleteColumnMoveTo { .. } => "Move column cards",
    };
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        compact,
        (&visual_title, compact_title),
        Style::default().fg(Color::LightBlue),
        "X",
        &mut hit_map,
    );
    let other_board = matches!(picker.purpose, PickerPurpose::MoveCardPickColumn { .. });
    let picker_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(u16::from(other_board)),
    );
    if other_board && inner.height > 0 {
        let action_area = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let buttons = [ActionButton {
            label: "Other board",
            compact_label: "Other board",
            action: UiAction::PickerOtherBoard,
            tone: ActionTone::Normal,
        }];
        ActionStrip { buttons: &buttons }.render(f, action_area, &mut hit_map);
    }

    if picker.options.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "(no choices available)",
                Style::default().fg(Color::DarkGray),
            )),
            picker_area,
        );
        return;
    }

    // Always reserve a gutter when overflow is possible.  This makes wrapping
    // and row windows identical before/after selection or resize.
    let preliminary: Vec<u16> = picker
        .options
        .iter()
        .map(|(name, _)| {
            wrapped_row_count(name, picker_area.width.saturating_sub(2).max(1)).max(1) as u16
        })
        .collect();
    let overflow =
        preliminary.iter().map(|&h| h as usize).sum::<usize>() > picker_area.height as usize;
    let text_w = picker_area
        .width
        .saturating_sub(if overflow { 1 } else { 0 })
        .max(1);
    let heights: Vec<u16> = picker
        .options
        .iter()
        .map(|(name, _)| {
            // One cell is the selection marker; text itself remains whole-row wrapped.
            wrapped_row_count(name, text_w.saturating_sub(1).max(1)).max(1) as u16
        })
        .collect();
    let (start, end) = windowed_rows(&heights, picker.sel, picker_area.height);
    let constraints = heights[start..end]
        .iter()
        .copied()
        .map(Constraint::Length)
        .collect::<Vec<_>>();
    let row_area = Rect::new(picker_area.x, picker_area.y, text_w, picker_area.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(row_area);
    for (visible, option_idx) in (start..end).enumerate() {
        let (name, _) = &picker.options[option_idx];
        let selected = option_idx == picker.sel;
        let line = Line::from(vec![
            Span::styled(
                if selected { "›" } else { " " },
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                name.as_str(),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).wrap(Wrap { trim: false }),
            rows[visible],
        );
        hit_map.push(rows[visible], Zone::PickerRow(option_idx));
    }
    if overflow {
        crate::widgets::vertical_scrollbar(
            f,
            Rect::new(
                picker_area.right().saturating_sub(1),
                picker_area.y,
                1,
                picker_area.height,
            ),
            heights.iter().map(|&h| h as usize).sum(),
            heights[..start].iter().map(|&h| h as usize).sum(),
            picker_area.height as usize,
        );
    }
}

pub(super) fn draw_move_column(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.move_column else {
        return;
    };
    let name = app
        .board
        .columns
        .iter()
        .find(|c| c.id == state.column_id)
        .map(|c| c.name.as_str())
        .unwrap_or("column");
    let mode = app.layout_mode();
    let box_area = sheet_area_for_app(app, mode, 58, 5, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Move column", "Move column"),
        Style::default().fg(Color::Magenta),
        "X",
        &mut hit_map,
    );
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let move_controls = Layout::horizontal([
        Constraint::Length(10.min(chunks[0].width / 3)),
        Constraint::Min(1),
        Constraint::Length(11.min(chunks[0].width / 3)),
    ])
    .split(chunks[0]);
    render_button_chip_at(
        f,
        move_controls[0],
        "← Left",
        &mut hit_map,
        Zone::Action(UiAction::StageColumnLeft),
    );
    f.render_widget(
        Paragraph::new(format!("reorder {name}"))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        move_controls[1],
    );
    render_button_chip_at(
        f,
        move_controls[2],
        "Right →",
        &mut hit_map,
        Zone::Action(UiAction::StageColumnRight),
    );
    let buttons = [
        ActionButton {
            label: "Confirm",
            compact_label: "Confirm",
            action: UiAction::CommitColumnMove,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Cancel",
            compact_label: "Cancel",
            action: UiAction::CancelColumnMove,
            tone: ActionTone::Normal,
        },
    ];
    ActionStrip { buttons: &buttons }.render(f, chunks[1], &mut hit_map);
}

pub(super) fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let Some(confirm) = &app.confirm else { return };
    let mode = app.layout_mode();
    let target_w = if mode == LayoutMode::Compact {
        area.width
    } else {
        50
    };
    let message_w = target_w.saturating_sub(4).max(1);
    let message_rows = wrapped_row_count(&confirm.message, message_w).max(1) as u16;
    let box_area = sheet_area_for_app(app, mode, 50, message_rows.saturating_add(4).max(5), area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Confirm", "Confirm"),
        Style::default().fg(Color::Red),
        "X",
        &mut hit_map,
    );
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let message_area = Rect::new(
        chunks[0].x,
        chunks[0].y + chunks[0].height.saturating_sub(message_rows) / 2,
        chunks[0].width,
        message_rows.min(chunks[0].height),
    );
    f.render_widget(
        Paragraph::new(confirm.message.as_str())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        message_area,
    );
    let buttons = [
        ActionButton {
            label: "Yes",
            compact_label: "Yes",
            action: UiAction::ConfirmYes,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "No",
            compact_label: "No",
            action: UiAction::ConfirmNo,
            tone: ActionTone::Normal,
        },
    ];
    ActionStrip { buttons: &buttons }.render(f, chunks[1], &mut hit_map);
}

pub(super) fn draw_reorder_card(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.reorder_card else {
        return;
    };
    let title = app
        .board
        .cards
        .iter()
        .find(|c| c.id == state.card_id)
        .map(|c| c.title.as_str())
        .unwrap_or("card");
    let mode = app.layout_mode();
    let box_area = sheet_area_for_app(app, mode, 58, 5, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Reorder card", "Reorder card"),
        Style::default().fg(Color::Magenta),
        "X",
        &mut hit_map,
    );
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let move_controls = Layout::horizontal([
        Constraint::Length(8.min(chunks[0].width / 3)),
        Constraint::Min(1),
        Constraint::Length(10.min(chunks[0].width / 3)),
    ])
    .split(chunks[0]);
    render_button_chip_at(
        f,
        move_controls[0],
        "↑",
        &mut hit_map,
        Zone::Action(UiAction::StageCardUp),
    );
    f.render_widget(
        Paragraph::new(format!("reorder {title}"))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        move_controls[1],
    );
    render_button_chip_at(
        f,
        move_controls[2],
        "↓",
        &mut hit_map,
        Zone::Action(UiAction::StageCardDown),
    );
    let buttons = [
        ActionButton {
            label: "Confirm",
            compact_label: "Confirm",
            action: UiAction::CommitCardReorder,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Cancel",
            compact_label: "Cancel",
            action: UiAction::CancelCardReorder,
            tone: ActionTone::Normal,
        },
    ];
    ActionStrip { buttons: &buttons }.render(f, chunks[1], &mut hit_map);
}

/// Inner content rect of the help sheet's stacked section-card list. The
/// content fills the sheet; no persistent hint row is reserved.
pub fn help_list_rect(app: &App, area: Rect) -> Rect {
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area_for_app(app, app.layout_mode(), 110, content_h, area);
    Rect::new(
        box_area.x + 1,
        box_area.y + 1,
        box_area.width.saturating_sub(2),
        box_area.height.saturating_sub(2),
    )
}

pub fn help_content_width(list_rect: Rect) -> u16 {
    list_rect.width.saturating_sub(1).max(1)
}

/// One help section: its card title plus the key rows it documents.
pub type HelpSection<'a> = (String, &'a [(Screen, &'a str, &'a str)]);

/// Split `HELP_KEYS` at its `"--"` separators into titled sections. The
/// first section (the board) has no leading separator; every separator
/// introduces the section that follows it. Titles are capitalized for use as
/// card titles.
pub fn help_sections() -> Vec<HelpSection<'static>> {
    fn capitalize(s: &str) -> String {
        let mut chars = s.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }
    let mut out: Vec<HelpSection<'static>> = Vec::new();
    let mut title = "Board".to_string();
    let mut start = 0usize;
    for (i, (_, k, d)) in HELP_KEYS.iter().enumerate() {
        if *k == "--" {
            if i > start {
                out.push((title.clone(), &HELP_KEYS[start..i]));
            }
            title = capitalize(d.trim_matches(|c| c == '-' || c == ' '));
            start = i + 1;
        }
    }
    if start < HELP_KEYS.len() {
        out.push((title, &HELP_KEYS[start..]));
    }
    out
}

/// Wrapped rows a section's key rows occupy at `width` (the text width
/// inside the card borders), measured exactly as the renderer draws them: a
/// padded key column plus the description, greedy-wrapped at the card's
/// content width.
fn section_key_rows(section: &HelpSection, width: u16) -> usize {
    section
        .1
        .iter()
        .map(|(_, k, d)| wrapped_row_count(&format!("{:<11} {}", k, d), width))
        .sum()
}

/// Full height of one section card: top + bottom border plus the wrapped key
/// rows (a section with no rows still renders its title card). `text_w` is
/// the width of the text area inside the card borders.
fn section_card_height(section: &HelpSection, text_w: u16) -> usize {
    section_key_rows(section, text_w).max(1).saturating_add(2)
}

/// Stacked rows of a list of section cards, with a 1-row gap between cards.
/// The single source the renderer and the scroll clamps both use, so
/// `help_scroll` can never exceed what is actually drawn.
fn help_column_rows(sections: &[HelpSection], text_w: u16) -> usize {
    let cards: usize = sections
        .iter()
        .map(|section| section_card_height(section, text_w))
        .sum();
    cards.saturating_add(sections.len().saturating_sub(1))
}

/// Total rows of the single-column (Compact/Regular) help sheet: every
/// section card plus the inter-card gaps. `width` is the card width
/// including its side borders, matching what the renderer draws. The Compact
/// scroll clamp.
pub fn help_wrapped_rows(width: u16) -> usize {
    help_column_rows(&help_sections(), width.saturating_sub(2).max(1))
}

/// Wide-mode two-column split of the help sections: sections keep their
/// reading order, and the cut lands as close to half the stacked rows as
/// possible so both columns need roughly the same amount of scrolling.
fn help_wide_columns(inner: Rect) -> (Vec<HelpSection<'static>>, Vec<HelpSection<'static>>) {
    let gutter = HELP_GUTTER_WIDTH.min(inner.width.saturating_sub(2));
    let columns_width = inner.width.saturating_sub(gutter);
    let left_w = columns_width / 2;
    let sections = help_sections();
    let total = help_column_rows(&sections, left_w.saturating_sub(2).max(1));
    let half = total / 2;
    let mut cut = 0usize;
    let mut acc = 0usize;
    while cut < sections.len() {
        let next =
            section_card_height(&sections[cut], left_w.saturating_sub(2).max(1)).saturating_add(1); // + gap
        if acc.saturating_add(next) > half && cut > 0 {
            break;
        }
        acc = acc.saturating_add(next);
        cut += 1;
    }
    (sections[..cut].to_vec(), sections[cut..].to_vec())
}

pub fn help_regular_max_scroll(app: &App, area: Rect) -> usize {
    if app.layout_mode() != LayoutMode::Wide {
        let rect = help_list_rect(app, area);
        return help_wrapped_rows(help_content_width(rect))
            .saturating_sub(rect.height.max(1) as usize);
    }
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area_for_app(app, app.layout_mode(), 110, content_h, area);
    let inner = Rect::new(
        box_area.x + 1,
        box_area.y + 1,
        box_area.width.saturating_sub(2),
        box_area.height.saturating_sub(2),
    );
    let visible = inner.height as usize;
    let (left, right) = help_wide_columns(inner);
    let text_w = inner.width.saturating_sub(HELP_GUTTER_WIDTH) / 2;
    let text_w = text_w.saturating_sub(2).max(1);
    help_column_rows(&left, text_w)
        .max(help_column_rows(&right, text_w))
        .saturating_sub(visible)
}

/// Render stacked section cards into `area`, clipping each card to the
/// `[scroll, scroll + area.height)` window. Border rows are drawn as separate
/// top (title) / bottom strips and the key rows as a side-bordered block, so
/// a card cut off by the window edge keeps its borders on the correct rows
/// instead of ratatui reflowing them into the content area.
fn draw_help_section_cards(
    f: &mut Frame,
    area: Rect,
    sections: &[HelpSection],
    scroll: usize,
    card_w: u16,
) {
    let window_lo = scroll;
    let window_hi = scroll.saturating_add(area.height as usize);
    let text_w = card_w.saturating_sub(2).max(1);
    let border_style = Style::default().fg(Color::DarkGray);
    let mut top = 0usize;
    for section in sections {
        let card_h = section_card_height(section, text_w);
        let bottom = top.saturating_add(card_h);
        // Top border strip (carries the section title). `Borders::TOP` alone
        // draws no corner glyphs, so the `┌┐` cells are stamped afterwards.
        if top >= window_lo && top < window_hi {
            let strip = Rect::new(area.x, area.y + (top - window_lo) as u16, card_w, 1);
            let title = format!(" {} ", section.0);
            let title = crate::view::truncate(&title, card_w.saturating_sub(3) as usize);
            f.render_widget(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(border_style)
                    .title(title),
                strip,
            );
            let buf = f.buffer_mut();
            buf[(strip.x, strip.y)]
                .set_symbol("┌")
                .set_style(border_style);
            buf[(strip.right() - 1, strip.y)]
                .set_symbol("┐")
                .set_style(border_style);
        }
        // Key rows `[top+1, bottom-1)` with their left/right borders.
        let c_lo = (top + 1).max(window_lo);
        let c_hi = (bottom.saturating_sub(1)).min(window_hi);
        if c_lo < c_hi {
            let block = Block::default()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(border_style);
            let outer = Rect::new(
                area.x,
                area.y + (c_lo - window_lo) as u16,
                card_w,
                (c_hi - c_lo) as u16,
            );
            let inner = block.inner(outer);
            let lines: Vec<Line> = section
                .1
                .iter()
                .map(|(_, k, d)| {
                    Line::from(vec![
                        Span::styled(format!("{:<11} ", k), Style::default().fg(Color::Yellow)),
                        Span::raw(*d),
                    ])
                })
                .collect();
            f.render_widget(block, outer);
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll(((c_lo - (top + 1)) as u16, 0)),
                inner,
            );
        }
        // Bottom border strip, corners stamped like the top one.
        let bottom_row = bottom.saturating_sub(1);
        if bottom_row >= window_lo && bottom_row < window_hi {
            let strip = Rect::new(area.x, area.y + (bottom_row - window_lo) as u16, card_w, 1);
            f.render_widget(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(border_style),
                strip,
            );
            let buf = f.buffer_mut();
            buf[(strip.x, strip.y)]
                .set_symbol("└")
                .set_style(border_style);
            buf[(strip.right() - 1, strip.y)]
                .set_symbol("┘")
                .set_style(border_style);
        }
        top = bottom.saturating_add(1); // + inter-card gap
    }
}

pub(super) fn draw_help(app: &App, f: &mut Frame, area: Rect) {
    if app.layout_mode() != LayoutMode::Wide {
        draw_help_wrapped(app, f, area);
        return;
    }
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area_for_app(app, app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        false,
        ("Help — all keybindings", "Help"),
        Style::default().fg(Color::LightBlue),
        "X",
        &mut hit_map,
    );
    let scroll = app.help_scroll.min(help_regular_max_scroll(app, area));
    let (left, right) = help_wide_columns(inner);
    let gutter = HELP_GUTTER_WIDTH.min(inner.width.saturating_sub(2));
    let columns_width = inner.width.saturating_sub(gutter);
    let left_w = columns_width / 2;
    draw_help_section_cards(
        f,
        Rect::new(inner.x, inner.y, left_w, inner.height),
        &left,
        scroll,
        left_w,
    );
    let right_w = columns_width - left_w;
    draw_help_section_cards(
        f,
        Rect::new(inner.x + left_w + gutter, inner.y, right_w, inner.height),
        &right,
        scroll,
        right_w,
    );
}

fn draw_help_wrapped(app: &App, f: &mut Frame, area: Rect) {
    let content_h = HELP_KEYS.len().div_ceil(2) as u16 + 2;
    let box_area = sheet_area_for_app(app, app.layout_mode(), 110, content_h, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let block_inner = render_overlay_frame(
        f,
        box_area,
        app.layout_mode() == LayoutMode::Compact,
        ("Help — all keybindings", "Help"),
        Style::default().fg(Color::LightBlue),
        "X",
        &mut hit_map,
    );
    let list_rect = block_inner;
    let content_w = help_content_width(list_rect);
    let total_rows = help_wrapped_rows(content_w);
    let visible_rows = list_rect.height.max(1) as usize;
    let max_scroll = total_rows.saturating_sub(visible_rows);
    let scroll = app.help_scroll.min(max_scroll);
    let sections = help_sections();
    draw_help_section_cards(
        f,
        Rect::new(list_rect.x, list_rect.y, content_w, list_rect.height),
        &sections,
        scroll,
        content_w,
    );
    if total_rows > visible_rows {
        let sb = Rect::new(
            list_rect.right().saturating_sub(1),
            list_rect.y,
            1,
            list_rect.height,
        );
        crate::widgets::vertical_scrollbar(f, sb, total_rows, scroll, visible_rows);
        if sb.height > 0 {
            let up = Rect::new(sb.x, sb.y, 1, 1);
            f.render_widget(Paragraph::new("▲"), up);
            hit_map.push(up, Zone::HelpScrollUp);
        }
        if sb.height > 1 {
            let down = Rect::new(sb.x, sb.bottom() - 1, 1, 1);
            f.render_widget(Paragraph::new("▼"), down);
            hit_map.push(down, Zone::HelpScrollDown);
        }
    }
}

// -- comment history sheet ---------------------------------------------------

const COMMENT_HISTORY_W: u16 = 90;
const COMMENT_HISTORY_H: u16 = 24;

pub fn comment_history_rect(app: &App, area: Rect) -> Rect {
    let box_area = sheet_area_for_app(
        app,
        app.layout_mode(),
        COMMENT_HISTORY_W,
        COMMENT_HISTORY_H,
        area,
    );
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(box_area);
    Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(1).max(1),
        inner.height,
    )
}

fn comment_history_header(idx: usize, entry: &CommentHistory) -> String {
    let mut header = format!("#{} {}", idx + 1, entry.created_at);
    if entry.deleted_at.is_some() {
        header.push_str(" · deleted");
    }
    header
}

pub fn comment_history_wrapped_rows(entries: &[CommentHistory], width: u16) -> usize {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            wrapped_row_count(&comment_history_header(i, e), width)
                + wrapped_row_count(&e.body, width)
        })
        .sum::<usize>()
        .max(1)
}

pub(super) fn draw_comment_history(app: &App, f: &mut Frame, area: Rect) {
    let Some(state) = &app.comment_history else {
        return;
    };
    let mode = app.layout_mode();
    let box_area = sheet_area_for_app(app, mode, COMMENT_HISTORY_W, COMMENT_HISTORY_H, area);
    f.render_widget(Clear, box_area);
    let mut hit_map = app.hit_map.borrow_mut();
    let inner = render_overlay_frame(
        f,
        box_area,
        mode == LayoutMode::Compact,
        ("Comment history (j/k scroll)", "History"),
        Style::default().fg(Color::LightBlue),
        "X",
        &mut hit_map,
    );
    let content = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(1).max(1),
        inner.height,
    );
    if state.entries.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "(no history)",
                Style::default().fg(Color::Gray),
            )),
            content,
        );
        return;
    }
    let mut lines = Vec::with_capacity(state.entries.len() * 2);
    for (i, e) in state.entries.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            comment_history_header(i, e),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(e.body.as_str()));
    }
    let total_rows = comment_history_wrapped_rows(&state.entries, content.width);
    let visible = content.height.max(1) as usize;
    let scroll = state.scroll.min(total_rows.saturating_sub(visible));
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        content,
    );
    if total_rows > visible {
        let sb = Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height);
        crate::widgets::vertical_scrollbar(f, sb, total_rows, scroll, visible);
        if sb.height > 0 {
            let up = Rect::new(sb.x, sb.y, 1, 1);
            f.render_widget(Paragraph::new("▲"), up);
            hit_map.push(up, Zone::HistoryScrollUp);
        }
        if sb.height > 1 {
            let down = Rect::new(sb.x, sb.bottom() - 1, 1, 1);
            f.render_widget(Paragraph::new("▼"), down);
            hit_map.push(down, Zone::HistoryScrollDown);
        }
    }
}

/// Paint only a transient toast. The old contextual hint/footer row is
/// intentionally gone; board actions remain the persistent bottom rail and an
/// idle frame reserves no extra blank row for help text.
pub(super) fn draw_footer(app: &App, f: &mut Frame, area: Rect) {
    let Some(toast) = &app.toast else {
        return;
    };
    let action_h = super::board_action_rows(area.width).min(area.height);
    let header_h = super::board_header_height(area.width).min(area.height.saturating_sub(action_h));
    if area.height <= header_h.saturating_add(action_h) {
        return;
    }
    let y = area.bottom().saturating_sub(action_h + 1);
    let rect = Rect::new(area.x, y, area.width, 1);
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
}
