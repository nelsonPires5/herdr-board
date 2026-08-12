//! Scope tests: archive, card move, drag, column reorder, shove.

use super::helpers::{
    demo_app, demo_app_with_detail, demo_client, driver_of, key, RecordingClient,
};
use board_core::client::BoardClient;
use board_core::protocol::CardStatus;
use board_tui::app::{update, CardFilter, Effect, PickerPurpose, Screen};
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
fn move_column_m_opens_mode_and_enter_emits_single_reorder() {
    let mut app = demo_app();
    let plan_id = app.col_id_at(1).unwrap();
    update(&mut app, key(KeyCode::Right)); // focus Plan
                                           // Opening the mode is local-only (no effect, like open_move_picker).
    assert!(update(&mut app, key(KeyCode::Char('M'))).is_empty());
    assert_eq!(app.screen, Screen::MoveColumn);
    assert!(app.move_column.is_some());

    // ←/→ reorder locally without emitting anything.
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right)); // Plan now at index 3

    // Enter commits exactly one reorder at the column's current index.
    let effects = update(&mut app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::ColumnReorder { id, position }] => {
            assert_eq!(*id, plan_id);
            assert_eq!(*position, 3);
        }
        other => panic!("expected one ColumnReorder, got {} effects", other.len()),
    }
    assert_eq!(app.screen, Screen::Board);
    assert!(app.move_column.is_none());
}

#[test]
fn move_column_esc_restores_order_and_emits_nothing() {
    let mut app = demo_app();
    let original: Vec<i64> = app.board.columns.iter().map(|c| c.id).collect();
    update(&mut app, key(KeyCode::Right)); // focus Plan
    update(&mut app, key(KeyCode::Char('M')));
    update(&mut app, key(KeyCode::Right));
    update(&mut app, key(KeyCode::Right));
    // Mid-mode the *displayed* order is the staged one…
    let staged: Vec<i64> = (0..original.len())
        .map(|i| app.col_id_at(i).unwrap())
        .collect();
    assert_ne!(staged, original);
    let effects = update(&mut app, key(KeyCode::Esc));
    assert!(effects.is_empty(), "Esc must not persist anything");
    assert_eq!(app.screen, Screen::Board);
    // …and Esc drops the staging, so the displayed order is the snapshot again.
    let now: Vec<i64> = (0..original.len())
        .map(|i| app.col_id_at(i).unwrap())
        .collect();
    assert_eq!(now, original, "Esc must restore the original column order");
    assert_eq!(
        app.board.columns.iter().map(|c| c.id).collect::<Vec<_>>(),
        original
    );
}

/// A11: the staged reorder must live outside `app.board`, which a
/// `board_changed` refresh replaces wholesale. Staging it *in* the snapshot
/// meant a refresh tick landing mid-mode silently threw the user's order away.
#[test]
fn move_column_staged_order_survives_a_refresh_mid_mode() {
    let mut d = driver_of(demo_client().unwrap());
    let snapshot_order: Vec<i64> = d.app.board.columns.iter().map(|c| c.id).collect();
    let plan_id = snapshot_order[1];

    d.handle(key(KeyCode::Right)); // focus Plan
    d.handle(key(KeyCode::Char('M')));
    d.handle(key(KeyCode::Right));
    d.handle(key(KeyCode::Right)); // Plan staged at index 3
    let staged: Vec<i64> = (0..snapshot_order.len())
        .map(|i| d.app.col_id_at(i).unwrap())
        .collect();
    assert_eq!(staged[3], plan_id, "Plan is staged at index 3");

    // A refresh tick replaces the whole snapshot while the mode is still open.
    d.handle(board_tui::app::Msg::Refresh);
    assert_eq!(d.app.screen, Screen::MoveColumn);
    assert_eq!(
        d.app.board.columns.iter().map(|c| c.id).collect::<Vec<_>>(),
        snapshot_order,
        "the authoritative snapshot is never mutated by staging"
    );
    let after: Vec<i64> = (0..snapshot_order.len())
        .map(|i| d.app.col_id_at(i).unwrap())
        .collect();
    assert_eq!(after, staged, "the staged order survives the refresh");

    // …and Enter still commits the staged position, not the snapshot one.
    let effects = board_tui::app::update(&mut d.app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::ColumnReorder { id, position }] => {
            assert_eq!(*id, plan_id);
            assert_eq!(*position, 3);
        }
        other => panic!("expected one ColumnReorder, got {} effects", other.len()),
    }
}

#[test]
fn move_column_clamps_at_edges_without_wrapping() {
    let mut app = demo_app();
    let first_id = app.col_id_at(0).unwrap();
    update(&mut app, key(KeyCode::Char('M'))); // focus first column (Todo)
                                               // Moving left at the left edge is a no-op (no wraparound).
    assert!(update(&mut app, key(KeyCode::Left)).is_empty());
    assert!(update(&mut app, key(KeyCode::Char('h'))).is_empty());
    assert_eq!(app.col_id_at(0), Some(first_id));
    update(&mut app, key(KeyCode::Esc));
}

#[test]
fn move_column_mode_drives_a_single_column_reorder_rpc() {
    use std::sync::{Arc, Mutex};
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let client = RecordingClient {
        inner: demo_client().unwrap(),
        calls: calls.clone(),
    };
    let mut d = driver_of(client);
    let plan_id = d.app.board.columns[1].id;
    d.handle(key(KeyCode::Right)); // focus Plan
    d.handle(key(KeyCode::Char('M')));
    d.handle(key(KeyCode::Right));
    d.handle(key(KeyCode::Left)); // net-zero wander
    d.handle(key(KeyCode::Right));
    d.handle(key(KeyCode::Enter)); // commit
    let recorded = calls.lock().unwrap();
    let reorders = recorded
        .iter()
        .filter(|method| method.as_str() == "column.reorder")
        .count();
    assert_eq!(
        reorders, 1,
        "exactly one column.reorder on Enter: {recorded:?}"
    );
    drop(recorded);
    // The refetch after reorder reflects the persisted position.
    assert_eq!(
        d.app.board.columns.iter().position(|c| c.id == plan_id),
        Some(2)
    );
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

#[test]
fn move_shortcut_opens_active_board_column_picker() {
    let mut app = demo_app();
    let card_id = app.selected_card_id().unwrap();
    let active_board = app.board.board.id;
    let effects = update(&mut app, key(KeyCode::Char('m')));
    // `m` is the fast same-board path: it opens the active board's column
    // picker directly (no I/O effect). `b` inside it switches to cross-board.
    assert!(effects.is_empty(), "opening the picker is local-only");
    assert_eq!(app.screen, Screen::Picker);
    assert!(matches!(
        app.picker.as_ref().unwrap().purpose,
        PickerPurpose::MoveCardPickColumn { card_id: id, board_id }
        if id == card_id && board_id == active_board
    ));
}

// -- reorder-card mini-mode (O: j/k stage, Enter commits, Esc cancels) -------

/// Focus the Review column's first card (three cards there) the way a user
/// would: three rights then nothing (selection starts at Todo/0).
fn focus_review(app: &mut board_tui::app::App) -> i64 {
    update(app, key(KeyCode::Right));
    update(app, key(KeyCode::Right));
    update(app, key(KeyCode::Right));
    let id = app.selected_card_id().unwrap();
    assert_eq!(app.col_id_at(app.sel_col), Some(app.board.columns[3].id));
    id
}

#[test]
fn reorder_card_o_opens_mode_and_enter_emits_single_same_column_move() {
    let mut app = demo_app();
    let card_id = focus_review(&mut app);
    let column_id = app.col_id_at(3).unwrap();

    // Opening the mode is local-only.
    assert!(update(&mut app, key(KeyCode::Char('O'))).is_empty());
    assert_eq!(app.screen, Screen::ReorderCard);
    assert!(app.reorder_card.is_some());

    // j/k stage locally without emitting anything; the selection follows the
    // staged card (Review has two cards, so the last index is 1).
    update(&mut app, key(KeyCode::Char('j')));
    update(&mut app, key(KeyCode::Char('j')));
    assert_eq!(
        app.reorder_card.as_ref().unwrap().staged_index,
        1,
        "staging clamps at the column's last card (index 1)"
    );
    assert_eq!(app.sel_card, 1, "selection follows the staged card");
    assert_eq!(
        app.selected_card_id(),
        Some(card_id),
        "staged card stays selected"
    );

    // Enter commits exactly one same-column move carrying the position.
    let effects = update(&mut app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::CardMove(p)] => {
            assert_eq!(p.id, card_id);
            assert_eq!(p.column_id, column_id, "column must not change");
            assert_eq!(p.position, Some(1));
            assert_eq!(p.board_id, None, "intra-board reorder");
        }
        other => panic!("expected one CardMove, got {} effects", other.len()),
    }
    assert_eq!(app.screen, Screen::Board);
    assert!(app.reorder_card.is_none());
    // The selection stays on the reordered card, ready for the refetched
    // board to confirm it at `position`.
    assert_eq!(app.sel_card, 1);
}

#[test]
fn reorder_card_esc_restores_order_and_emits_nothing() {
    let mut app = demo_app();
    let card_id = focus_review(&mut app);
    let original: Vec<i64> = app
        .cards_of(app.col_id_at(3).unwrap())
        .iter()
        .map(|c| c.id)
        .collect();

    update(&mut app, key(KeyCode::Char('O')));
    update(&mut app, key(KeyCode::Char('j')));
    update(&mut app, key(KeyCode::Char('j')));
    // Mid-mode the *displayed* order is the staged one (two j presses clamp
    // to the last index 1)…
    let staged: Vec<i64> = app
        .cards_of(app.col_id_at(3).unwrap())
        .iter()
        .map(|c| c.id)
        .collect();
    assert_ne!(staged, original);

    let effects = update(&mut app, key(KeyCode::Esc));
    assert!(effects.is_empty(), "Esc must not persist anything");
    assert_eq!(app.screen, Screen::Board);
    assert!(app.reorder_card.is_none());
    // …and Esc drops the staging, so the displayed order is the snapshot again.
    let now: Vec<i64> = app
        .cards_of(app.col_id_at(3).unwrap())
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(now, original, "Esc must restore the original card order");
    assert_eq!(app.sel_card, 0, "selection returns to the original index");
    assert_eq!(app.selected_card_id(), Some(card_id));
    assert_eq!(app.sel_card, 0, "selection returns to the original index");
}

#[test]
fn reorder_card_clamps_at_edges_without_wrapping() {
    let mut app = demo_app();
    focus_review(&mut app);
    update(&mut app, key(KeyCode::Char('O')));
    // Staging up at the top is a no-op.
    update(&mut app, key(KeyCode::Up));
    assert_eq!(app.reorder_card.as_ref().unwrap().staged_index, 0);
    // Staging down past the bottom clamps at the last index (Review has two
    // cards).
    for _ in 0..5 {
        update(&mut app, key(KeyCode::Char('j')));
    }
    assert_eq!(app.reorder_card.as_ref().unwrap().staged_index, 1);
    // …and back up clamps at 0 again (no wraparound).
    for _ in 0..5 {
        update(&mut app, key(KeyCode::Char('k')));
    }
    assert_eq!(app.reorder_card.as_ref().unwrap().staged_index, 0);
}

#[test]
fn reorder_card_enter_without_staging_emits_nothing() {
    let mut app = demo_app();
    focus_review(&mut app);
    update(&mut app, key(KeyCode::Char('O')));
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(effects.is_empty(), "same-position Enter is a no-op");
    assert_eq!(app.screen, Screen::Board);
    assert!(app.reorder_card.is_none());
}

/// The staged position must live outside `app.board`, which a `board_changed`
/// refresh replaces wholesale — mirroring the move-column A11 contract.
#[test]
fn reorder_card_staged_order_survives_a_refresh_mid_mode() {
    let mut d = driver_of(demo_client().unwrap());
    let card_id = focus_review(&mut d.app);
    let column_id = d.app.col_id_at(3).unwrap();
    d.handle(key(KeyCode::Char('O')));
    d.handle(key(KeyCode::Char('j')));
    d.handle(key(KeyCode::Char('j')));
    let staged: Vec<i64> = d.app.cards_of(column_id).iter().map(|c| c.id).collect();
    assert_eq!(staged[1], card_id, "card staged at the last index (1)");

    // A refresh tick replaces the whole snapshot while the mode is still open.
    d.handle(board_tui::app::Msg::Refresh);
    assert_eq!(d.app.screen, Screen::ReorderCard);
    assert_eq!(d.app.reorder_card.as_ref().unwrap().staged_index, 1);
    let after: Vec<i64> = d.app.cards_of(column_id).iter().map(|c| c.id).collect();
    assert_eq!(after, staged, "the staged order survives the refresh");

    // …and Enter still commits the staged position, not the snapshot one.
    let effects = board_tui::app::update(&mut d.app, key(KeyCode::Enter));
    match effects.as_slice() {
        [Effect::CardMove(p)] => {
            assert_eq!(p.id, card_id);
            assert_eq!(p.position, Some(1));
        }
        other => panic!("expected one CardMove, got {} effects", other.len()),
    }
}

#[test]
fn reorder_card_rejects_archived_cards_like_other_moves() {
    let mut app = demo_app();
    let card_id = focus_review(&mut app);
    app.board
        .cards
        .iter_mut()
        .find(|card| card.id == card_id)
        .unwrap()
        .archived_at = Some("2026-07-14 12:00:00".into());
    app.card_filter = CardFilter::All;

    let effects = update(&mut app, key(KeyCode::Char('O')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::Board);
    assert!(app.reorder_card.is_none());
    assert!(app.toast.as_ref().is_some_and(|toast| {
        toast.is_error && toast.text.contains("restore archived card before moving")
    }));
}

// -- same-column drag reorder -------------------------------------------------

#[test]
fn drag_card_within_same_column_reorders_to_hovered_position() {
    let mut app = demo_app();
    let col = 3; // Review: two cards
    let column_id = app.col_id_at(col).unwrap();
    let card_id = app.cards_of(column_id)[0].id;
    app.begin_card_drag(card_id, col);
    // Hover the same column's last card, then drop.
    app.drag_hover_card(col, Some(1));
    let effects = app.finish_drag();
    match effects.as_slice() {
        [Effect::CardMove(p)] => {
            assert_eq!(p.id, card_id);
            assert_eq!(p.column_id, column_id, "same-column reorder");
            assert_eq!(p.position, Some(1));
        }
        other => panic!("expected one CardMove, got {} effects", other.len()),
    }
    assert!(app.drag.is_none(), "drag cleared after finish");
}

#[test]
fn drag_card_dropped_on_own_slot_is_noop() {
    let mut app = demo_app();
    let col = 3;
    let column_id = app.col_id_at(col).unwrap();
    let card_id = app.cards_of(column_id)[1].id;
    app.begin_card_drag(card_id, col);
    assert_eq!(app.drag.as_ref().unwrap().from_card, Some(1));
    app.drag_hover_card(col, Some(1));
    let effects = app.finish_drag();
    assert!(effects.is_empty(), "dropping on the origin slot is a no-op");
    assert!(app.drag.is_none());
}

#[test]
fn drag_card_to_other_column_still_moves_without_position() {
    let mut app = demo_app();
    let card_id = app.cards_of(app.col_id_at(1).unwrap())[0].id;
    app.begin_card_drag(card_id, 1);
    app.drag_hover_card(2, Some(1));
    let effects = app.finish_drag();
    match effects.as_slice() {
        [Effect::CardMove(p)] => {
            assert_eq!(p.column_id, app.col_id_at(2).unwrap());
            assert_eq!(p.position, None, "cross-column drags keep appending");
        }
        other => panic!("expected one CardMove, got {} effects", other.len()),
    }
}
