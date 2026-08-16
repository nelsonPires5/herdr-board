//! Modal return paths and the destructive-action confirmations (B1–B8).
//!
//! The unifying rule under test: every modal records the `Screen` it was
//! opened from and dismissing it restores exactly that screen — no
//! re-derivation from what the modal happens to be for.

use super::helpers::{demo_app, demo_app_with_detail, demo_client, driver_of, key};
use board_core::protocol::CardStatus;
use board_tui::app::{update, App, ConfirmPurpose, Effect, PickerPurpose, Screen};
use crossterm::event::KeyCode;

/// Card detail open on the demo board's running card (so it has an open run).
fn detail_app() -> App {
    demo_app_with_detail(CardStatus::Running)
}

// -- B1: the risky delete path confirms too ---------------------------------

#[test]
fn deleting_a_column_with_cards_confirms_after_the_destination_picker() {
    let mut app = demo_app();
    // Todo (index 0) holds cards but has no open run.
    update(&mut app, key(KeyCode::Char('D')));
    assert_eq!(app.screen, Screen::Picker);
    let column_id = app.col_id_at(0).unwrap();
    let (target_label, target_id) = app.picker.as_ref().unwrap().options[0].clone();
    let moved = app
        .board
        .cards
        .iter()
        .filter(|c| c.column_id == column_id)
        .count();

    // Enter picks a destination — that is not consent to the delete.
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(
        effects.is_empty(),
        "picking a destination must not fire ColumnDelete"
    );
    assert_eq!(app.screen, Screen::Confirm);
    let confirm = app.confirm.as_ref().expect("confirmation raised");
    assert!(
        confirm.message.contains(&target_label) && confirm.message.contains(&moved.to_string()),
        "confirmation names the count and destination: {:?}",
        confirm.message
    );

    // …and only `y` commits, carrying the destination through.
    let effects = update(&mut app, key(KeyCode::Char('y')));
    match effects.as_slice() {
        [Effect::ColumnDelete { id, move_cards_to }] => {
            assert_eq!(*id, column_id);
            assert_eq!(*move_cards_to, Some(target_id));
        }
        other => panic!("expected one ColumnDelete, got {} effects", other.len()),
    }
    assert_eq!(app.screen, Screen::Board);
}

#[test]
fn declining_the_column_delete_confirmation_emits_nothing() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('D')));
    update(&mut app, key(KeyCode::Enter));
    let effects = update(&mut app, key(KeyCode::Char('n')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::Board);
    assert!(app.confirm.is_none());
}

// -- B2/B3: unified return paths --------------------------------------------

#[test]
fn help_returns_to_the_screen_it_was_opened_from() {
    // From the board.
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.screen, Screen::Board);

    // From card detail — the pre-fix behaviour dropped the user back on the
    // board while `app.detail` was still open.
    let mut app = detail_app();
    assert_eq!(app.screen, Screen::CardDetail);
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Char(' ')));
    assert_eq!(app.screen, Screen::CardDetail);
    assert!(
        app.detail.is_some(),
        "the open card survives the round trip"
    );
}

#[test]
fn opening_help_resets_its_scroll_from_every_screen() {
    let mut app = detail_app();
    app.help_scroll = 7;
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn a_form_opened_from_card_detail_returns_there_on_save_and_on_cancel() {
    for finish in [KeyCode::Esc, KeyCode::Enter] {
        let mut app = detail_app();
        update(&mut app, key(KeyCode::Char('c'))); // add comment
        assert_eq!(app.screen, Screen::CardForm);
        if finish == KeyCode::Enter {
            // A submittable body, so Enter is a save rather than a validation
            // error that keeps the form open.
            if let Some(form) = app.form.as_mut() {
                form.focused_mut().set_text("hi");
            }
        }
        update(&mut app, key(finish));
        assert_eq!(app.screen, Screen::CardDetail, "finishing with {finish:?}");
        assert!(app.form.is_none());
    }
}

#[test]
fn a_form_opened_from_the_board_returns_to_the_board() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('n')));
    assert_eq!(app.screen, Screen::CardForm);
    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::Board);
}

#[test]
fn a_confirmation_raised_from_card_detail_returns_there_on_both_answers() {
    // Yes.
    let mut app = detail_app();
    update(&mut app, key(KeyCode::Char('x')));
    assert_eq!(app.screen, Screen::Confirm);
    let effects = update(&mut app, key(KeyCode::Char('y')));
    assert!(matches!(effects.as_slice(), [Effect::RunCancel(_)]));
    assert_eq!(app.screen, Screen::CardDetail);

    // No.
    let mut app = detail_app();
    update(&mut app, key(KeyCode::Char('x')));
    let effects = update(&mut app, key(KeyCode::Esc));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
}

// -- B4: `?` is global ------------------------------------------------------

#[test]
fn help_is_reachable_from_every_non_form_screen() {
    // Picker.
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('m')));
    assert_eq!(app.screen, Screen::Picker);
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::Picker);

    // Confirm.
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('d')));
    assert_eq!(app.screen, Screen::Confirm);
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::Confirm);

    // Move-column mini-mode.
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('M')));
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(app.screen, Screen::Help);
    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::MoveColumn);
}

#[test]
fn question_mark_is_literal_text_inside_a_form() {
    let mut app = demo_app();
    update(&mut app, key(KeyCode::Char('n')));
    assert_eq!(app.screen, Screen::CardForm);
    update(&mut app, key(KeyCode::Char('?')));
    assert_eq!(
        app.screen,
        Screen::CardForm,
        "`?` must type into the focused field, not open help"
    );
    assert_eq!(app.form.as_ref().unwrap().focused().get_text(), "?");
}

// -- B5/B6: run actions ------------------------------------------------------

#[test]
fn retry_confirms_before_relaunching_an_agent() {
    let mut app = detail_app();
    let card_id = app.detail.as_ref().unwrap().card.id;
    let effects = update(&mut app, key(KeyCode::Char('r')));
    assert!(effects.is_empty(), "`r` alone must not relaunch anything");
    assert_eq!(app.screen, Screen::Confirm);
    assert!(matches!(
        app.confirm.as_ref().unwrap().purpose,
        ConfirmPurpose::RetryRun(id) if id == card_id
    ));

    let effects = update(&mut app, key(KeyCode::Char('y')));
    assert!(matches!(effects.as_slice(), [Effect::RunRetry(id)] if *id == card_id));
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn declining_retry_launches_nothing() {
    let mut app = detail_app();
    update(&mut app, key(KeyCode::Char('r')));
    let effects = update(&mut app, key(KeyCode::Char('n')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
}

#[test]
fn cancel_is_refused_when_no_run_is_open() {
    // A `done` card has no open run: `x` explains instead of asking a question
    // whose "yes" the daemon would reject.
    let mut app = demo_app_with_detail(CardStatus::Done);
    let effects = update(&mut app, key(KeyCode::Char('x')));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::CardDetail);
    assert!(app.confirm.is_none());
    let toast = app.toast.as_ref().expect("refusal is explained");
    assert!(toast.is_error);
    assert!(toast.text.contains("no open run"));
}

// -- B7: `q` closes the switcher --------------------------------------------

#[test]
fn q_closes_the_switcher_sheet_from_either_level() {
    let mut d = driver_of(demo_client().unwrap());
    d.app.last_area = ratatui::layout::Rect::new(0, 0, 40, 20); // Compact
    d.handle(key(KeyCode::Char('b'))); // opens straight at Boards
    assert_eq!(d.app.screen, Screen::Switcher);
    d.handle(key(KeyCode::Char('q')));
    assert_eq!(d.app.screen, Screen::Board);
    assert!(d.app.switcher.is_none());

    // Columns level (reached via the header tap in `mouse.rs`; constructed
    // here directly for the same effect).
    let mut d = driver_of(demo_client().unwrap());
    d.app.last_area = ratatui::layout::Rect::new(0, 0, 40, 20);
    d.app.switcher = Some(board_tui::app::SwitcherState {
        level: board_tui::app::SwitcherLevel::Columns,
        sel: 0,
        columns_sel: 0,
        boards: Vec::new(),
        entered_at_boards: false,
        return_to: Screen::Board,
    });
    d.app.screen = Screen::Switcher;
    d.handle(key(KeyCode::Char('q')));
    assert_eq!(d.app.screen, Screen::Board);
    assert!(d.app.switcher.is_none());
}

// -- B8: no unguarded indexing ----------------------------------------------

#[test]
fn enter_on_an_empty_picker_does_not_panic() {
    let mut app = demo_app();
    app.picker = Some(board_tui::app::Picker {
        title: "nothing to pick".into(),
        options: Vec::new(),
        sel: 0,
        purpose: PickerPurpose::SwitchBoard,
        return_to: Screen::Board,
    });
    app.screen = Screen::Picker;
    let effects = update(&mut app, key(KeyCode::Enter));
    assert!(effects.is_empty());
    assert_eq!(app.screen, Screen::Picker, "the dead end stays dismissable");
    // …and Esc still gets the user out.
    update(&mut app, key(KeyCode::Esc));
    assert_eq!(app.screen, Screen::Board);
}
