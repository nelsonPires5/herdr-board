//! Scope tests: archive, card move, drag, column reorder, shove.

use super::helpers::{demo_app, demo_app_with_detail, key};
use board_core::client::BoardClient;
use board_core::protocol::CardStatus;
use board_tui::app::{update, CardFilter, Effect, Screen};
use crossterm::event::KeyCode;

#[test]
fn archive_shortcut_archives_and_restores_selected_card() {
    let mut app = demo_app();
    let card_id = app.selected_card_id().unwrap();
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { id, archived: true }] if *id == card_id
    ));

    app.board
        .cards
        .iter_mut()
        .find(|card| card.id == card_id)
        .unwrap()
        .archived_at = Some("2026-07-14 12:00:00".into());
    app.card_filter = CardFilter::Archived;
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { id, archived: false }] if *id == card_id
    ));
}

#[test]
fn archived_card_must_be_restored_before_moving() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let done_idx = board
        .columns
        .iter()
        .position(|column| column.name == "Done")
        .unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.column_id == board.columns[done_idx].id)
        .unwrap();
    client.card_archive(card.id, true).unwrap();
    let mut app = board_tui::app::App::new(client.board_get().unwrap());
    app.card_filter = CardFilter::All;
    app.sel_col = done_idx;

    let effects = update(&mut app, key(KeyCode::Char('m')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::Board);
    assert!(app.toast.as_ref().is_some_and(|toast| {
        toast.is_error && toast.text.contains("restore archived card before moving")
    }));
}

#[test]
fn deleting_column_accounts_for_archived_cards_hidden_by_filter() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let done_idx = board
        .columns
        .iter()
        .position(|column| column.name == "Done")
        .unwrap();
    let card_ids: Vec<i64> = board
        .cards
        .iter()
        .filter(|card| card.column_id == board.columns[done_idx].id)
        .map(|card| card.id)
        .collect();
    for id in card_ids {
        client.card_archive(id, true).unwrap();
    }
    let mut app = board_tui::app::App::new(client.board_get().unwrap());
    app.sel_col = done_idx;
    assert!(app.cards_of(board.columns[done_idx].id).is_empty());

    update(&mut app, key(KeyCode::Char('D')));
    assert_eq!(app.screen, Screen::Picker);
}

#[test]
fn archive_shortcut_rejects_busy_card() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Right)); // Plan's running card
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|toast| {
        toast.is_error && toast.text.contains("cancel it before archiving")
    }));
}

#[test]
fn drag_card_to_other_column_produces_move() {
    let mut app = demo_app();
    // Grab the running card in Plan (column index 1).
    let plan_id = app.col_id_at(1).unwrap();
    let card_id = app.cards_of(plan_id)[0].id;
    app.begin_card_drag(card_id, 1);
    // Hover the same column -> no effect on finish.
    app.drag_hover(1);
    // Hover Execute (index 2) then drop.
    app.drag_hover(2);
    let effects = app.finish_drag();
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::CardMove(p) => {
            assert_eq!(p.id, card_id);
            assert_eq!(p.column_id, app.col_id_at(2).unwrap());
        }
        _ => panic!("expected CardMove"),
    }
    assert!(app.drag.is_none(), "drag cleared after finish");
}

#[test]
fn drag_dropped_on_origin_is_noop() {
    let mut app = demo_app();
    app.begin_card_drag(42, 1);
    app.drag_hover(1);
    let effects = app.finish_drag();
    assert!(effects.is_empty());
    assert!(app.drag.is_none());
}

#[test]
fn column_drag_produces_reorder() {
    let mut app = demo_app();
    let col_id = app.col_id_at(1).unwrap();
    app.begin_column_drag(col_id, 1);
    app.drag_hover(3);
    let effects = app.finish_drag();
    match &effects[0] {
        Effect::ColumnReorder { id, position } => {
            assert_eq!(*id, col_id);
            assert_eq!(*position, 3);
        }
        _ => panic!("expected ColumnReorder"),
    }
}

#[test]
fn shove_moves_card_and_focus() {
    let mut app = demo_app();
    // Focus Plan's running card.
    update(&mut app, key(KeyCode::Right));
    let card_id = app.selected_card_id().unwrap();
    let effects = update(&mut app, key(KeyCode::Char('L')));
    assert_eq!(app.sel_col, 2); // moved focus to Execute
    match &effects[0] {
        Effect::CardMove(p) => assert_eq!(p.id, card_id),
        _ => panic!("expected CardMove"),
    }
}

#[test]
fn archive_guard_blocks_awaiting_card_on_board_and_detail() {
    // Board screen: Review column (idx 3) has the failed card at 0 and the
    // awaiting card ("Tune retry backoff") at 1.
    let mut app = demo_app();
    app.sel_col = 3;
    app.sel_card = 1;
    assert_eq!(app.selected_card_status(), Some(CardStatus::Awaiting));
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|t| t.is_error));

    // Detail screen: same guard.
    let mut app = demo_app_with_detail(CardStatus::Awaiting);
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(effects.is_empty());
    assert!(app.toast.as_ref().is_some_and(|t| t.is_error));
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn done_card_is_final_and_can_be_archived() {
    // Done column (idx 5): "Ship v0.1" (idle) at 0, "Write changelog" (done) at 1.
    let mut app = demo_app();
    app.sel_col = 5;
    app.sel_card = 1;
    assert_eq!(app.selected_card_status(), Some(CardStatus::Done));
    let effects = update(&mut app, key(KeyCode::Char('a')));
    assert!(matches!(
        effects.as_slice(),
        [Effect::CardArchive { archived: true, .. }]
    ));
}
