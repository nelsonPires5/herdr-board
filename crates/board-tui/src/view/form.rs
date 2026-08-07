use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::forms::Form;
use crate::widgets::{render_sheet_frame, windowed_rows, Zone};

use super::{main_area, sheet_area, truncate, LayoutMode};

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

/// Group a field into a visual section. Sections render as bordered cards with
/// the section name as their title, matching the requested form composition.
fn field_section(id: crate::forms::FieldId, is_column: bool) -> &'static str {
    use crate::forms::FieldId as F;
    if is_column {
        match id {
            F::Name | F::Trigger | F::SystemPrompt => "Definition",
            F::OnSuccess | F::OnFail | F::FreshSession => "Automation",
            _ => "Overrides",
        }
    } else {
        match id {
            F::Title | F::Description => "Task",
            F::Harness | F::Model | F::ModelCustom | F::Effort | F::Permission => "Agent",
            _ => "Execution Target",
        }
    }
}

pub(super) fn draw_form(app: &App, form: &Form, f: &mut Frame, area: Rect) {
    let mode = app.layout_mode();

    // -- model -----------------------------------------------------------------
    let visible: Vec<usize> = (0..form.fields.len())
        .filter(|i| form.field_visible(*i))
        .collect();
    let is_column = form.is_column_form();
    let is_comment = matches!(
        form.kind,
        crate::forms::FormKind::Comment { .. } | crate::forms::FormKind::CommentEdit { .. }
    );

    // Build sections over *visible* field indices, preserving field order.
    // Each section contributes a 1-row section-header card plus its fields.
    let mut sections: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for &fi in &visible {
        let sec = if is_comment {
            "Comment"
        } else {
            field_section(form.fields[fi].id, is_column)
        };
        match sections.last_mut() {
            Some((name, idxs)) if *name == sec => idxs.push(fi),
            _ => sections.push((sec, vec![fi])),
        }
    }

    // Combined layout: for each section, [header(1)] + field heights + 1 gap.
    let mut heights: Vec<u16> = Vec::new();
    // (section header: name+index | field index), parallel to `heights`
    type RowMeta = (Option<(&'static str, usize)>, Option<usize>);
    let mut row_meta: Vec<RowMeta> = Vec::new();
    // row_meta entries: (some((section_title, section_index)) header,
    //                    some(field_index) field)
    for (si, (sec, idxs)) in sections.iter().enumerate() {
        heights.push(1); // section header card
        row_meta.push((Some((*sec, si)), None));
        for &fi in idxs {
            heights.push(field_height(&form.fields[fi]));
            row_meta.push((None, Some(fi)));
        }
        // Section cards' own borders provide the visual separation; no extra
        // gap row, so grouped forms keep fitting the same sizes as flat forms.
        let _ = si;
    }
    let content_h = heights.iter().copied().sum::<u16>().saturating_add(4); // + button bar + margins

    // -- sheet placement ---------------------------------------------------------
    let box_area = if app.form_fullscreen {
        let base = main_area(area);
        Rect::new(base.x, base.y, base.width, base.height)
    } else {
        sheet_area(mode, 96, content_h, area)
    };
    f.render_widget(Clear, box_area);

    let mut hit_map = app.hit_map.borrow_mut();
    let compact = mode == LayoutMode::Compact;
    let toggle = if app.form_fullscreen {
        "f: popup"
    } else {
        "f: fullscreen"
    };
    let full_title = format!(
        "{}  ·  {toggle}  ·  Tab: field · Enter: save · Esc: cancel",
        form.title()
    );
    let inner = render_sheet_frame(
        f,
        box_area,
        compact,
        &full_title,
        form.title(),
        Style::default().fg(Color::LightBlue),
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
    render_form_actions(f, bar_area, &mut hit_map);

    // -- windowing over the combined (section-header + field) layout ------------
    let focus_row = visible
        .iter()
        .position(|&i| i == form.focus)
        .map(|_| {
            // map the focused field to its combined row index
            row_meta
                .iter()
                .position(|(_, fld)| *fld == Some(form.focus))
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let (win_start, win_end) = windowed_rows(&heights, focus_row, fields_area.height);
    let overflowing = win_end - win_start < heights.len();

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
        crate::widgets::vertical_scrollbar(
            f,
            sb_rect,
            heights.len(),
            win_start,
            win_end - win_start,
        );
    }
    drop(hit_map);

    let win_constraints: Vec<Constraint> = heights[win_start..win_end]
        .iter()
        .map(|h| Constraint::Length(*h))
        .collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(win_constraints)
        .split(fields_area);

    // -- draw rows ---------------------------------------------------------------
    // We track the current section's combined bounds so each section can be
    // rendered as one bordered card afterwards.

    for (row_idx, row) in rows.iter().copied().enumerate() {
        let abs_row = win_start + row_idx;
        match row_meta.get(abs_row) {
            Some((Some((title, si)), _)) => {
                // Section header: a thin clickable card with the section title.
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .style(Style::default().bg(Color::Rgb(10, 20, 30)));
                let inner_h = block.inner(row);
                f.render_widget(block, row);
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  {title} "),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    inner_h,
                );
                app.hit_map.borrow_mut().push(row, Zone::Shield);
                let _ = si;
            }
            Some((None, Some(fi))) => {
                draw_form_field(app, form, *fi, row, f);
            }
            _ => {}
        }
    }
}

/// Render one form field (label row + value row) inside `area`.
fn draw_form_field(app: &App, form: &Form, fi: usize, row_area: Rect, f: &mut Frame) {
    let field = &form.fields[fi];
    let is_focus = fi == form.focus;
    let label_style = if is_focus {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    app.hit_map.borrow_mut().push(row_area, Zone::FormField(fi));
    let marker = if is_focus { "▌ " } else { "  " };
    let editor = field.multiline.then_some("[$EDITOR]");
    let label_line = form_label_line(
        marker,
        field.label,
        editor,
        row_area.width as usize,
        label_style,
    );
    f.render_widget(
        Paragraph::new(label_line),
        Rect::new(row_area.x, row_area.y, row_area.width, 1),
    );
    if field.multiline {
        let marker_w = marker.chars().count() as u16;
        let trailing_w = "[$EDITOR]".chars().count() as u16;
        let label_w = row_area.width.saturating_sub(marker_w + trailing_w + 1);
        let shown_label_w = truncate(field.label, label_w as usize).chars().count() as u16;
        let editor_x = row_area.x + marker_w + shown_label_w + 1;
        app.hit_map.borrow_mut().push(
            Rect::new(editor_x, row_area.y, trailing_w, 1),
            Zone::FormEditor(fi),
        );
    }
    let value_area = Rect::new(
        row_area.x,
        row_area.y + 1,
        row_area.width,
        row_area.height.saturating_sub(1),
    );

    match &field.kind {
        crate::forms::FieldKind::Choice { .. } => {
            let val_style = if is_focus {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let text = choice_control(&field.display(), value_area.width as usize);
            let shown_w = text.chars().count() as u16;
            f.render_widget(
                Paragraph::new(Span::styled(text, val_style)),
                Rect::new(value_area.x, value_area.y, value_area.width, 1),
            );
            let mut hm = app.hit_map.borrow_mut();
            hm.push(
                Rect::new(value_area.x, value_area.y, 3.min(value_area.width), 1),
                Zone::FormChoicePrev(fi),
            );
            if shown_w >= 3 && shown_w <= value_area.width {
                hm.push(
                    Rect::new(value_area.x + shown_w - 3, value_area.y, 3, 1),
                    Zone::FormChoiceNext(fi),
                );
            }
        }
        crate::forms::FieldKind::Text(ta) if field.multiline => {
            // Keeps the cursor line visible and draws a live cursor marker at
            // the real cursor column so edits move visibly with the cursor.
            let val_style = if is_focus {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            let cursor_row = ta.cursor().0.min(ta.lines().len().saturating_sub(1));
            let total_lines = ta.lines().len().max(1);
            let visible_rows = value_area.height.max(1) as usize;
            let max_scroll = total_lines.saturating_sub(visible_rows);
            let scroll = cursor_row
                .min(total_lines.saturating_sub(1))
                .saturating_sub(visible_rows.saturating_sub(1))
                .min(max_scroll);

            // Build rendered lines; mark the cursor character on its line.
            let cursor_col = ta.cursor().1;
            let mut rendered: Vec<Line> = Vec::new();
            for (li, line) in ta.lines().iter().enumerate() {
                let mut s = line.clone();
                if is_focus && li == cursor_row {
                    let col = cursor_col.min(s.chars().count() + 1);
                    s.insert(col.min(s.len()), '▏');
                }
                rendered.push(Line::from(s));
            }
            let p = Paragraph::new(Text::from(rendered))
                .style(val_style)
                .wrap(Wrap { trim: false })
                .scroll((scroll as u16, 0));
            f.render_widget(p, value_area);
        }
        crate::forms::FieldKind::Text(ta) => {
            // Single-line field: show the live cursor bar at the cursor column.
            let val_style = if is_focus {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(Color::White)
            };
            let mut t = ta.lines().join("  ⏎  ");
            if is_focus {
                let col = ta.cursor().1.min(t.chars().count() + 1);
                t.insert(col, '▏');
            }
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
/// Keep labels and the multiline editor affordance complete at supported
/// widths. Only hostile/dynamic labels are eligible for ellipsis.
fn form_label_line<'a>(
    marker: &'a str,
    label: &str,
    trailing: Option<&'a str>,
    width: usize,
    style: Style,
) -> Line<'a> {
    let marker_w = marker.chars().count();
    let trailing_w = trailing.map_or(0, |s| s.chars().count() + 1);
    let label_w = width.saturating_sub(marker_w + trailing_w);
    let label = truncate(label, label_w);
    let mut spans = vec![Span::styled(marker, style), Span::styled(label, style)];
    if let Some(trailing) = trailing {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            trailing,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// Render both choice controls before allocating the remaining cells to data.
/// Thus `[‹]` and `[›]` never disappear even when the selected value is hostile.
fn choice_control(value: &str, width: usize) -> String {
    const OVERHEAD: usize = 10; // "[‹]  " + "  [›]"
    if width < OVERHEAD {
        // Real form inners are wider than this, but keep tiny test terminals
        // bounded rather than allowing the paragraph to spill.
        return truncate("[‹]  [›]", width);
    }
    let value = truncate(value, width - OVERHEAD);
    format!("[‹]  {value}  [›]")
}

/// Equal-width action rail. The existing semantic zones remain unchanged, so
/// keyboard and pointer submission still share the established reducer paths.
fn render_form_actions(f: &mut Frame, area: Rect, hit_map: &mut crate::widgets::HitMap) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let save_w = area.width / 2;
    let save = Rect::new(area.x, area.y, save_w, 1);
    let cancel = Rect::new(area.x + save_w, area.y, area.width - save_w, 1);

    f.render_widget(
        Paragraph::new("[ Save ]")
            .alignment(ratatui::layout::Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ),
        save,
    );
    hit_map.push(save, Zone::BarSave);

    f.render_widget(
        Paragraph::new("[ Cancel ]")
            .alignment(ratatui::layout::Alignment::Center)
            .style(
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ),
        cancel,
    );
    hit_map.push(cancel, Zone::BarCancel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_controls_survive_compact_width_and_only_data_ellipsizes() {
        assert_eq!(choice_control("manual", 38), "[‹]  manual  [›]");
        let hostile = choice_control(&"x".repeat(80), 12);
        assert_eq!(hostile, "[‹]  x…  [›]");
        assert!(hostile.contains("[‹]"));
        assert!(hostile.contains("[›]"));
    }

    #[test]
    fn action_rail_uses_full_equal_width_existing_zones() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        for width in [38, 50, 58, 78, 94] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
            let mut hit_map = crate::widgets::HitMap::default();
            terminal
                .draw(|f| render_form_actions(f, f.area(), &mut hit_map))
                .unwrap();

            assert_eq!(hit_map.hit(0, 0), Some(Zone::BarSave));
            assert_eq!(hit_map.hit(width / 2 - 1, 0), Some(Zone::BarSave));
            assert_eq!(hit_map.hit(width / 2, 0), Some(Zone::BarCancel));
            assert_eq!(hit_map.hit(width - 1, 0), Some(Zone::BarCancel));

            let rendered = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("[ Save ]"));
            assert!(rendered.contains("[ Cancel ]"));
        }
    }

    #[test]
    fn multiline_label_reserves_complete_editor_affordance() {
        let line = form_label_line(
            "▌ ",
            "description (base prompt)",
            Some("[$EDITOR]"),
            38,
            Style::default(),
        );
        let rendered = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "▌ description (base prompt) [$EDITOR]");
        assert!(!rendered.contains('…'));
    }
}
