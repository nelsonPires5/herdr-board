use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::forms::{FieldKind, Form};
use crate::widgets::{render_sheet_frame, windowed_rows, ButtonBar};

use super::{sheet_area, truncate, LayoutMode};

// -- form --------------------------------------------------------------------

// Caps a multiline field's value at 4 wrapped rows (+1 label row = 5 total,
// in both Regular/Wide and Compact) so one long description field can never
// starve the rest of the form — see `windowed_rows` for how the remaining
// fields scroll to keep the focused one in view instead of overflowing the
// sheet.
const MULTILINE_ROWS: u16 = 5;

/// Height (in rows, including its label row) a field occupies.
fn field_height(field: &crate::forms::Field) -> u16 {
    if field.multiline {
        MULTILINE_ROWS
    } else {
        2
    }
}

pub(super) fn draw_form(app: &App, form: &Form, f: &mut Frame, area: Rect) {
    let mode = app.layout_mode();

    // Content-sized on large terminals, while still shrinking to small ones.
    let visible: Vec<usize> = (0..form.fields.len())
        .filter(|i| form.field_visible(*i))
        .collect();
    let heights: Vec<u16> = visible
        .iter()
        .map(|&i| field_height(&form.fields[i]))
        .collect();
    let content_h = heights.iter().sum::<u16>().saturating_add(3); // fields + reserved button-bar row

    let box_area = sheet_area(mode, 96, content_h, area);
    f.render_widget(Clear, box_area);

    let mut hit_map = app.hit_map.borrow_mut();
    let compact = mode == LayoutMode::Compact;
    let full_title = format!("{} (Tab: field · Enter: save · Esc: cancel)", form.title());
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        &full_title,
        form.title(),
        Style::default().fg(Color::Blue),
        &mut hit_map,
    );

    // Reserve the last row for the button bar.
    let fields_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let bar_area = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    ButtonBar::new("Save", "Cancel").render(f, bar_area, &mut hit_map);

    // Vertical scroll: show the widest run of *whole* fields around the
    // focused one that fits in `fields_area`, so the focused field is always
    // fully visible and nothing ever renders past the reserved rect.
    // `sheet_area`/`content_h` already grow the sheet to fit every field when
    // the terminal has room (see `content_h` above); this only windows when
    // the fields genuinely cannot fit in `main_area` at this size.
    let focus_pos = visible.iter().position(|&i| i == form.focus).unwrap_or(0);
    let (win_start, win_end) = windowed_rows(&heights, focus_pos, fields_area.height);
    let overflowing = win_end - win_start < visible.len();

    // Same treatment `board_layout`'s per-column scrollbar and the Compact
    // help list get: a 1-cell track on the right edge whenever the window
    // doesn't show everything, in every layout mode (not just Compact) — so
    // scrolled-off fields (e.g. `space ref`/`harness override` on a plain
    // 80x24 desktop terminal) are at least visibly indicated as present
    // rather than silently missing. Only reserve the column when it's
    // actually needed, so the non-overflowing path never loses a column.
    let fields_area = if overflowing {
        Rect::new(
            fields_area.x,
            fields_area.y,
            fields_area.width.saturating_sub(1),
            fields_area.height,
        )
    } else {
        fields_area
    };
    if overflowing {
        let sb_rect = Rect::new(
            fields_area.x + fields_area.width,
            fields_area.y,
            1,
            fields_area.height,
        );
        let mut state = ScrollbarState::new(visible.len())
            .position(win_start)
            .viewport_content_length((win_end - win_start).max(1));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        f.render_stateful_widget(scrollbar, sb_rect, &mut state);
    }
    drop(hit_map);

    let win_visible = &visible[win_start..win_end];
    let win_heights = &heights[win_start..win_end];

    let constraints: Vec<Constraint> = win_heights.iter().map(|h| Constraint::Length(*h)).collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(fields_area);

    for (row_idx, &fi) in win_visible.iter().enumerate() {
        let field = &form.fields[fi];
        let is_focus = fi == form.focus;
        let label_style = if is_focus {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let row_area = rows[row_idx];
        let hint = if field.multiline {
            "  (Ctrl+E: $EDITOR)"
        } else {
            ""
        };
        let label_line = Line::from(Span::styled(
            truncate(&format!("{}{}", field.label, hint), row_area.width as usize),
            label_style,
        ));
        f.render_widget(
            Paragraph::new(label_line),
            Rect::new(row_area.x, row_area.y, row_area.width, 1),
        );
        let value_area = Rect::new(
            row_area.x,
            row_area.y + 1,
            row_area.width,
            row_area.height.saturating_sub(1),
        );

        match &field.kind {
            FieldKind::Choice { .. } => {
                let val_style = if is_focus {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(Color::White)
                };
                let text = format!("< {} >", field.display());
                f.render_widget(
                    Paragraph::new(Span::styled(
                        truncate(&text, value_area.width as usize),
                        val_style,
                    )),
                    Rect::new(value_area.x, value_area.y, value_area.width, 1),
                );
            }
            FieldKind::Text(ta) if field.multiline => {
                // Bug B fix: render the real multi-line content wrapped to the
                // reserved rows, instead of joining lines with a separator and
                // truncating to a single line.
                let mut text = ta.lines().join("\n");
                if is_focus {
                    text.push('▏');
                }
                let val_style = if is_focus {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let cursor_row = ta.cursor().0;
                let total_lines = ta.lines().len().max(1);
                let visible_rows = value_area.height.max(1) as usize;
                // Keep the cursor line visible: scroll so it stays within the
                // reserved rows once the content overflows them.
                let max_scroll = total_lines.saturating_sub(visible_rows);
                let scroll = cursor_row
                    .min(total_lines.saturating_sub(1))
                    .saturating_sub(visible_rows.saturating_sub(1))
                    .min(max_scroll);
                let p = Paragraph::new(Text::from(text))
                    .style(val_style)
                    .wrap(Wrap { trim: false })
                    .scroll((scroll as u16, 0));
                f.render_widget(p, value_area);
            }
            FieldKind::Text(ta) => {
                let mut t = ta.lines().join("  ⏎  ");
                if is_focus {
                    t.push('▏');
                }
                let val_style = if is_focus {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default().fg(Color::White)
                };
                f.render_widget(
                    Paragraph::new(Span::styled(
                        truncate(&t, value_area.width as usize),
                        val_style,
                    )),
                    Rect::new(value_area.x, value_area.y, value_area.width, 1),
                );
            }
        }
    }
}
