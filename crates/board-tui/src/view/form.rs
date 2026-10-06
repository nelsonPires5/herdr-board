use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::forms::Form;
use crate::widgets::{button_text, render_button_chip_at, render_sheet_frame, windowed_rows, Zone};

use super::{board_body_area_for, sheet_area_for_app, truncate, LayoutMode};

// -- form --------------------------------------------------------------------

// Caps a multiline field's value at 4 wrapped rows (+1 label row = 5 total,
// in both Regular/Wide and Compact) so one long description field can never
// starve the rest of the form — see `windowed_rows` for how the remaining
// fields scroll to keep the focused one in view instead of overflowing the
// sheet.
const MULTILINE_ROWS: u16 = 5;

/// Height (in rows, including its label row) a field occupies.
fn field_height(field: &crate::forms::Field, wrapped_editor: bool) -> u16 {
    if field.multiline {
        MULTILINE_ROWS.saturating_add(u16::from(wrapped_editor))
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
            F::MoveProject | F::MoveBoard | F::MoveColumn | F::MovePosition => "Move",
            F::ProjectPath => "Project",
            F::BoardName => "Board",
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
    // Each section is one complete bordered card: the border owns its title,
    // sides, and bottom edge, so no one-row header can float disconnected
    // above the fields it names.
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
    // A section card is its fields plus top/bottom border rows. These section
    // rows are the scroll units, keeping a card's geometry intact whenever it
    // fits in the viewport instead of slicing a border through a field.
    // Compact terminals below 55 columns wrap the editor affordance to its
    // own label row so the actual field label never ellipsizes.
    let wrapped_editor = mode == LayoutMode::Compact && area.width < 55;
    let heights: Vec<u16> = sections
        .iter()
        .map(|(_, idxs)| {
            idxs.iter()
                .map(|&fi| field_height(&form.fields[fi], wrapped_editor))
                .sum::<u16>()
                .saturating_add(2)
        })
        .collect();
    // Add the one-row action rail and the form frame's two border rows to the
    // preferred height; otherwise a fully fitting set of section cards would
    // still acquire a needless scrollbar one row short.
    let content_h = heights.iter().copied().sum::<u16>().saturating_add(3);

    // -- sheet placement ---------------------------------------------------------
    let box_area = if app.form_fullscreen {
        board_body_area_for(app, area)
    } else {
        sheet_area_for_app(app, mode, 96, content_h, area)
    };
    f.render_widget(Clear, box_area);

    let mut hit_map = app.hit_map.borrow_mut();
    let compact = mode == LayoutMode::Compact;
    // `f` is literal text inside text fields, so the popup/fullscreen toggle is
    // advertised (and bound) only while focus sits on a picker field.
    let toggle = form.focused_is_choice().then_some(if app.form_fullscreen {
        "f: popup"
    } else {
        "f: fullscreen"
    });
    let full_title = match toggle {
        Some(toggle) => format!(
            "{}  ·  {toggle}  ·  Tab: field · Enter: save · Esc: cancel",
            form.title()
        ),
        None => format!(
            "{}  ·  Tab: field · Enter: save · Esc: cancel",
            form.title()
        ),
    };
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

    // -- windowing over complete section cards -------------------------------
    let focus_row = sections
        .iter()
        .position(|(_, fields)| fields.contains(&form.focus))
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
    let rows = Layout::vertical(win_constraints).split(fields_area);

    // -- draw complete section cards ------------------------------------------
    for (row_idx, row) in rows.iter().copied().enumerate() {
        let abs_row = win_start + row_idx;
        let Some((title, fields)) = sections.get(abs_row) else {
            continue;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::Rgb(10, 20, 30)))
            .title(Span::styled(
                format!(" {title} "),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        let content = block.inner(row);
        f.render_widget(block, row);
        let field_constraints = fields
            .iter()
            .map(|&fi| Constraint::Length(field_height(&form.fields[fi], wrapped_editor)))
            .collect::<Vec<_>>();
        let field_rows = Layout::vertical(field_constraints).split(content);
        for (&fi, field_row) in fields.iter().zip(field_rows.iter().copied()) {
            draw_form_field(app, form, fi, field_row, f);
        }
    }
}

fn marker_width_for_label(label: &str) -> u16 {
    // `▌ ` (or two spaces) + the label + one separator before the editor chip.
    2u16.saturating_add(label.chars().count() as u16)
        .saturating_add(1)
}

/// Text fields share one value treatment regardless of whether their buffer is
/// one line or a wrapped textarea. In particular, a focused multiline editor
/// gets the same unmistakable reverse selection as the title/name fields, and
/// an unfocused buffer stays white/readable instead of falling back to the
/// section's dim gray.
fn text_field_value_style(is_focus: bool) -> Style {
    if is_focus {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(Color::White)
    }
}

fn insert_cursor_marker(value: &mut String, char_col: usize) {
    let byte = value
        .char_indices()
        .nth(char_col.min(value.chars().count()))
        .map(|(byte, _)| byte)
        .unwrap_or(value.len());
    value.insert(byte, '▏');
}

/// Measurement-only style tagging the real cursor marker inside the scratch
/// buffer. `Rgb(255, 0, 255)` never appears in form rendering, so a literal
/// `▏` typed by the user stays distinguishable from the inserted cursor.
fn cursor_measure_style() -> Style {
    Style::default().fg(Color::Rgb(255, 0, 255))
}

fn split_at_char(s: &str, char_col: usize) -> (String, String) {
    let col = char_col.min(s.chars().count());
    let byte = s.char_indices().nth(col).map(|(b, _)| b).unwrap_or(s.len());
    let (before, after) = s.split_at(byte);
    (before.to_owned(), after.to_owned())
}

/// Visual row of the cursor marker inside a single logical line, measured
/// with the identical `Paragraph` word wrapping (`Wrap { trim: false }`).
/// Renders only the cursor line into a scratch buffer so allocation stays
/// bounded; the full line (including text after the cursor) is rendered to
/// preserve word-wrap behavior after the cursor.
fn cursor_visual_offset_in_line(before: &str, after: &str, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    let tagged = Line::from(vec![
        Span::raw(before.to_owned()),
        Span::styled("▏", cursor_measure_style()),
        Span::raw(after.to_owned()),
    ]);
    let total = Paragraph::new(tagged.clone())
        .wrap(Wrap { trim: false })
        .line_count(width);
    if total == 0 {
        return 0;
    }
    let height_u16 = total.min(u16::MAX as usize) as u16;
    if height_u16 == 0 {
        return 0;
    }
    let area = Rect::new(0, 0, width, height_u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(tagged)
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);
    let target = Color::Rgb(255, 0, 255);
    for y in 0..height_u16 {
        for x in 0..width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "▏" && cell.fg == target {
                return y as usize;
            }
        }
    }
    // Fallback preserves prefix wrapping when the styled scan misses.
    let prefix = format!("{before}▏");
    Paragraph::new(Line::from(prefix))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .saturating_sub(1)
        .min(total.saturating_sub(1))
}

/// Visual scroll for the New Task description field only.
///
/// Measures with the same `Paragraph` wrapping the renderer uses (not
/// char-count/width arithmetic): total wrapped rows plus the marker's actual
/// visual row. Returns `min(cursor_row-(height-1), total-height)`, saturated
/// to `u16`, with early return for empty areas.
fn description_visual_scroll(
    lines: &[String],
    cursor: (usize, usize),
    width: u16,
    height: u16,
    show_marker: bool,
) -> u16 {
    if width == 0 || height == 0 || lines.is_empty() {
        return 0;
    }
    let cursor_row = cursor.0.min(lines.len().saturating_sub(1));
    let height_usize = height as usize;
    let mut total = 0usize;
    let mut rows_before = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let count = if show_marker && idx == cursor_row {
            let (before, after) = split_at_char(line, cursor.1);
            let marked = format!("{before}▏{after}");
            Paragraph::new(Line::from(marked))
                .wrap(Wrap { trim: false })
                .line_count(width)
        } else {
            Paragraph::new(Line::from(line.clone()))
                .wrap(Wrap { trim: false })
                .line_count(width)
        };
        // `line_count` returns at least 1 for `width > 0`; guard anyway so an
        // empty logical line still occupies one visual row.
        let count = count.max(1);
        total = total.saturating_add(count);
        if idx < cursor_row {
            rows_before = rows_before.saturating_add(count);
        }
    }
    let (before, after) = split_at_char(&lines[cursor_row], cursor.1);
    let within = cursor_visual_offset_in_line(&before, &after, width).min(total.saturating_sub(1));
    let cursor_visual_row = rows_before.saturating_add(within);
    cursor_visual_row
        .saturating_sub(height_usize.saturating_sub(1))
        .min(total.saturating_sub(height_usize))
        .min(u16::MAX as usize) as u16
}

/// Render one form field (label row + value row) inside `area`.
fn draw_form_field(app: &App, form: &Form, fi: usize, row_area: Rect, f: &mut Frame) {
    let field = &form.fields[fi];
    let is_focus = fi == form.focus;
    let wrap_editor = field.multiline
        && row_area.width
            < marker_width_for_label(field.label) + button_text("$EDITOR").chars().count() as u16;
    let label_style = if is_focus {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    app.hit_map.borrow_mut().push(row_area, Zone::FormField(fi));
    let marker = if is_focus { "▌ " } else { "  " };
    let editor = (!wrap_editor && field.multiline).then_some("[ $EDITOR ]");
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
        let trailing_w = button_text("$EDITOR").chars().count() as u16;
        let editor_x = if wrap_editor {
            row_area.right().saturating_sub(trailing_w)
        } else {
            let label_w = row_area.width.saturating_sub(marker_w + trailing_w + 1);
            let shown_label_w = truncate(field.label, label_w as usize).chars().count() as u16;
            row_area.x + marker_w + shown_label_w + 1
        };
        let editor_y = row_area.y + u16::from(wrap_editor);
        render_button_chip_at(
            f,
            Rect::new(editor_x, editor_y, trailing_w, 1),
            "$EDITOR",
            &mut app.hit_map.borrow_mut(),
            Zone::FormEditor(fi),
        );
    }
    let value_start = row_area.y + 1 + u16::from(wrap_editor);
    let value_area = Rect::new(
        row_area.x,
        value_start,
        row_area.width,
        row_area.height.saturating_sub(1 + u16::from(wrap_editor)),
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
            let arrow_w = button_text("‹").chars().count() as u16;
            let mut hm = app.hit_map.borrow_mut();
            render_button_chip_at(
                f,
                Rect::new(value_area.x, value_area.y, arrow_w.min(value_area.width), 1),
                "‹",
                &mut hm,
                Zone::FormChoicePrev(fi),
            );
            if shown_w >= arrow_w && shown_w <= value_area.width {
                render_button_chip_at(
                    f,
                    Rect::new(value_area.x + shown_w - arrow_w, value_area.y, arrow_w, 1),
                    "›",
                    &mut hm,
                    Zone::FormChoiceNext(fi),
                );
            }
        }
        crate::forms::FieldKind::Text(ta) if field.multiline => {
            // Keeps the cursor line visible and draws a live cursor marker at
            // the real cursor column so edits move visibly with the cursor.
            let val_style = text_field_value_style(is_focus);
            let cursor_row = ta.cursor().0.min(ta.lines().len().saturating_sub(1));
            let is_new_task_description =
                matches!(form.kind, crate::forms::FormKind::CardCreate { .. })
                    && field.id == crate::forms::FieldId::Description;
            let scroll: u16 = if is_new_task_description {
                description_visual_scroll(
                    ta.lines(),
                    ta.cursor(),
                    value_area.width,
                    value_area.height,
                    is_focus,
                )
            } else {
                let total_lines = ta.lines().len().max(1);
                let visible_rows = value_area.height.max(1) as usize;
                let max_scroll = total_lines.saturating_sub(visible_rows);
                cursor_row
                    .min(total_lines.saturating_sub(1))
                    .saturating_sub(visible_rows.saturating_sub(1))
                    .min(max_scroll) as u16
            };

            // Build rendered lines; mark the cursor character on its line.
            let cursor_col = ta.cursor().1;
            let mut rendered: Vec<Line> = Vec::new();
            for (li, line) in ta.lines().iter().enumerate() {
                let mut s = line.clone();
                if is_focus && li == cursor_row {
                    insert_cursor_marker(&mut s, cursor_col);
                }
                rendered.push(Line::from(s));
            }
            let p = Paragraph::new(Text::from(rendered))
                .style(val_style)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));
            f.render_widget(p, value_area);
        }
        crate::forms::FieldKind::Text(ta) => {
            // Single-line field: show the live cursor bar at the cursor column.
            let val_style = text_field_value_style(is_focus);
            let mut t = ta.lines().join("  ⏎  ");
            if is_focus {
                insert_cursor_marker(&mut t, ta.cursor().1);
            }
            f.render_widget(
                Paragraph::new(Span::styled(
                    truncate(&t, value_area.width as usize),
                    val_style,
                ))
                .style(val_style),
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
/// Thus `[ ‹ ]` and `[ › ]` never disappear even when the selected value is hostile.
fn choice_control(value: &str, width: usize) -> String {
    const OVERHEAD: usize = 14; // "[ ‹ ]  " + "  [ › ]"
    if width < OVERHEAD {
        // Real form inners are wider than this, but keep tiny test terminals
        // bounded rather than allowing the paragraph to spill.
        return truncate("[ ‹ ]  [ › ]", width);
    }
    let value = truncate(value, width - OVERHEAD);
    format!("[ ‹ ]  {value}  [ › ]")
}

/// Equal-width action rail. The existing semantic zones remain unchanged, so
/// keyboard and pointer submission still share the established reducer paths.
/// Only the exact `[ Save ]` / `[ Cancel ]` cells are painted as chips.
fn render_form_actions(f: &mut Frame, area: Rect, hit_map: &mut crate::widgets::HitMap) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let save_w = area.width / 2;
    let save = Rect::new(area.x, area.y, save_w, 1);
    let cancel = Rect::new(area.x + save_w, area.y, area.width - save_w, 1);
    render_button_chip_at(f, save, "Save", hit_map, Zone::BarSave);
    render_button_chip_at(f, cancel, "Cancel", hit_map, Zone::BarCancel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_value_uses_title_value_treatment_when_focused_or_not() {
        use board_core::model::Board;
        use board_core::protocol::BoardSnapshot;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        fn value_cell(
            field: crate::forms::FieldId,
            focus: crate::forms::FieldId,
        ) -> ratatui::buffer::Cell {
            let mut form = crate::forms::Form::card_create(1);
            let field_idx = form.fields.iter().position(|f| f.id == field).unwrap();
            form.focus = form.fields.iter().position(|f| f.id == focus).unwrap();
            let app = crate::app::App::new(BoardSnapshot {
                board: Board {
                    id: 1,
                    project_id: 1,
                    name: "Global".into(),
                    scope_path: None,
                    archived_at: None,
                },
                columns: Vec::new(),
                cards: Vec::new(),
                active_runs: Vec::new(),
            });
            let mut terminal = Terminal::new(TestBackend::new(50, 6)).unwrap();
            terminal
                .draw(|f| draw_form_field(&app, &form, field_idx, f.area(), f))
                .unwrap();
            terminal.backend().buffer()[(0, 1)].clone()
        }

        for focused in [true, false] {
            let title_focus = if focused {
                crate::forms::FieldId::Title
            } else {
                crate::forms::FieldId::Description
            };
            let description_focus = if focused {
                crate::forms::FieldId::Description
            } else {
                crate::forms::FieldId::Title
            };
            let title = value_cell(crate::forms::FieldId::Title, title_focus);
            let description = value_cell(crate::forms::FieldId::Description, description_focus);
            assert_eq!(
                title.modifier, description.modifier,
                "title and description focus modifiers must match when focused={focused}"
            );
            assert_eq!(title.fg, description.fg);
            assert_eq!(title.bg, description.bg);
        }
        assert!(
            value_cell(
                crate::forms::FieldId::Description,
                crate::forms::FieldId::Description
            )
            .modifier
            .contains(Modifier::REVERSED),
            "focused multiline values must retain the title/name reverse treatment"
        );
        assert_eq!(
            value_cell(
                crate::forms::FieldId::Description,
                crate::forms::FieldId::Title
            )
            .fg,
            Color::White,
            "unfocused multiline values must remain readable"
        );
    }

    #[test]
    fn column_system_prompt_matches_name_value_treatment_when_focused_or_not() {
        use board_core::model::Board;
        use board_core::protocol::BoardSnapshot;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        fn value_cell(
            field_id: crate::forms::FieldId,
            focus: crate::forms::FieldId,
        ) -> ratatui::buffer::Cell {
            let mut form = crate::forms::Form::column_create(&[]);
            let field = form
                .fields
                .iter()
                .position(|field| field.id == field_id)
                .unwrap();
            form.focus = form
                .fields
                .iter()
                .position(|field| field.id == focus)
                .unwrap();
            let app = crate::app::App::new(BoardSnapshot {
                board: Board {
                    id: 1,
                    project_id: 1,
                    name: "Global".into(),
                    scope_path: None,
                    archived_at: None,
                },
                columns: Vec::new(),
                cards: Vec::new(),
                active_runs: Vec::new(),
            });
            let mut terminal = Terminal::new(TestBackend::new(50, 6)).unwrap();
            terminal
                .draw(|f| draw_form_field(&app, &form, field, f.area(), f))
                .unwrap();
            terminal.backend().buffer()[(0, 1)].clone()
        }

        for focused in [true, false] {
            let name_focus = if focused {
                crate::forms::FieldId::Name
            } else {
                crate::forms::FieldId::SystemPrompt
            };
            let prompt_focus = if focused {
                crate::forms::FieldId::SystemPrompt
            } else {
                crate::forms::FieldId::Name
            };
            let name = value_cell(crate::forms::FieldId::Name, name_focus);
            let prompt = value_cell(crate::forms::FieldId::SystemPrompt, prompt_focus);
            assert_eq!(name.modifier, prompt.modifier);
            assert_eq!(name.fg, prompt.fg);
            assert_eq!(name.bg, prompt.bg);
        }
        assert!(
            value_cell(
                crate::forms::FieldId::SystemPrompt,
                crate::forms::FieldId::SystemPrompt
            )
            .modifier
            .contains(Modifier::REVERSED),
            "focused column system-prompt values must retain the name treatment"
        );
    }

    #[test]
    fn cursor_marker_uses_character_columns_for_multibyte_text() {
        let mut value = "Olá mundo".to_string();
        insert_cursor_marker(&mut value, 3);
        assert_eq!(value, "Olá▏ mundo");
    }

    #[test]
    fn choice_controls_survive_compact_width_and_only_data_ellipsizes() {
        assert_eq!(choice_control("manual", 38), "[ ‹ ]  manual  [ › ]");
        let hostile = choice_control(&"x".repeat(80), 16);
        assert_eq!(hostile, "[ ‹ ]  x…  [ › ]");
        assert!(hostile.contains("[ ‹ ]"));
        assert!(hostile.contains("[ › ]"));
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
            Some("[ $EDITOR ]"),
            39,
            Style::default(),
        );
        let rendered = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "▌ description (base prompt) [ $EDITOR ]");
        assert!(!rendered.contains('…'));
    }

    // -- New Task description visual-scroll regressions (issue #114) ---------
    mod description_scroll {
        use super::*;
        use board_core::model::Board;
        use board_core::protocol::BoardSnapshot;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use tui_textarea::{CursorMove, TextArea};

        fn test_app() -> crate::app::App {
            crate::app::App::new(BoardSnapshot {
                board: Board {
                    id: 1,
                    project_id: 1,
                    name: "Global".into(),
                    scope_path: None,
                    archived_at: None,
                },
                columns: Vec::new(),
                cards: Vec::new(),
                active_runs: Vec::new(),
            })
        }

        fn card_create_with(
            lines: Vec<String>,
            cursor: (u16, u16),
            focus_desc: bool,
        ) -> crate::forms::Form {
            let mut form = crate::forms::Form::card_create(1);
            let idx = form
                .fields
                .iter()
                .position(|f| f.id == crate::forms::FieldId::Description)
                .unwrap();
            if let crate::forms::FieldKind::Text(ta) = &mut form.fields[idx].kind {
                **ta = TextArea::new(lines);
                ta.move_cursor(CursorMove::Jump(cursor.0, cursor.1));
            }
            let title_idx = form
                .fields
                .iter()
                .position(|f| f.id == crate::forms::FieldId::Title)
                .unwrap();
            form.focus = if focus_desc { idx } else { title_idx };
            form
        }

        fn value_text(buf: &ratatui::buffer::Buffer, y0: u16, h: u16, w: u16) -> String {
            let mut out = String::new();
            for y in y0..y0 + h {
                for x in 0..w {
                    out.push_str(buf[(x, y)].symbol());
                }
                out.push('\n');
            }
            out
        }

        fn draw_and_snapshot(
            form: &crate::forms::Form,
            width: u16,
            row_h: u16,
        ) -> ratatui::buffer::Buffer {
            let app = test_app();
            let idx = form
                .fields
                .iter()
                .position(|f| f.id == crate::forms::FieldId::Description)
                .unwrap();
            let mut terminal = Terminal::new(TestBackend::new(width, row_h)).unwrap();
            terminal
                .draw(|f| {
                    draw_form_field(&app, form, idx, Rect::new(0, 0, width, row_h), f);
                })
                .unwrap();
            terminal.backend().buffer().clone()
        }

        fn has_marker(buf: &ratatui::buffer::Buffer, y0: u16, h: u16, w: u16) -> bool {
            for y in y0..y0 + h {
                for x in 0..w {
                    if buf[(x, y)].symbol() == "▏" {
                        return true;
                    }
                }
            }
            false
        }

        #[test]
        fn six_wrapped_lines_end_and_top() {
            let lines: Vec<String> = (1..=6)
                .map(|i| format!("linha{i}_{}", "x".repeat(50)))
                .collect();
            let last_len = lines[5].chars().count() as u16;
            // Cursor at end of linha6: marker + linha6 visible at bottom.
            let form = card_create_with(lines.clone(), (5, last_len), true);
            let before = form
                .fields
                .iter()
                .find(|f| f.id == crate::forms::FieldId::Description)
                .unwrap()
                .get_text();
            // width 40 avoids $EDITOR wrapping; row 5 => value 40x4.
            let buf = draw_and_snapshot(&form, 40, 5);
            let vt = value_text(&buf, 1, 4, 40);
            assert!(
                has_marker(&buf, 1, 4, 40),
                "cursor marker must stay visible at bottom"
            );
            assert!(
                vt.contains("linha6"),
                "linha6 must be visible at bottom, got:\n{vt}"
            );
            assert!(!vt.contains("linha1"), "linha1 should scroll out at bottom");
            let after = form
                .fields
                .iter()
                .find(|f| f.id == crate::forms::FieldId::Description)
                .unwrap()
                .get_text();
            assert_eq!(before, after);
            assert_eq!(after, lines.join("\n"));
            // Cursor at start: scroll 0, linha1 reachable at top.
            let form_top = card_create_with(lines.clone(), (0, 0), true);
            let buf_top = draw_and_snapshot(&form_top, 40, 5);
            let vt_top = value_text(&buf_top, 1, 4, 40);
            assert!(has_marker(&buf_top, 1, 4, 40));
            assert!(vt_top.contains("linha1"), "linha1 must be visible at top");
            assert_eq!(description_visual_scroll(&lines, (0, 0), 40, 4, true), 0);
            // End scroll equals total-height.
            let total: usize = lines
                .iter()
                .map(|l| {
                    Paragraph::new(Line::from(l.clone()))
                        .wrap(Wrap { trim: false })
                        .line_count(40)
                        .max(1)
                })
                .sum();
            // Cursor line gains a marker char; total with marker >= plain total.
            let scroll_end = description_visual_scroll(&lines, (5, last_len as usize), 40, 4, true);
            assert!(scroll_end > 0);
            assert!(usize::from(scroll_end) <= total.saturating_sub(4) + 2);
        }

        #[test]
        fn single_long_line_start_middle_end() {
            let line = "x".repeat(200);
            let lines = vec![line.clone()];
            assert_eq!(description_visual_scroll(&lines, (0, 0), 40, 4, true), 0);
            // Middle (row 2) stays visible at scroll 0; end (row 5) must scroll.
            let mid = description_visual_scroll(&lines, (0, 100), 40, 4, true);
            let end = description_visual_scroll(&lines, (0, 200), 40, 4, true);
            assert_eq!(mid, 0);
            assert!(end > mid, "end must scroll past middle");
            // Narrow width forces middle to scroll as well.
            let mid_narrow = description_visual_scroll(&lines, (0, 100), 20, 4, true);
            assert!(mid_narrow > 0);
            // Buffer: both middle and end keep marker visible, text unchanged.
            for col in [0u16, 100, 200] {
                let form = card_create_with(lines.clone(), (0, col), true);
                let buf = draw_and_snapshot(&form, 40, 5);
                assert!(has_marker(&buf, 1, 4, 40), "marker visible for col {col}");
                assert_eq!(
                    form.fields
                        .iter()
                        .find(|f| f.id == crate::forms::FieldId::Description)
                        .unwrap()
                        .get_text(),
                    line
                );
            }
        }

        #[test]
        fn cursor_up_down_keeps_visible() {
            let lines: Vec<String> = (1..=6)
                .map(|i| format!("row{i}_{}", "y".repeat(45)))
                .collect();
            // Simulate Down from top then Up from bottom via Jump (same as CursorMove).
            let form_down = card_create_with(lines.clone(), (2, 5), true);
            let buf = draw_and_snapshot(&form_down, 40, 5);
            assert!(has_marker(&buf, 1, 4, 40));
            // Real TextArea Up/Down path.
            let mut ta = TextArea::new(lines.clone());
            ta.move_cursor(CursorMove::Jump(0, 0));
            ta.move_cursor(CursorMove::Down);
            ta.move_cursor(CursorMove::Down);
            assert_eq!(ta.cursor().0, 2);
            let s = description_visual_scroll(ta.lines(), ta.cursor(), 40, 4, true);
            let total: usize = ta
                .lines()
                .iter()
                .map(|l| {
                    Paragraph::new(Line::from(l.clone()))
                        .wrap(Wrap { trim: false })
                        .line_count(40)
                        .max(1)
                })
                .sum();
            assert!(s + 4 > 2, "cursor visual row must be visible");
            assert!(usize::from(s) <= total.saturating_sub(4));
            ta.move_cursor(CursorMove::Up);
            assert_eq!(ta.cursor().0, 1);
            let s2 = description_visual_scroll(ta.lines(), ta.cursor(), 40, 4, true);
            assert!(s2 <= s);
        }

        #[test]
        fn short_lines_need_no_scroll() {
            let lines = vec!["a".to_string(), "b".to_string()];
            for cursor in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                assert_eq!(description_visual_scroll(&lines, cursor, 40, 4, true), 0);
                assert_eq!(description_visual_scroll(&lines, cursor, 40, 4, false), 0);
            }
            let form = card_create_with(lines.clone(), (1, 1), true);
            let buf = draw_and_snapshot(&form, 40, 5);
            assert!(has_marker(&buf, 1, 4, 40));
        }

        #[test]
        fn exact_width_marker_boundary() {
            // 10-char line at width 10: marker pushes to a second visual row.
            let lines = vec!["1234567890".to_string()];
            assert_eq!(description_visual_scroll(&lines, (0, 0), 10, 1, true), 0);
            assert_eq!(description_visual_scroll(&lines, (0, 10), 10, 1, true), 1);
            assert_eq!(description_visual_scroll(&lines, (0, 10), 10, 4, true), 0);
            let form = card_create_with(lines.clone(), (0, 10), true);
            let buf = draw_and_snapshot(&form, 40, 5);
            // Wide render keeps everything visible; narrow helper proves boundary.
            assert!(has_marker(&buf, 1, 4, 40));
        }

        #[test]
        fn spaces_use_word_wrapping_not_char_count() {
            // Word wrapping parity: helper must match Paragraph wrapping, not
            // naive width arithmetic. Assert against whole-paragraph count.
            let lines = vec!["abcde fghij".to_string()];
            let marked = "abcde fghij▏".to_string();
            let total = Paragraph::new(Line::from(marked))
                .wrap(Wrap { trim: false })
                .line_count(5)
                .max(1);
            assert_eq!(
                description_visual_scroll(&lines, (0, 11), 5, 1, true),
                (total.saturating_sub(1)) as u16
            );
            assert_eq!(description_visual_scroll(&lines, (0, 0), 5, 1, true), 0);
            // Buffer keeps marker visible for both edges.
            for col in [0u16, 11] {
                let form = card_create_with(lines.clone(), (0, col), true);
                let buf = draw_and_snapshot(&form, 40, 5);
                assert!(has_marker(&buf, 1, 4, 40));
            }
        }

        #[test]
        fn unicode_columns_and_resize() {
            let lines = vec![format!("Olá 🌍 {}", "ü".repeat(60))];
            let end_col = lines[0].chars().count();
            let narrow = description_visual_scroll(&lines, (0, end_col), 20, 4, true);
            let wide = description_visual_scroll(&lines, (0, end_col), 40, 4, true);
            assert!(narrow >= wide, "narrower width must scroll at least as far");
            for w in [20u16, 40] {
                let form = card_create_with(lines.clone(), (0, end_col as u16), true);
                let buf = draw_and_snapshot(&form, w.max(40), 5);
                assert!(has_marker(&buf, 1, 4, w.max(40)));
                assert_eq!(
                    form.fields
                        .iter()
                        .find(|f| f.id == crate::forms::FieldId::Description)
                        .unwrap()
                        .get_text(),
                    lines.join("\n")
                );
            }
            // Direct helper unicode visibility at narrow width.
            let s = description_visual_scroll(&lines, (0, end_col), 20, 4, true);
            let total = Paragraph::new(Line::from(lines[0].clone()))
                .wrap(Wrap { trim: false })
                .line_count(20)
                .max(1);
            assert!(usize::from(s) <= total.saturating_sub(4) + 1);
        }

        #[test]
        fn literal_marker_is_distinguished() {
            let lines = vec!["a▏b".to_string(), "c".to_string()];
            // Cursor before the literal marker; styled scan must find the real one.
            let s = description_visual_scroll(&lines, (0, 1), 40, 4, true);
            assert_eq!(s, 0);
            let form = card_create_with(lines.clone(), (0, 1), true);
            let buf = draw_and_snapshot(&form, 40, 5);
            // Both literal and cursor markers render; text model unchanged.
            let vt = value_text(&buf, 1, 4, 40);
            assert!(vt.contains('▏'));
            assert_eq!(
                form.fields
                    .iter()
                    .find(|f| f.id == crate::forms::FieldId::Description)
                    .unwrap()
                    .get_text(),
                "a▏b\nc"
            );
        }

        #[test]
        fn empty_and_zero_areas() {
            let lines = vec![String::new()];
            assert_eq!(description_visual_scroll(&lines, (0, 0), 40, 4, true), 0);
            assert_eq!(description_visual_scroll(&lines, (0, 0), 0, 4, true), 0);
            assert_eq!(description_visual_scroll(&lines, (0, 0), 40, 0, true), 0);
            assert_eq!(description_visual_scroll(&[], (0, 0), 40, 4, true), 0);
            let form = card_create_with(vec![String::new()], (0, 0), true);
            let buf = draw_and_snapshot(&form, 40, 5);
            assert!(has_marker(&buf, 1, 4, 40));
        }

        #[test]
        fn other_multiline_fields_preserved() {
            // Only CardCreate+Description uses the visual scroll; other
            // multiline fields keep the legacy logical path and full text.
            let long = "z".repeat(200);
            let visual =
                description_visual_scroll(std::slice::from_ref(&long), (0, 200), 40, 4, true);
            assert!(visual > 0);
            let mut col = crate::forms::Form::column_create(&[]);
            let idx = col
                .fields
                .iter()
                .position(|f| f.id == crate::forms::FieldId::SystemPrompt)
                .unwrap();
            if let crate::forms::FieldKind::Text(ta) = &mut col.fields[idx].kind {
                **ta = TextArea::new(vec![long.clone()]);
                ta.move_cursor(CursorMove::Jump(0, 200));
            }
            col.focus = idx;
            let app = test_app();
            let mut terminal = Terminal::new(TestBackend::new(50, 6)).unwrap();
            terminal
                .draw(|f| draw_form_field(&app, &col, idx, Rect::new(0, 0, 50, 6), f))
                .unwrap();
            assert_eq!(col.fields[idx].get_text(), long);
        }
    }
}
