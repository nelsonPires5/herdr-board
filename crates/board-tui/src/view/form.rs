use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
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
                    insert_cursor_marker(&mut s, cursor_col);
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
}
