//! Card detail tests: popup, fullscreen, scrolling, history, awaiting/done.

use super::helpers::{demo_app, demo_app_with_detail, driver_of, key};
use board_core::client::BoardClient;
use board_core::db::{EnqueueRun, FinalizeRun};
use board_core::protocol::{CardStatus, RunOutcome};
use board_tui::app::{update, DetailScrollTarget, Effect, Msg, Screen};
use board_tui::forms::FormKind;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

#[test]
fn card_detail_o_emits_focus_and_driver_quits_only_on_success() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let running = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Running)
        .unwrap()
        .clone();
    let mut app = board_tui::app::App::new(board);
    app.screen = Screen::CardDetail;
    app.detail = Some(client.card_get(running.id).unwrap());
    let effects = update(&mut app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(), [Effect::FocusRun(id)] if *id == running.id));

    let mut success = driver_of(super::helpers::demo_client().unwrap());
    success.set_origin_socket(Some("/tmp/herdr.sock".into()));
    success.handle(key(KeyCode::Right));
    success.handle(key(KeyCode::Enter));
    success.handle(key(KeyCode::Char('o')));
    assert!(success.app.should_quit);

    let mut error = driver_of(super::helpers::demo_client().unwrap());
    error.set_origin_socket(Some("/tmp/herdr.sock".into()));
    error.handle(key(KeyCode::Enter));
    error.handle(key(KeyCode::Char('o')));
    assert!(!error.app.should_quit);
    assert!(error.app.toast.as_ref().is_some_and(|toast| toast.is_error));

    let mut no_herdr = driver_of(super::helpers::demo_client().unwrap());
    no_herdr.set_origin_socket(None);
    no_herdr.handle(key(KeyCode::Right));
    no_herdr.handle(key(KeyCode::Enter));
    no_herdr.handle(key(KeyCode::Char('o')));
    assert!(!no_herdr.app.should_quit);
    assert!(no_herdr
        .app
        .toast
        .as_ref()
        .is_some_and(|toast| toast.text.contains("requires Herdr")));
}

#[test]
fn card_detail_toggles_popup_and_fullscreen() {
    let mut app = demo_app();
    app.screen = Screen::CardDetail;
    assert!(!app.detail_fullscreen);

    update(&mut app, key(KeyCode::Char('f')));
    assert!(app.detail_fullscreen);
    update(&mut app, key(KeyCode::Char('f')));
    assert!(!app.detail_fullscreen);
}

#[test]
fn card_detail_edit_opens_form_and_returns_to_detail() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card_id = board.cards[0].id;
    let detail = client.card_get(card_id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;

    let effects = update(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.screen, Screen::CardForm);
    assert!(matches!(
        app.form.as_ref().map(|form| form.kind),
        Some(FormKind::CardEdit { card_id: id }) if id == card_id
    ));
    assert!(matches!(effects.as_slice(), [Effect::LoadFormOptions]));

    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn card_detail_scrolls_comments_and_runs_independently() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("extra comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "claude",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        client
            .db()
            .promote_run_uow(run.id, None, None, None)
            .unwrap();
        client
            .db()
            .finalize_run_uow(&FinalizeRun {
                run_id: run.id,
                outcome: RunOutcome::Ok,
                summary: Some("done"),
                comments: &[],
                target_column_id: None,
                final_status: CardStatus::Done,
                final_awaiting_reason: None,
                next: None,
            })
            .unwrap();
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;

    update(&mut app, key(KeyCode::Down));
    assert!(app.detail_comments_scroll > 0);
    assert_eq!(app.detail_runs_scroll, 0);

    let comments_scroll = app.detail_comments_scroll;
    update(&mut app, key(KeyCode::Tab));
    assert_eq!(app.detail_scroll_target, DetailScrollTarget::Runs);
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.detail_comments_scroll, comments_scroll);
    assert!(app.detail_runs_scroll > 0);
}

#[test]
fn opening_detail_starts_comments_and_runs_at_latest() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "claude",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        client
            .db()
            .promote_run_uow(run.id, None, None, None)
            .unwrap();
        client
            .db()
            .finalize_run_uow(&FinalizeRun {
                run_id: run.id,
                outcome: RunOutcome::Ok,
                summary: Some("done"),
                comments: &[],
                target_column_id: None,
                final_status: CardStatus::Done,
                final_awaiting_reason: None,
                next: None,
            })
            .unwrap();
    }
    let mut driver = driver_of(client);
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Right));
    driver.handle(key(KeyCode::Enter));

    let detail = driver.app.detail.as_ref().unwrap();
    let layout = board_tui::view::detail_layout(&driver.app, driver.app.last_area);
    let comments_visible = layout.comments.height.saturating_sub(1) as usize;
    let runs_visible = layout.runs.height.saturating_sub(1) as usize;
    assert_eq!(
        driver.app.detail_comments_scroll + comments_visible,
        detail.comments.len()
    );
    assert_eq!(
        driver.app.detail_runs_scroll + runs_visible,
        detail.runs.len()
    );
    assert_eq!(
        detail.comments.last().unwrap().body,
        "comment 19",
        "comments remain oldest-to-newest"
    );
}

#[test]
fn shrinking_detail_to_popup_reanchors_history_to_latest() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..20 {
        client
            .comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
        let run = client
            .db()
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "claude",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        client
            .db()
            .promote_run_uow(run.id, None, None, None)
            .unwrap();
        client
            .db()
            .finalize_run_uow(&FinalizeRun {
                run_id: run.id,
                outcome: RunOutcome::Ok,
                summary: Some("done"),
                comments: &[],
                target_column_id: None,
                final_status: CardStatus::Done,
                final_awaiting_reason: None,
                next: None,
            })
            .unwrap();
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.last_area = Rect::new(0, 0, 254, 67);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_fullscreen = true;
    app.scroll_detail_to_latest();

    update(&mut app, key(KeyCode::Char('f')));

    let detail = app.detail.as_ref().unwrap();
    let layout = board_tui::view::detail_layout(&app, app.last_area);
    let comments_visible = layout.comments.height.saturating_sub(1) as usize;
    let runs_visible = layout.runs.height.saturating_sub(1) as usize;
    assert_eq!(
        app.detail_comments_scroll + comments_visible,
        detail.comments.len()
    );
    assert_eq!(app.detail_runs_scroll + runs_visible, detail.runs.len());
}

#[test]
fn card_detail_title_action_is_clickable() {
    let mut app = demo_app();
    app.screen = Screen::CardDetail;
    let button = board_tui::view::detail_toggle_rect(&app, app.last_area);

    update(
        &mut app,
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::empty(),
        }),
    );

    assert!(app.detail_fullscreen);
}

#[test]
fn enter_in_detail_confirms_awaiting_card_via_run_done() {
    let mut app = demo_app_with_detail(CardStatus::Awaiting);
    let card_id = app.detail.as_ref().unwrap().card.id;
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(
        matches!(effects.as_slice(), [Effect::RunDone(id, RunOutcome::Ok)] if *id == card_id),
        "Enter on an awaiting card must emit RunDone(ok) for that card"
    );
    // Stays on the detail screen; the driver reloads it after run.done.
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn enter_in_detail_is_noop_for_done_and_other_statuses() {
    for status in [
        CardStatus::Done,
        CardStatus::Running,
        CardStatus::Failed,
        CardStatus::Idle,
    ] {
        let mut app = demo_app_with_detail(status);
        assert!(
            update(&mut app, key(KeyCode::Enter)).is_empty(),
            "Enter must be a no-op for status {}",
            status.as_str()
        );
    }
}
