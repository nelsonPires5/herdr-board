use super::detail::detail_section_title;
use super::{board_picker_label, pane_title, HELP_GUTTER_WIDTH, HELP_KEYS};
use crate::app::CardFilter;
use board_core::model::Board;

/// Full width of a help key row's key column: the 11-char padded key plus
/// the separating space (`{:<11} `). The layout-invariant tests below prove
/// descriptions never collide with the key column.
const HELP_KEY_WIDTH: u16 = 12;
/// Characters a help key label may occupy (the `{:<11}` pad).
const HELP_KEY_TEXT: usize = 11;

#[test]
fn pane_titles_include_scope_filter_and_sanitize_long_labels() {
    let global = Board {
        id: 1,
        name: "Global".into(),
        scope_path: None,
    };
    assert_eq!(
        pane_title(&global, CardFilter::Active),
        "Board [Global · ACTIVE]"
    );

    let scoped = Board {
        id: 2,
        name: "/tmp/repo".into(),
        scope_path: Some("/tmp/a[unsafe]/abcdefghijklmnopqrstuvwxyz0123456789".into()),
    };
    let title = pane_title(&scoped, CardFilter::Archived);
    assert!(title.starts_with("Board [abcdefghijklmnopqrstuvwxyz01234"));
    assert!(title.ends_with("… · ARCHIVED]"));
    assert!(!title.contains('[') || title.starts_with("Board ["));
    assert_eq!(
        board_picker_label(&scoped),
        "abcdefghijklmnopqrstuvwxyz01234… — /tmp/a(unsafe)/abcdefghijklmnopqrstuvwxyz0123456789"
    );
}

#[test]
fn detail_titles_show_only_overflow_arrows() {
    assert_eq!(detail_section_title("comments", 3, 0, 3), "comments");
    assert_eq!(detail_section_title("comments", 8, 0, 3), "comments ↓");
    assert_eq!(detail_section_title("comments", 8, 2, 3), "comments ↑↓");
    assert_eq!(detail_section_title("runs", 8, 5, 3), "runs ↑");
}

/// The key column is padded, not clipped, so an over-long label used to render
/// straight into its own description with no separating space (`q / Esc / any`
/// did exactly that in the two-column sheet). Descriptions were already pinned
/// below; keys were not, which is how it shipped.
#[test]
fn help_keys_fit_the_key_column() {
    for (_, key, description) in HELP_KEYS {
        if *key != "--" {
            assert!(
                key.chars().count() <= HELP_KEY_TEXT,
                "key {key:?} ({} chars) exceeds the {HELP_KEY_TEXT}-char key column \
                 and would collide with {description:?}",
                key.chars().count(),
            );
        }
    }
}

#[test]
fn help_descriptions_fit_each_80_column_panel_column() {
    let inner_width = 80_u16 - 2;
    let column_width = (inner_width - HELP_GUTTER_WIDTH) / 2;
    let description_width = column_width.saturating_sub(HELP_KEY_WIDTH);
    for (_, key, description) in HELP_KEYS {
        if *key != "--" {
            assert!(
                description.chars().count() <= description_width as usize,
                "{key} description does not fit: {description}"
            );
        }
    }
}

#[cfg(feature = "fake-client")]
#[test]
fn card_title_and_id_are_neutral_in_compact_and_desktop_cards() {
    use board_core::client::BoardClient;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::Terminal;

    let mut client = crate::testkit::demo_client().unwrap();
    let mut app = crate::app::App::new(client.board_get().unwrap());
    app.sel_col = 1; // Plan: the running card gives the status row a color.
    app.sel_card = 0;

    for (width, height, compact) in [(40_u16, 20_u16, true), (120, 35, false)] {
        let area = Rect::new(0, 0, width, height);
        app.last_area = area;
        let layout = super::board_layout(&app, area);
        let col = layout
            .cols
            .iter()
            .find(|col| col.idx == app.sel_col)
            .expect("selected column layout");
        let (_, card_rect) = col.cards.first().expect("running card layout");
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(*card_rect);
        let title_rows = if compact {
            inner.height.saturating_sub(3).max(1)
        } else {
            1
        };
        let status_y = inner.y + if compact { title_rows } else { 1 };

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| super::view(&app, f)).unwrap();
        let buffer = terminal.backend().buffer();

        for y in inner.y..inner.y.saturating_add(title_rows) {
            for x in inner.x..inner.right() {
                let cell = &buffer[(x, y)];
                if !cell.symbol().trim().is_empty() {
                    assert_eq!(
                        cell.fg,
                        Color::White,
                        "title/id cell at ({x},{y}) must be neutral at {width}x{height}"
                    );
                }
            }
        }

        let status = (inner.x..inner.right())
            .map(|x| &buffer[(x, status_y)])
            .find(|cell| cell.symbol() == "▶")
            .expect("running status glyph");
        assert_eq!(
            status.fg,
            Color::LightGreen,
            "semantic status color belongs on the status row"
        );
    }
}

#[cfg(feature = "fake-client")]
#[test]
fn reorder_mini_mode_keeps_selection_chrome_on_the_staged_card() {
    use crate::app::{App, Screen};
    use board_core::client::{BoardClient, FakeBoardClient};
    use board_core::protocol::{CardCreateParams, ColumnCreateParams, Trigger};
    use crossterm::event::KeyCode;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::Terminal;

    // Two idle cards in a manual column: idle borders are Gray, so the
    // LightCyan selection outline is unambiguous. (The demo board's queued
    // card already renders LightCyan, which would mask the assertion.)
    let mut client = FakeBoardClient::new().unwrap();
    let list = client
        .column_create(&ColumnCreateParams {
            name: "List".to_string(),
            trigger: Some(Trigger::Manual),
            ..Default::default()
        })
        .unwrap()
        .id;
    for title in ["first", "second"] {
        client
            .card_create(&CardCreateParams {
                title: title.to_string(),
                description: Some("fixture".to_string()),
                column_id: Some(list),
                harness: Some("claude".to_string()),
                ..Default::default()
            })
            .unwrap();
    }
    let mut app = App::new(client.board_get().unwrap());
    let area = Rect::new(0, 0, 120, 35);
    app.last_area = area;
    let layout = super::board_layout(&app, area);
    let col = layout
        .cols
        .iter()
        .find(|col| col.idx == 1)
        .expect("List column visible at 120x35");
    let (_, first) = col.cards[0];
    let (_, second) = col.cards[1];
    app.sel_col = 1;
    app.sel_card = 0;

    let border_fg = |app: &App, rect: Rect| -> Color {
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        terminal.draw(|f| super::view(app, f)).unwrap();
        terminal.backend().buffer()[(rect.x, rect.y + 1)].fg
    };

    // O enters the mini-mode; the card keeps its LightCyan selection outline
    // so the card being moved stays identifiable under the sheet.
    let effects = crate::app::update(&mut app, crate::testkit::key(KeyCode::Char('O')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::ReorderCard);
    assert_eq!(
        border_fg(&app, first),
        Color::LightCyan,
        "selected card outline must survive entering the reorder mini-mode"
    );

    // Staging down moves the outline to the staged card.
    let _ = crate::app::update(&mut app, crate::testkit::key(KeyCode::Char('j')));
    assert_ne!(
        border_fg(&app, first),
        Color::LightCyan,
        "unstaged card must not keep the selection outline"
    );
    assert_eq!(
        border_fg(&app, second),
        Color::LightCyan,
        "the staged card must carry the selection outline"
    );
}
