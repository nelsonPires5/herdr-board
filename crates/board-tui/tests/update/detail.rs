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
fn detail_comment_scroll_clamps_to_wrapped_rows() {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    // Several long comments that each word-wrap across multiple rows at the
    // popup width, so the comments section scrolls by row, not by comment.
    for i in 0..6 {
        client
            .comment_add(
                card.id,
                &format!(
                    "Long comment number {i} with enough words to force wrapping \
                     across at least a couple of rendered rows inside the comments \
                     section at the popup width."
                ),
                Some("reviewer"),
            )
            .unwrap();
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.last_area = Rect::new(0, 0, 80, 24);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.scroll_detail_to_latest();

    let d = app.detail.as_ref().unwrap();
    let layout = board_tui::view::detail_layout(&app, app.last_area);
    let total = board_tui::view::comment_wrapped_rows(d, layout.comments.width);
    let comment_count = d.comments.len();
    let (_, visible) = board_tui::view::comments_viewport(&app, &layout);
    assert!(
        app.detail_comments_scroll + visible <= total,
        "scroll {} + visible {} must not exceed wrapped rows {} (no blank overflow)",
        app.detail_comments_scroll,
        visible,
        total
    );

    // Driving comment focus far past the end clamps the selection at the
    // last comment and, since `follow_comment_focus` for the last comment
    // converges on the same bottom anchor as `scroll_detail_to_latest`, the
    // scroll offset ends back at the latest anchor too.
    let latest = app.detail_comments_scroll;
    for _ in 0..50 {
        update(&mut app, key(KeyCode::Down));
    }
    assert_eq!(app.detail_comment_sel, comment_count - 1);
    assert_eq!(app.detail_comments_scroll, latest);
}

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

    // Comments focused: `Down` moves the comment focus, not a row scroll.
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.detail_comment_sel, 1);
    assert_eq!(app.detail_runs_scroll, 0);

    let comment_sel = app.detail_comment_sel;
    update(&mut app, key(KeyCode::Tab));
    assert_eq!(app.detail_scroll_target, DetailScrollTarget::Runs);
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.detail_comment_sel, comment_sel);
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
    let (_, comments_visible) = board_tui::view::comments_viewport(&driver.app, &layout);
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
    let (_, comments_visible) = board_tui::view::comments_viewport(&app, &layout);
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

// -- comment focus / edit / delete / history --------------------------------

/// A `CardDetail` with exactly `n` fresh comments, comments-focused, selection
/// at `0`.
fn app_with_n_comments(n: usize) -> (board_tui::app::App, board_core::model::Comment) {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    // A freshly created card starts with zero comments (unlike the seeded
    // demo cards), so the `n` comments added below are the only ones.
    let column_id = board.columns[0].id;
    let card = client
        .card_create(&board_core::protocol::CardCreateParams {
            title: "comment focus fixture".into(),
            column_id: Some(column_id),
            harness: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();
    let mut first = None;
    for i in 0..n {
        let c = client
            .comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
        if first.is_none() {
            first = Some(c);
        }
    }
    let detail = client.card_get(card.id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_scroll_target = DetailScrollTarget::Comments;
    app.detail_comment_sel = 0;
    (app, first.expect("at least one comment"))
}

#[test]
fn comment_focus_clamps_at_both_ends_without_wrapping() {
    let (mut app, _first) = app_with_n_comments(3);

    // Already at the top: Up must not wrap to the last index.
    update(&mut app, key(KeyCode::Up));
    assert_eq!(app.detail_comment_sel, 0);

    // Down repeatedly clamps at the last index (2), no wrap past it.
    for _ in 0..10 {
        update(&mut app, key(KeyCode::Down));
    }
    assert_eq!(app.detail_comment_sel, 2);

    // Up repeatedly clamps back at 0, no wrap past it.
    for _ in 0..10 {
        update(&mut app, key(KeyCode::Up));
    }
    assert_eq!(app.detail_comment_sel, 0);
}

#[test]
fn e_edits_the_focused_comment_when_comments_focused() {
    let (mut app, first) = app_with_n_comments(2);

    let effects = update(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.screen, Screen::CardForm);
    assert!(
        effects.is_empty(),
        "editing a comment loads no form options"
    );
    assert!(matches!(
        app.form.as_ref().map(|f| f.kind),
        Some(FormKind::CommentEdit { comment_id }) if comment_id == first.id
    ));
    assert_eq!(
        app.form.as_ref().unwrap().fields[0].get_text(),
        first.body,
        "the edit form pre-fills the focused comment's body"
    );
}

#[test]
fn e_edits_the_card_when_runs_focused() {
    let (mut app, _first) = app_with_n_comments(2);
    let card_id = app.detail.as_ref().unwrap().card.id;
    app.detail_scroll_target = DetailScrollTarget::Runs;

    let effects = update(&mut app, key(KeyCode::Char('e')));
    assert_eq!(app.screen, Screen::CardForm);
    assert!(matches!(
        app.form.as_ref().map(|f| f.kind),
        Some(FormKind::CardEdit { card_id: id }) if id == card_id
    ));
    assert!(matches!(effects.as_slice(), [Effect::LoadFormOptions]));
}

#[test]
fn d_confirms_comment_delete_and_y_deletes_n_cancels_both_return_to_detail() {
    let (mut app, first) = app_with_n_comments(1);

    update(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.screen, Screen::Confirm);
    let effects = update(&mut app, key(KeyCode::Char('y')));
    assert!(
        matches!(effects.as_slice(), [Effect::CommentDelete { id }] if *id == first.id),
        "confirming must emit CommentDelete for the focused comment"
    );
    assert_eq!(app.screen, Screen::CardDetail);

    update(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.screen, Screen::Confirm);
    let effects = update(&mut app, key(KeyCode::Char('n')));
    assert!(effects.is_empty(), "declining must emit no effect");
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn h_emits_load_comment_history_for_the_focused_comment() {
    let (mut app, first) = app_with_n_comments(1);
    let effects = update(&mut app, key(KeyCode::Char('h')));
    assert!(matches!(effects.as_slice(), [Effect::LoadCommentHistory { id }] if *id == first.id));
}

#[test]
fn d_and_h_are_noops_with_no_comments() {
    let mut app = demo_app_with_detail(CardStatus::Failed);
    app.detail.as_mut().unwrap().comments.clear();
    app.detail_scroll_target = DetailScrollTarget::Comments;

    assert!(update(&mut app, key(KeyCode::Char('d'))).is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
    assert!(update(&mut app, key(KeyCode::Char('h'))).is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
}

// -- system comments are immutable: e/d toast instead of acting -------------

/// Like `app_with_n_comments`, but the single comment is authored `"system"`
/// (`Db::update_comment`/`soft_delete_comment` reject it; `docs/protocol.md`).
fn app_with_system_comment() -> (board_tui::app::App, board_core::model::Comment) {
    let mut client = super::helpers::demo_client().unwrap();
    let board = client.board_get().unwrap();
    let column_id = board.columns[0].id;
    let card = client
        .card_create(&board_core::protocol::CardCreateParams {
            title: "system comment fixture".into(),
            column_id: Some(column_id),
            harness: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();
    let comment = client
        .comment_add(card.id, "auto-generated note", Some("system"))
        .unwrap();
    let detail = client.card_get(card.id).unwrap();
    let mut app = board_tui::app::App::new(board);
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_scroll_target = DetailScrollTarget::Comments;
    app.detail_comment_sel = 0;
    (app, comment)
}

#[test]
fn e_on_a_system_comment_toasts_and_opens_no_form() {
    let (mut app, _comment) = app_with_system_comment();
    let effects = update(&mut app, key(KeyCode::Char('e')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
    assert!(app.form.is_none());
    assert!(app
        .toast
        .as_ref()
        .is_some_and(|t| t.is_error && t.text.contains("immutable")));
}

#[test]
fn d_on_a_system_comment_toasts_and_opens_no_confirm() {
    let (mut app, _comment) = app_with_system_comment();
    let effects = update(&mut app, key(KeyCode::Char('d')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
    assert!(app.confirm.is_none());
    assert!(app
        .toast
        .as_ref()
        .is_some_and(|t| t.is_error && t.text.contains("immutable")));
}

#[test]
fn h_on_a_system_comment_still_loads_history() {
    let (mut app, comment) = app_with_system_comment();
    let effects = update(&mut app, key(KeyCode::Char('h')));
    assert!(matches!(effects.as_slice(), [Effect::LoadCommentHistory { id }] if *id == comment.id));
    assert_eq!(app.screen, Screen::CardDetail);
}
