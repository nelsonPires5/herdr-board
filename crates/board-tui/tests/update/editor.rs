//! Editor round-trip redraw regression (Bug A): returning from `$EDITOR` used
//! to leave the terminal blank because ratatui's cell-diff had no idea the
//! alternate screen had been clobbered outside its control. `EditResult::
//! needs_full_redraw` is the fix's signal; `Driver::needs_full_redraw` +
//! `Driver::sync_frame_area` are the (test-only-exercised) hooks `event_loop`
//! uses to force a `terminal.clear()` before the next draw.

use super::helpers::key;
use board_tui::editor::FakeEditor;
use board_tui::forms::FieldId;
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

fn driver_with_editor(result: &str) -> Driver {
    let client = super::helpers::demo_client().unwrap();
    Driver::with_editor(Box::new(client), Box::new(FakeEditor::new(result))).unwrap()
}

/// Open the new-card form and focus its multiline Description field, so
/// Ctrl+E dispatches `Effect::EditFocusedTextArea`.
fn open_form_focused_on_description(d: &mut Driver) {
    d.handle(key(KeyCode::Char('n')));
    assert_eq!(d.app.screen, board_tui::app::Screen::CardForm);
    let form = d.app.form.as_mut().expect("new-card form");
    let idx = form
        .fields
        .iter()
        .position(|f| f.id == FieldId::Description)
        .expect("Description field present");
    form.focus = idx;
    assert!(form.focused_is_multiline());
}

fn ctrl_e() -> board_tui::app::Msg {
    board_tui::app::Msg::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
}

/// `FakeEditor` always reports `needs_full_redraw: false` (see `editor.rs`),
/// so exercising the `true` branch needs a small local shim launcher.
struct RedrawEditor;
impl board_tui::editor::EditorLauncher for RedrawEditor {
    fn edit(&self, _initial: &str) -> anyhow::Result<board_tui::editor::EditResult> {
        Ok(board_tui::editor::EditResult {
            text: "edited".to_string(),
            needs_full_redraw: true,
        })
    }
}

#[test]
fn editor_round_trip_with_needs_full_redraw_true_sets_then_clears_the_flag() {
    let client = super::helpers::demo_client().unwrap();
    let mut d = Driver::with_editor(Box::new(client), Box::new(RedrawEditor)).unwrap();

    open_form_focused_on_description(&mut d);
    assert!(
        !d.needs_full_redraw(),
        "no redraw pending before the editor round-trip"
    );

    d.handle(ctrl_e());

    assert!(
        d.needs_full_redraw(),
        "an editor round-trip reporting needs_full_redraw:true must set the driver's flag"
    );
    assert_eq!(
        d.app.form.as_ref().unwrap().focused().get_text(),
        "edited",
        "the field text must still be updated from the round-trip"
    );

    // `event_loop`'s consume-then-clear step.
    assert!(d.take_needs_full_redraw(), "flag must be observed as set");
    assert!(
        !d.needs_full_redraw(),
        "the flag must be cleared once consumed"
    );
}

#[test]
fn editor_round_trip_with_needs_full_redraw_false_never_sets_the_flag() {
    // `FakeEditor` (used everywhere else in this suite) reports
    // `needs_full_redraw: false` — the common case for tests, where nothing
    // ever really clobbered the alternate screen.
    let mut d = driver_with_editor("edited via $EDITOR");
    open_form_focused_on_description(&mut d);
    assert!(!d.needs_full_redraw());

    d.handle(ctrl_e());

    assert!(
        !d.needs_full_redraw(),
        "FakeEditor reports needs_full_redraw:false, so the flag must stay clear"
    );
    assert_eq!(
        d.app.form.as_ref().unwrap().focused().get_text(),
        "edited via $EDITOR"
    );
}

#[test]
fn simulated_terminal_size_change_sets_the_redraw_flag() {
    let mut d = driver_with_editor("x");
    d.app.last_area = Rect::new(0, 0, 80, 24);
    assert!(!d.needs_full_redraw());

    // Same size: no-op, matching `event_loop`'s per-iteration check.
    d.sync_frame_area(Rect::new(0, 0, 80, 24));
    assert!(
        !d.needs_full_redraw(),
        "an unchanged size must not force a redraw"
    );

    // Shrink then grow back: still a genuine size change each time.
    d.sync_frame_area(Rect::new(0, 0, 40, 20));
    assert!(
        d.needs_full_redraw(),
        "a terminal-size change must set the redraw flag"
    );
    assert_eq!(d.app.last_area, Rect::new(0, 0, 40, 20));

    // The flag survives until consumed, even across another draw-cycle sync
    // at the same (new) size.
    d.sync_frame_area(Rect::new(0, 0, 40, 20));
    assert!(d.needs_full_redraw());

    assert!(d.take_needs_full_redraw());
    assert!(!d.needs_full_redraw());
}
