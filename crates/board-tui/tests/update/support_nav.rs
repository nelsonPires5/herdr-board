//! Support infrastructure and navigation tests.

use super::helpers::{demo_app, driver_of, key};
use board_core::client::BoardClient;
use board_tui::app::{update, App, CardFilter, Effect, Msg, Screen};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

#[test]
fn board_layout_always_fills_available_width() {
    let mut app = demo_app();

    let area = Rect::new(0, 0, 121, 35);
    let layout = board_tui::view::board_layout(&app, area);
    assert_eq!(layout.cols.first().unwrap().rect.x, area.x);
    let last = layout.cols.last().unwrap().rect;
    assert_eq!(last.x + last.width, area.x + area.width);

    // When not every column fits, the selected column drives a full-width
    // window rather than leaving a partial final page.
    app.sel_col = app.board.columns.len() - 1;
    let layout = board_tui::view::board_layout(&app, area);
    assert_eq!(layout.cols.last().unwrap().idx, app.sel_col);
    let last = layout.cols.last().unwrap().rect;
    assert_eq!(last.x + last.width, area.x + area.width);

    // A single-column board also consumes the entire viewport.
    let mut client = board_core::client::FakeBoardClient::new().unwrap();
    let single = App::new(client.board_get().unwrap());
    let layout = board_tui::view::board_layout(&single, area);
    assert_eq!(layout.cols[0].rect.width, area.width);
}

#[test]
fn board_picker_loads_and_switch_preserves_filter() {
    let mut app = demo_app();
    let effects = update(&mut app, key(KeyCode::Char('b')));
    assert!(matches!(effects.as_slice(), [Effect::LoadBoards]));

    let mut driver = driver_of(super::helpers::demo_client().unwrap());
    driver.handle(key(KeyCode::Char('v')));
    assert_eq!(driver.app.card_filter, CardFilter::All);
    driver.handle(key(KeyCode::Char('b')));
    assert_eq!(driver.app.screen, Screen::Picker);
    assert!(driver.app.picker.as_ref().unwrap().options.len() >= 3);
    driver.handle(key(KeyCode::Down));
    driver.handle(key(KeyCode::Enter));
    assert_eq!(driver.app.screen, Screen::Board);
    assert_eq!(driver.app.card_filter, CardFilter::All);
    assert_eq!(driver.app.sel_col, 0);
    assert_eq!(
        driver.app.board.board.scope_path.as_deref(),
        Some("/Volumes/archive/project")
    );
    driver.handle(Msg::Refresh);
    assert_eq!(
        driver.app.board.board.scope_path.as_deref(),
        Some("/Volumes/archive/project")
    );
}

#[test]
fn column_navigation_wraps() {
    let mut app = demo_app();
    let n = app.board.columns.len();
    assert!(n >= 2);
    assert_eq!(app.sel_col, 0);
    // left from first wraps to last
    update(&mut app, key(KeyCode::Left));
    assert_eq!(app.sel_col, n - 1);
    // right from last wraps to first
    update(&mut app, key(KeyCode::Right));
    assert_eq!(app.sel_col, 0);
    // hjkl aliases
    update(&mut app, key(KeyCode::Char('l')));
    assert_eq!(app.sel_col, 1);
    update(&mut app, key(KeyCode::Char('h')));
    assert_eq!(app.sel_col, 0);
}

#[test]
fn archive_filter_defaults_active_and_cycles_all_then_archived() {
    let mut client = super::helpers::demo_client().unwrap();
    let done = client
        .board_get()
        .unwrap()
        .columns
        .into_iter()
        .find(|column| column.name == "Done")
        .unwrap();
    let card_ids: Vec<i64> = client
        .card_list(Some(done.id))
        .unwrap()
        .iter()
        .map(|card| card.id)
        .collect();
    for id in card_ids {
        client.card_archive(id, true).unwrap();
    }
    let mut app = App::new(client.board_get().unwrap());

    assert_eq!(app.card_filter, CardFilter::Active);
    assert!(app.cards_of(done.id).is_empty());

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::All)]
    ));
    assert_eq!(app.card_filter, CardFilter::All);
    assert_eq!(app.cards_of(done.id).len(), 2);

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::Archived)]
    ));
    assert_eq!(app.card_filter, CardFilter::Archived);
    assert_eq!(app.cards_of(done.id).len(), 2);

    let effects = update(&mut app, key(KeyCode::Char('v')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::SetPaneTitle(CardFilter::Active)]
    ));
    assert_eq!(app.card_filter, CardFilter::Active);
}

#[test]
fn card_navigation_wraps_within_column() {
    let mut app = demo_app();
    // Move to Execute (index 2): it has 2 cards.
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right));
    let col_id = app.col_id_at(app.sel_col).unwrap();
    let count = app.cards_of(col_id).len();
    assert_eq!(count, 2);
    assert_eq!(app.sel_card, 0);
    update(&mut app, key(KeyCode::Up)); // wrap to last
    assert_eq!(app.sel_card, count - 1);
    update(&mut app, key(KeyCode::Down)); // wrap to first
    assert_eq!(app.sel_card, 0);
}

#[test]
fn switching_columns_clamps_card_index() {
    let mut app = demo_app();
    // Execute has 2 cards; select the 2nd.
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.sel_card, 1);
    // Move to Human Review (index 4) which is empty -> clamps to 0.
    update(&mut app, key(KeyCode::Right)); // Review
    update(&mut app, key(KeyCode::Right)); // Human Review
    assert_eq!(app.sel_card, 0);
}

#[test]
fn template_only_on_empty_board() {
    // Empty board -> T applies template.
    let mut c = board_core::client::FakeBoardClient::new().unwrap();
    let mut app = App::new(c.board_get().unwrap());
    assert!(app.is_empty_board());
    let effects = update(&mut app, key(KeyCode::Char('T')));
    assert!(matches!(effects.as_slice(), [Effect::TemplateApply(_)]));

    // Non-empty board -> T just toasts, no effect.
    let mut app2 = demo_app();
    let effects2 = update(&mut app2, key(KeyCode::Char('T')));
    assert!(effects2.is_empty());
    assert!(app2.toast.is_some());
}

#[test]
fn help_and_quit() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.screen, Screen::Board);
    let effects = update(&mut app, key(KeyCode::Char('q')));
    assert!(matches!(effects.as_slice(), [Effect::Quit]));
}

// -- Feature 2: `r` refresh --------------------------------------------------

#[test]
fn r_refreshes_board_with_toast() {
    let mut app = demo_app();
    let effects = update(&mut app, key(KeyCode::Char('r')));
    assert!(matches!(effects.as_slice(), [Effect::Refetch]));
    assert_eq!(app.toast.as_ref().unwrap().text, "refreshed");
    assert!(!app.toast.as_ref().unwrap().is_error);
}

#[test]
fn r_reloads_through_the_driver() {
    // The seeded board has a "Todo" card; deleting it out-of-band and then
    // pressing `r` should reload state from the client.
    let mut d = driver_of(super::helpers::demo_client().unwrap());
    let before = d.app.board.cards.len();
    let victim = d.app.board.cards[0].id;
    d.app.board.cards.retain(|c| c.id != victim); // desync local state
    assert_eq!(d.app.board.cards.len(), before - 1);
    d.handle(key(KeyCode::Char('r')));
    assert_eq!(d.app.board.cards.len(), before); // refetched from client
}
