use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::forms::{FieldKind, Form};

use super::{centered_rect_abs, truncate};

// -- form --------------------------------------------------------------------

pub(super) fn draw_form(form: &Form, f: &mut Frame, area: Rect) {
    // Content-sized on large terminals, while still shrinking to small ones.
    let visible: Vec<usize> = (0..form.fields.len())
        .filter(|i| form.field_visible(*i))
        .collect();
    let content_h = visible
        .iter()
        .map(|i| if form.fields[*i].multiline { 4 } else { 2 })
        .sum::<u16>()
        .saturating_add(2);
    let box_area = centered_rect_abs(96, content_h, area);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(format!(
            " {} (Tab: field · Enter: save · Esc: cancel) ",
            form.title()
        ));
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    // Build per-field rows; multiline fields get more height.
    let constraints: Vec<Constraint> = visible
        .iter()
        .map(|i| {
            if form.fields[*i].multiline {
                Constraint::Length(4)
            } else {
                Constraint::Length(2)
            }
        })
        .collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (row_idx, &fi) in visible.iter().enumerate() {
        let field = &form.fields[fi];
        let is_focus = fi == form.focus;
        let label_style = if is_focus {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let area = rows[row_idx];
        let value_text = match &field.kind {
            FieldKind::Choice { .. } => format!("< {} >", field.display()),
            FieldKind::Text(ta) => {
                let mut t = ta.lines().join("  ⏎  ");
                if is_focus {
                    t.push('▏');
                }
                t
            }
        };
        let hint = if field.multiline {
            "  (Ctrl+E: $EDITOR)"
        } else {
            ""
        };
        let mut lines = vec![Line::from(Span::styled(
            format!("{}{}", field.label, hint),
            label_style,
        ))];
        let val_style = if is_focus {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            truncate(&value_text, area.width as usize),
            val_style,
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), area);
    }
}
