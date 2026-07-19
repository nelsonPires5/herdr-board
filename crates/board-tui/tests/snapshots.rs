//! ratatui `TestBackend` + `insta` snapshots driven through the real `Driver`
//! and `FakeBoardClient`. Everything is deterministic: a fixed `now`, fixed
//! terminal sizes, and running-card timers pinned by rewriting `updated_at`.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{AwaitingReason, CardCreateParams, CardStatus, RunOutcome};
use board_tui::app::{App, Msg};
use board_tui::editor::FakeEditor;
use board_tui::forms::{FieldId, FieldKind};
use board_tui::testkit::demo_client;
use board_tui::view::{parse_epoch, view};
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

const NOW_STR: &str = "2026-07-14 12:00:00";
const RUN_START: &str = "2026-07-14 11:58:00"; // 2m before NOW

fn now() -> i64 {
    parse_epoch(NOW_STR).unwrap()
}

/// Pin `now` and rewrite every running card's `updated_at` so timers are stable
/// (a board fetch resets them, so callers re-run this right before rendering).
fn pin(app: &mut App) {
    app.now = now();
    for c in &mut app.board.cards {
        if c.status == CardStatus::Running {
            c.updated_at = RUN_START.to_string();
        }
    }
}

fn driver<C: BoardClient + 'static>(client: C) -> Driver {
    Driver::with_editor(
        Box::new(client),
        Box::new(FakeEditor::new("edited via $EDITOR")),
    )
    .unwrap()
}

fn key(d: &mut Driver, code: KeyCode) {
    d.handle(Msg::Key(KeyEvent::new(code, KeyModifiers::empty())));
}

fn render(d: &mut Driver, w: u16, h: u16) -> String {
    pin(&mut d.app);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| view(&d.app, f)).unwrap();
    term.backend().to_string()
}

#[test]
fn empty_board() {
    let mut d = driver(FakeBoardClient::new().unwrap());
    insta::assert_snapshot!("empty_board", render(&mut d, 80, 24));
}

#[test]
fn seeded_board_glyphs_80x24() {
    let mut d = driver(demo_client().unwrap());
    insta::assert_snapshot!("seeded_board_80x24", render(&mut d, 80, 24));
}

#[test]
fn seeded_board_glyphs_120x35() {
    let mut d = driver(demo_client().unwrap());
    insta::assert_snapshot!("seeded_board_120x35", render(&mut d, 120, 35));
}

#[test]
fn archived_cards_all_and_archived_only() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let done = board
        .columns
        .iter()
        .find(|column| column.name == "Done")
        .unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.column_id == done.id)
        .unwrap();
    client.card_archive(card.id, true).unwrap();

    let mut d = driver(client);
    d.app.sel_col = d.app.board.columns.len() - 1;
    key(&mut d, KeyCode::Char('v')); // all
    insta::assert_snapshot!("archived_cards_all", render(&mut d, 120, 35));

    key(&mut d, KeyCode::Char('v')); // archived only
    insta::assert_snapshot!("archived_cards_only", render(&mut d, 120, 35));
}

#[test]
fn new_card_modal() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('n'));
    insta::assert_snapshot!("new_card_modal", render(&mut d, 80, 24));
}

#[test]
fn new_card_modal_pi_custom_model_low() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('n'));
    let form = d.app.form.as_mut().unwrap();
    let model = form
        .fields
        .iter_mut()
        .find(|field| field.id == FieldId::Model)
        .unwrap();
    if let FieldKind::Choice { opts, idx } = &mut model.kind {
        *idx = opts.iter().position(|opt| opt.label == "(custom)").unwrap();
    }
    form.on_model_changed();
    form.fields
        .iter_mut()
        .find(|field| field.id == FieldId::ModelCustom)
        .unwrap()
        .set_text("openai-codex/example");
    let effort = form
        .fields
        .iter_mut()
        .find(|field| field.id == FieldId::Effort)
        .unwrap();
    if let FieldKind::Choice { opts, idx } = &mut effort.kind {
        *idx = opts.iter().position(|opt| opt.label == "low").unwrap();
    }
    insta::assert_snapshot!("new_card_modal_pi_custom_low", render(&mut d, 80, 24));
}

#[test]
fn new_card_modal_freetext_fallback() {
    // Capability + space fetch both fail -> guided fields degrade to free text
    // and the footer warns.
    let client = demo_client().unwrap().without_caps().without_spaces();
    let mut d = driver(client);
    key(&mut d, KeyCode::Char('n'));
    insta::assert_snapshot!("new_card_modal_fallback", render(&mut d, 80, 24));
}

#[test]
fn edit_card_modal_selectors() {
    // The running card in Plan has model/effort/permission set and space_ref
    // "w4" -> the workspace selector preselects "MELI scraper (w4)".
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // Plan
    key(&mut d, KeyCode::Char('e'));
    insta::assert_snapshot!("edit_card_modal", render(&mut d, 80, 24));
}

#[test]
fn column_form() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('N'));
    insta::assert_snapshot!("column_form", render(&mut d, 80, 24));
}

#[test]
fn card_detail_with_comments_and_runs() {
    let mut d = driver(demo_client().unwrap());
    // Navigate to the failed card in Review (column index 3).
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("card_detail", render(&mut d, 80, 24));
}

#[test]
fn card_detail_popup_and_fullscreen_120x35() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("card_detail_popup_120x35", render(&mut d, 120, 35));

    key(&mut d, KeyCode::Char('f'));
    insta::assert_snapshot!("card_detail_fullscreen_120x35", render(&mut d, 120, 35));
}

#[test]
fn card_detail_history_overflow_starts_latest_and_scrolls_sections() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    for i in 0..15 {
        client
            .comment_add(card.id, &format!("overflow comment {i}"), Some("test"))
            .unwrap();
    }
    for _ in 0..10 {
        let run = client
            .db()
            .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
            .unwrap();
        client.db().start_run(run.id, None, None).unwrap();
        client
            .db()
            .finish_run(run.id, RunOutcome::Ok, Some("done"))
            .unwrap();
    }

    let mut d = driver(client);
    d.app.last_area = Rect::new(0, 0, 120, 35);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("card_detail_history_latest", render(&mut d, 120, 35));

    key(&mut d, KeyCode::Up);
    key(&mut d, KeyCode::Up);
    key(&mut d, KeyCode::Tab);
    key(&mut d, KeyCode::Up);
    key(&mut d, KeyCode::Up);
    insta::assert_snapshot!("card_detail_history_scrolled", render(&mut d, 120, 35));
}

#[test]
fn board_picker_wide_and_narrow() {
    let mut wide = driver(demo_client().unwrap());
    key(&mut wide, KeyCode::Char('b'));
    insta::assert_snapshot!("board_picker_120x35", render(&mut wide, 120, 35));

    let mut narrow = driver(demo_client().unwrap());
    key(&mut narrow, KeyCode::Char('b'));
    insta::assert_snapshot!("board_picker_80x24", render(&mut narrow, 80, 24));
}

#[test]
fn help_overlay() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('?'));
    insta::assert_snapshot!("help_overlay", render(&mut d, 80, 24));
}

#[test]
fn delete_column_with_cards_picker() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // Plan (has the running card)
    key(&mut d, KeyCode::Char('D'));
    insta::assert_snapshot!("delete_column_picker", render(&mut d, 80, 24));
}

#[test]
fn move_card_flow() {
    let mut d = driver(demo_client().unwrap());
    // "before": Todo's card is selected.
    insta::assert_snapshot!("move_before", render(&mut d, 80, 24));
    // Open the move picker and move the card to Plan (first option).
    key(&mut d, KeyCode::Char('m'));
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("move_after", render(&mut d, 80, 24));
}

#[test]
fn toast_on_client_error() {
    let mut d = driver(demo_client().unwrap());
    // Open a card's detail, then retry: FakeBoardClient has no run.retry -> toast.
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    key(&mut d, KeyCode::Char('r'));
    assert!(d.app.toast.as_ref().is_some_and(|t| t.is_error));
    insta::assert_snapshot!("toast_error", render(&mut d, 80, 24));
}

#[test]
fn awaiting_card_detail_shows_agent_done_reason() {
    let mut d = driver(demo_client().unwrap());
    // Review (idx 3): failed card first, awaiting ("Tune retry backoff") second.
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("awaiting_card_detail", render(&mut d, 80, 24));
}

#[test]
fn awaiting_card_detail_shows_idle_timeout_reason() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let todo = board
        .columns
        .iter()
        .find(|column| column.name == "Todo")
        .unwrap()
        .id;
    let id = client
        .card_create(&CardCreateParams {
            title: "Silent agent".into(),
            description: Some("Went idle without reporting back.".into()),
            column_id: Some(todo),
            harness: Some("claude".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    client
        .db()
        .set_card_awaiting(id, AwaitingReason::IdleExpired)
        .unwrap();

    let mut d = driver(client);
    key(&mut d, KeyCode::Down); // second card in Todo
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("awaiting_idle_detail", render(&mut d, 80, 24));
}

#[test]
fn done_card_detail_is_final() {
    let mut d = driver(demo_client().unwrap());
    // Done column (idx 5): "Ship v0.1" (idle) first, "Write changelog" (done) second.
    for _ in 0..5 {
        key(&mut d, KeyCode::Right);
    }
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("done_card_detail", render(&mut d, 80, 24));
}
