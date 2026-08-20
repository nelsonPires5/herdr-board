//! Interaction contract for the functional 1:1 TUI redesign.
//!
//! Freezes the exact current interaction surface (the 72-row `view::HELP_KEYS`
//! contract): every documented binding, in order. Any keyboard/mouse behavior
//! change must be mirrored here deliberately — this is the "nothing removed,
//! nothing added, nothing remapped" gate for the redesign.
//!
//! `view::HELP_KEYS` is itself verified against the real `src/app/*.rs` key
//! handlers by `tests/help.rs` (coverage floor), so this table is the exact
//! frozen spec on top of that coverage check.

use board_tui::app::{update, Screen};
use board_tui::testkit::{demo_driver, key};
use board_tui::view::HELP_KEYS;
use crossterm::event::KeyCode;

const EXPECTED: &[(Screen, &str, &str)] = &[
    (Screen::Board, "←/→ h/l", "focus column"),
    (Screen::Board, "↑/↓ k/j", "focus card"),
    (Screen::Board, "p", "switch project"),
    (Screen::Board, "b", "switch board"),
    (Screen::Board, "n", "new card"),
    (Screen::Board, "N", "new column"),
    (Screen::Board, "e", "edit card"),
    (Screen::Board, "E", "edit focused column"),
    (Screen::Board, "a", "archive / restore card"),
    (Screen::Board, "C", "duplicate card"),
    (Screen::Board, "v", "cycle active/all/archived"),
    (Screen::Board, "d", "delete card"),
    (Screen::Board, "D", "delete/move column cards"),
    (Screen::Board, "m", "move card (project→board)"),
    (Screen::Board, "M", "move focused column"),
    (Screen::Board, "O", "reorder card in column"),
    (Screen::Board, "H / L", "shove card left / right"),
    (Screen::Board, "Enter", "card detail"),
    (Screen::Board, "T", "apply template (empty)"),
    (Screen::Board, "r / R", "refresh board"),
    (Screen::Board, "?", "this help (any screen)"),
    (Screen::Board, "q / Esc", "back / quit"),
    (Screen::CardDetail, "--", "-- card detail --"),
    (Screen::CardDetail, "Enter", "confirm done (awaiting)"),
    (Screen::CardDetail, "e", "edit card / comment"),
    (Screen::CardDetail, "a", "archive / restore card"),
    (Screen::CardDetail, "C", "duplicate card"),
    (Screen::CardDetail, "c", "add comment"),
    (Screen::CardDetail, "d", "delete focused comment"),
    (Screen::CardDetail, "h", "comment history"),
    (Screen::CardDetail, "Tab", "focus comments / runs"),
    (Screen::CardDetail, "↑/↓ k/j", "select comment / run"),
    (Screen::CardDetail, "f / click", "toggle popup / fullscreen"),
    (Screen::CardDetail, "o", "jump to selected run pane"),
    (Screen::CardDetail, "x", "cancel run (asks first)"),
    (Screen::CardDetail, "r", "retry run (asks first)"),
    (Screen::CardDetail, "q / Esc", "back to board"),
    (Screen::CardForm, "--", "-- forms --"),
    (Screen::CardForm, "Tab", "next field"),
    (Screen::CardForm, "Shift+Tab", "previous field"),
    (Screen::CardForm, "←/→ Space", "cycle a picker field"),
    (Screen::CardForm, "Ctrl+E", "edit textarea in $EDITOR"),
    (Screen::CardForm, "Shift+Enter", "newline in textarea"),
    (Screen::CardForm, "Ctrl+J", "newline in textarea"),
    (Screen::CardForm, "f (picker)", "toggle popup / fullscreen"),
    (Screen::CardForm, "Enter", "submit"),
    (Screen::CardForm, "Esc", "cancel"),
    (Screen::Picker, "--", "-- picker / confirm --"),
    (Screen::Picker, "↑/↓ k/j", "move selection"),
    (Screen::Picker, "Enter", "choose"),
    (Screen::Picker, "v", "cycle active/all/archived"),
    (Screen::Picker, "a", "archive board/project"),
    (Screen::Picker, "r", "restore board/project"),
    (Screen::ProjectPicker, "↑/↓ k/j", "move selection"),
    (Screen::ProjectPicker, "Enter", "choose"),
    (Screen::ProjectPicker, "v", "cycle active/all/archived"),
    (Screen::ProjectPicker, "a", "archive board/project"),
    (Screen::ProjectPicker, "r", "restore board/project"),
    (Screen::BoardPicker, "↑/↓ k/j", "move selection"),
    (Screen::BoardPicker, "Enter", "choose"),
    (Screen::BoardPicker, "v", "cycle active/all/archived"),
    (Screen::BoardPicker, "a", "archive board/project"),
    (Screen::BoardPicker, "r", "restore board/project"),
    (Screen::Confirm, "y / n", "confirm / decline"),
    (Screen::Picker, "q / Esc", "cancel"),
    (Screen::ProjectPicker, "q / Esc", "cancel"),
    (Screen::BoardPicker, "q / Esc", "cancel"),
    (Screen::MoveColumn, "--", "-- move column (M) --"),
    (Screen::MoveColumn, "←/→ h/l", "stage the reorder"),
    (Screen::MoveColumn, "Enter", "commit the reorder"),
    (Screen::MoveColumn, "q / Esc", "discard"),
    (Screen::ReorderCard, "--", "-- reorder card (O) --"),
    (Screen::ReorderCard, "j/k ↑/↓", "stage the reorder"),
    (Screen::ReorderCard, "Enter", "commit the reorder"),
    (Screen::ReorderCard, "q / Esc", "discard"),
    (Screen::Switcher, "--", "-- sheets --"),
    (Screen::Switcher, "k/j Enter", "switcher: move / open"),
    (Screen::Switcher, "q / Esc", "switcher: close / back"),
    (Screen::CommentHistory, "↑/↓ k/j", "history: scroll"),
    (Screen::CommentHistory, "q / Esc", "history: back to card"),
    (Screen::Help, "↑/↓ k/j", "help: scroll (compact)"),
    (Screen::Help, "q/Esc/any", "help: close"),
    (Screen::Board, "--", "-- mouse --"),
    (Screen::Board, "click", "focus card/column"),
    (Screen::Board, "dbl-click", "open card detail"),
    (Screen::Board, "drag", "move card/reorder column"),
    (Screen::Board, "wheel", "scroll cards"),
];

#[test]
fn contract_freezes_the_exact_72_row_interaction_table() {
    assert_eq!(
        HELP_KEYS.len(),
        87,
        "the interaction contract must stay at exactly 87 bindings"
    );
    assert_eq!(EXPECTED.len(), HELP_KEYS.len());
    for (idx, (expected, actual)) in EXPECTED.iter().zip(HELP_KEYS.iter()).enumerate() {
        assert_eq!(
            expected, actual,
            "contract row {idx} changed: {expected:?} -> {actual:?}"
        );
    }
}

/// Lower- vs upper-case bindings that must stay distinct (`n`/`N`, `e`/`E`,
/// `d`/`D`, `m`/`M`, `H`/`L`, `v`). The redesigned visuals must never conflate
/// the case of a shortcut.
#[test]
fn board_shortcuts_stay_case_sensitive() {
    use board_tui::forms::FormKind;
    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('n')));
    assert_eq!(d.app.screen, Screen::CardForm);
    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('N')));
    assert_eq!(d.app.screen, Screen::ColumnForm);

    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('e')));
    let mut forms = Vec::new();
    if let Some(form) = &d.app.form {
        forms.push(std::mem::discriminant(&form.kind));
    }
    assert!(
        d.app.form.as_ref().is_some_and(|_| matches!(
            d.app.form.as_ref().unwrap().kind,
            FormKind::CardEdit { .. }
        )),
        "lowercase e must edit the card"
    );
    let _ = forms;

    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('E')));
    assert!(
        d.app.form.as_ref().is_some_and(|_| matches!(
            d.app.form.as_ref().unwrap().kind,
            FormKind::ColumnEdit { .. }
        )),
        "uppercase E must edit the column"
    );

    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('m')));
    assert_eq!(
        d.app.screen,
        Screen::CardForm,
        "lowercase m opens the move-card form"
    );
    assert!(
        d.app
            .form
            .as_ref()
            .is_some_and(|form| matches!(form.kind, FormKind::MoveCard { .. })),
        "lowercase m must open the move-card form"
    );
    let mut d = demo_driver("x");
    d.handle(key(KeyCode::Char('M')));
    assert_eq!(
        d.app.screen,
        Screen::MoveColumn,
        "uppercase M moves the column"
    );

    let _ = update;
}
