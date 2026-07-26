//! Touch/dead-zone coverage for the new Compact-mode `HitMap` widgets (header
//! buttons, switcher rows, button bars, sheet close) plus the wheel-scrolls-
//! the-hovered-column behavior.
//!
//! `HitMap` only exposes `hit(x, y)` (by design — it's a lookup table, not a
//! list), so instead of asking it for its zones directly this scans every
//! cell of the last-drawn frame and records the first coordinate at which
//! each distinct `Zone` value is found. That coordinate is guaranteed to be
//! inside the zone's registered rect (a real "click" target), without adding
//! a test-only accessor to production code.

use board_core::client::BoardClient;
use board_tui::app::{Msg, Screen, SwitcherLevel};
use board_tui::editor::FakeEditor;
use board_tui::forms::FieldId;
use board_tui::testkit::demo_client;
use board_tui::view::view;
use board_tui::widgets::Zone;
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

// -- helpers ------------------------------------------------------------------

fn driver() -> Driver {
    Driver::with_editor(
        Box::new(demo_client().unwrap()),
        Box::new(FakeEditor::new("edited")),
    )
    .unwrap()
}

fn key_msg(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::empty()))
}

/// The `Msg::Mouse` injector this suite needs; none existed before (mouse
/// input was only ever exercised through the `Driver`'s real terminal loop).
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Msg {
    Msg::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn left_down(column: u16, row: u16) -> Msg {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

/// Draw one frame at `(w, h)`, syncing `app.last_area` first so the HitMap
/// the draw registers agrees with what mouse handling will look up (mirrors
/// what `event_loop` does every iteration in `lib.rs`).
fn render_at(d: &mut Driver, w: u16, h: u16) {
    d.app.last_area = Rect::new(0, 0, w, h);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| view(&d.app, f)).unwrap();
}

/// Scan the whole frame and return one representative `(x, y)` per distinct
/// `Zone` value registered by the last draw, in the order first encountered.
fn hit_zones(d: &Driver, w: u16, h: u16) -> Vec<((u16, u16), Zone)> {
    let map = d.app.hit_map.borrow();
    let mut seen: Vec<Zone> = Vec::new();
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if let Some(zone) = map.hit(x, y) {
                if !seen.contains(&zone) {
                    seen.push(zone);
                    out.push(((x, y), zone));
                }
            }
        }
    }
    out
}

const COMPACT: (u16, u16) = (40, 20);
const REGULAR: (u16, u16) = (80, 24);

/// `view()` always draws the board first (`board::draw_board`) before any
/// overlay, and `HitMap` is only cleared once per frame — not re-scoped per
/// screen. In Compact mode that means the board's header zones stay
/// hit-testable underneath a fullscreen sheet even though the sheet visually
/// covers them (the fullscreen sheet's own top border row shares row 0 with
/// the header, but doesn't cover columns 0..3 or the center label). Every
/// `handle_zone` arm for these three zones is guarded with
/// `if app.screen == Screen::Board`, so a click there is inert on any other
/// screen — a harmless leak, not a misfire, but worth asserting explicitly
/// rather than silently special-casing it out of the matrix.
fn is_leaked_board_header_zone(zone: Zone) -> bool {
    matches!(
        zone,
        Zone::HeaderPrev | Zone::HeaderNext | Zone::HeaderSwitch
    )
}

// -- per-screen setup: real key-driven navigation, not hand-poked state -----

fn setup_board() -> Driver {
    driver()
}

/// Enter the Compact switcher sheet at the Columns level the same way a user
/// would tap the header's center button (`Zone::HeaderSwitch`), independent
/// of the size the dead-zone matrix later tests. NOT `b` — `b` means "switch
/// board" and now opens directly at the Boards level (see
/// `setup_switcher_boards_via_b`), so it can no longer reach Columns.
fn setup_switcher_columns() -> Driver {
    let mut d = driver();
    d.app.last_area = Rect::new(0, 0, 40, 20);
    render_at(&mut d, 40, 20);
    let (x, y) = hit_zones(&d, 40, 20)
        .into_iter()
        .find_map(|(pt, z)| (z == Zone::HeaderSwitch).then_some(pt))
        .expect("HeaderSwitch zone must be registered on the Compact board header");
    d.handle(left_down(x, y));
    assert_eq!(d.app.screen, Screen::Switcher);
    assert_eq!(
        d.app.switcher.as_ref().unwrap().level,
        SwitcherLevel::Columns
    );
    d
}

/// Enter the Compact switcher sheet directly at the Boards level via `b`
/// (Compact's `b` means "switch board" and skips Columns entirely — see
/// `board::board_key`'s `Char('b')` arm).
fn setup_switcher_boards_via_b() -> Driver {
    let mut d = driver();
    d.app.last_area = Rect::new(0, 0, 40, 20);
    d.handle(key_msg(KeyCode::Char('b')));
    let state = d.app.switcher.as_ref().unwrap();
    assert_eq!(state.level, SwitcherLevel::Boards);
    assert!(!state.boards.is_empty(), "boards must have loaded");
    assert!(state.entered_at_boards);
    d
}

fn setup_switcher_boards() -> Driver {
    let mut d = setup_switcher_columns();
    let n = d.app.board.columns.len();
    d.app.switcher.as_mut().unwrap().sel = n; // trailing "switch board" row
    d.handle(key_msg(KeyCode::Enter)); // -> Effect::LoadBoardsForSwitcher
    let state = d.app.switcher.as_ref().unwrap();
    assert_eq!(state.level, SwitcherLevel::Boards);
    assert!(!state.boards.is_empty(), "boards must have loaded");
    d
}

fn setup_form() -> Driver {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Char('N'))); // column form: fewer required fields
    assert_eq!(d.app.screen, Screen::ColumnForm);
    // Fill the one required field so BarSave's contract (submit) actually
    // succeeds instead of bouncing off validation.
    if let Some(form) = d.app.form.as_mut() {
        if let Some(f) = form.fields.iter_mut().find(|f| f.id == FieldId::Name) {
            f.set_text("Compact Col");
        }
    }
    d
}

fn setup_picker() -> Driver {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Char('m')));
    assert_eq!(d.app.screen, Screen::Picker);
    d
}

fn setup_confirm() -> Driver {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Char('d')));
    assert_eq!(d.app.screen, Screen::Confirm);
    d
}

fn setup_help() -> Driver {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Char('?')));
    assert_eq!(d.app.screen, Screen::Help);
    d
}

/// Open the detail of the seeded `Failed` card (Review column, index 3),
/// which already carries a few demo comments — the comment zone matrix needs
/// at least one to render `CommentRow`/`CommentEdit`/`CommentDelete`/
/// `CommentHistory`. Opening detail focuses the newest comment, which in this
/// fixture is the `[system]` one — see `setup_card_detail_non_system_focus`
/// for a variant focused on an editable comment.
fn setup_card_detail() -> Driver {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Right));
    d.handle(key_msg(KeyCode::Right));
    d.handle(key_msg(KeyCode::Right));
    d.handle(key_msg(KeyCode::Enter));
    assert_eq!(d.app.screen, Screen::CardDetail);
    assert!(
        !d.app.detail.as_ref().unwrap().comments.is_empty(),
        "fixture card must carry comments"
    );
    d
}

/// Same fixture, but with focus moved off the newest (`[system]`) comment
/// onto the second-newest (`[agent:2]`, editable) one, so the `CommentEdit`/
/// `CommentDelete` zones exercise the normal (non-immutable) path.
fn setup_card_detail_non_system_focus() -> Driver {
    let mut d = setup_card_detail();
    assert!(
        d.app.focused_comment().unwrap().author == "system",
        "fixture assumption: detail opens focused on the [system] comment"
    );
    d.handle(key_msg(KeyCode::Up));
    assert_ne!(d.app.focused_comment().unwrap().author, "system");
    d
}

// -- dead-zone contract: board header (Compact only) -------------------------

#[test]
fn compact_board_header_zones_present_and_wrap_both_ends() {
    let (w, h) = COMPACT;
    let mut probe = setup_board();
    render_at(&mut probe, w, h);
    let zones = hit_zones(&probe, w, h);
    assert!(
        !zones.is_empty(),
        "Compact board header must register hit zones"
    );

    for ((x, y), zone) in zones {
        let mut d = setup_board();
        render_at(&mut d, w, h);
        let before_col = d.app.sel_col;
        let n = d.app.board.columns.len();
        d.handle(left_down(x, y));
        match zone {
            Zone::HeaderPrev => {
                assert_eq!(d.app.sel_col, (before_col + n - 1) % n, "prev wraps at 0");
            }
            Zone::HeaderNext => {
                assert_eq!(d.app.sel_col, (before_col + 1) % n, "next wraps at n-1");
            }
            Zone::HeaderSwitch => {
                assert_eq!(d.app.screen, Screen::Switcher);
                let state = d.app.switcher.as_ref().unwrap();
                assert_eq!(state.level, SwitcherLevel::Columns);
                assert_eq!(state.sel, before_col);
            }
            other => panic!("unexpected zone on the board screen: {other:?}"),
        }
    }
}

#[test]
fn regular_board_draws_no_compact_header_zones() {
    let (w, h) = REGULAR;
    let mut d = setup_board();
    render_at(&mut d, w, h);
    assert!(
        hit_zones(&d, w, h).is_empty(),
        "Regular board must not register the Compact-only header zones"
    );
}

// -- dead-zone contract: switcher (level 1 = Columns, level 2 = Boards) ------

#[test]
fn switcher_columns_level_rows_select_and_close() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_switcher_columns();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        assert!(
            !zones.is_empty(),
            "switcher (columns level) must register hit zones at {w}x{h}"
        );

        for ((x, y), zone) in zones {
            let mut d = setup_switcher_columns();
            let n = d.app.board.columns.len();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SwitcherRow(idx) => {
                    assert!(idx < n, "SwitcherRow at columns level must index a column");
                    assert_eq!(d.app.screen, Screen::Board);
                    assert_eq!(d.app.sel_col, idx);
                    assert!(d.app.switcher.is_none());
                }
                Zone::SwitcherSwitchBoard => {
                    let state = d.app.switcher.as_ref().unwrap();
                    assert_eq!(state.level, SwitcherLevel::Boards);
                    assert!(!state.boards.is_empty());
                }
                Zone::SwitcherApplyTemplate => {
                    // The demo board is non-empty, so this row is disabled:
                    // activating it must raise the same toast the board `T`
                    // key raises, and keep the sheet open rather than apply.
                    assert_eq!(d.app.screen, Screen::Switcher);
                    assert!(d.app.switcher.is_some());
                    let toast = d.app.toast.as_ref().expect("error toast must be set");
                    assert!(toast.is_error);
                }
                Zone::SheetClose => {
                    assert_eq!(d.app.screen, Screen::Board);
                    assert!(d.app.switcher.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    // Guarded no-op: see `is_leaked_board_header_zone`.
                    assert_eq!(d.app.screen, Screen::Switcher);
                    assert!(d.app.switcher.is_some());
                }
                other => panic!("unexpected zone at switcher columns level: {other:?}"),
            }
        }
    }
}

#[test]
fn switcher_boards_level_rows_switch_board_and_back_button_restores_columns_sel() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_switcher_boards();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        assert!(
            !zones.is_empty(),
            "switcher (boards level) must register hit zones at {w}x{h}"
        );

        for ((x, y), zone) in zones {
            let mut d = setup_switcher_boards();
            render_at(&mut d, w, h);
            let expected_board_id = d.app.switcher.as_ref().and_then(|s| match zone {
                Zone::SwitcherRow(idx) => s.boards.get(idx).map(|(_, id)| *id),
                _ => None,
            });
            d.handle(left_down(x, y));
            match zone {
                Zone::SwitcherRow(_) => {
                    assert_eq!(d.app.screen, Screen::Board);
                    assert_eq!(Some(d.app.board.board.id), expected_board_id);
                }
                Zone::SheetClose => {
                    // Esc at the Boards level backs out to Columns rather
                    // than closing the whole sheet.
                    assert_eq!(d.app.screen, Screen::Switcher);
                    let state = d.app.switcher.as_ref().unwrap();
                    assert_eq!(state.level, SwitcherLevel::Columns);
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::Switcher);
                    let state = d.app.switcher.as_ref().unwrap();
                    assert_eq!(state.level, SwitcherLevel::Boards);
                }
                other => panic!("unexpected zone at switcher boards level: {other:?}"),
            }
        }
    }
}

/// Counterpart to the test above: when the Boards level was reached directly
/// via `b` (not drilled down from Columns), `SheetClose` must close the
/// sheet outright — there is no Columns view to back out to.
#[test]
fn switcher_boards_level_reached_via_b_closes_outright_on_sheet_close() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_switcher_boards_via_b();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        assert!(
            !zones.is_empty(),
            "switcher (boards level via `b`) must register hit zones at {w}x{h}"
        );

        for ((x, y), zone) in zones {
            let mut d = setup_switcher_boards_via_b();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SwitcherRow(_) => {
                    assert_eq!(d.app.screen, Screen::Board);
                }
                Zone::SheetClose => {
                    assert_eq!(
                        d.app.screen,
                        Screen::Board,
                        "Esc from a `b`-opened Boards level must close the sheet, \
                         not fall back to a Columns view the user never opened"
                    );
                    assert!(d.app.switcher.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::Switcher);
                    let state = d.app.switcher.as_ref().unwrap();
                    assert_eq!(state.level, SwitcherLevel::Boards);
                }
                other => panic!("unexpected zone at switcher boards level: {other:?}"),
            }
        }
    }
}

// -- dead-zone contract: form (ButtonBar Save/Cancel + sheet close) ----------

#[test]
fn form_bar_save_submits_and_bar_cancel_closes() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_form();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        assert!(
            !zones.is_empty(),
            "form must register at least the ButtonBar zones at {w}x{h}"
        );

        for ((x, y), zone) in zones {
            let mut d = setup_form();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::BarSave => {
                    assert_eq!(
                        d.app.screen,
                        Screen::Board,
                        "Save with a valid name field must submit and close the form"
                    );
                    assert!(d.app.form.is_none());
                }
                Zone::BarCancel => {
                    assert_eq!(d.app.screen, Screen::Board);
                    assert!(d.app.form.is_none());
                }
                Zone::SheetClose => {
                    // Only registered in Compact; Esc always cancels.
                    assert_eq!(d.app.screen, Screen::Board);
                    assert!(d.app.form.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert!(matches!(
                        d.app.screen,
                        Screen::CardForm | Screen::ColumnForm
                    ));
                    assert!(d.app.form.is_some());
                }
                other => panic!("unexpected zone on the form screen: {other:?}"),
            }
        }
    }
}

// -- dead-zone contract: picker / confirm / help (sheet close only) ----------

#[test]
fn picker_sheet_close_cancels_without_choosing() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_picker();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        if w < 60 {
            assert!(
                !zones.is_empty(),
                "Compact picker must register a sheet-close zone"
            );
        }
        for ((x, y), zone) in zones {
            let mut d = setup_picker();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SheetClose => {
                    assert_eq!(d.app.screen, Screen::Board);
                    assert!(d.app.picker.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::Picker);
                    assert!(d.app.picker.is_some());
                }
                other => panic!("unexpected zone on the picker screen: {other:?}"),
            }
        }
    }
}

#[test]
fn confirm_sheet_close_cancels_without_confirming() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_confirm();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        if w < 60 {
            assert!(
                !zones.is_empty(),
                "Compact confirm must register a sheet-close zone"
            );
        }
        for ((x, y), zone) in zones {
            let mut d = setup_confirm();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SheetClose => {
                    assert_eq!(d.app.screen, Screen::Board);
                    assert!(d.app.confirm.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::Confirm);
                    assert!(d.app.confirm.is_some());
                }
                other => panic!("unexpected zone on the confirm screen: {other:?}"),
            }
        }
    }
}

#[test]
fn help_sheet_close_returns_to_board() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_help();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        if w < 60 {
            assert!(
                !zones.is_empty(),
                "Compact help must register a sheet-close zone"
            );
        }
        for ((x, y), zone) in zones {
            let mut d = setup_help();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SheetClose => assert_eq!(d.app.screen, Screen::Board),
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::Help);
                }
                other => panic!("unexpected zone on the help screen: {other:?}"),
            }
        }
    }
}

// -- dead-zone contract: card detail comment zones ---------------------------

#[test]
fn card_detail_comment_zones_row_edit_delete_history() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_card_detail_non_system_focus();
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        assert!(
            !zones.is_empty(),
            "card detail must register at least the comment zones at {w}x{h}"
        );

        for ((x, y), zone) in zones {
            let mut d = setup_card_detail_non_system_focus();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::CommentRow(idx) => {
                    assert_eq!(d.app.screen, Screen::CardDetail);
                    assert_eq!(
                        d.app.detail_scroll_target,
                        board_tui::app::DetailScrollTarget::Comments
                    );
                    assert_eq!(d.app.detail_comment_sel, idx);
                }
                Zone::CommentEdit => {
                    assert_eq!(d.app.screen, Screen::CardForm);
                    assert!(matches!(
                        d.app.form.as_ref().map(|f| f.kind),
                        Some(board_tui::forms::FormKind::CommentEdit { .. })
                    ));
                }
                Zone::CommentDelete => {
                    assert_eq!(d.app.screen, Screen::Confirm);
                    assert!(d.app.confirm.is_some());
                }
                Zone::CommentHistory => {
                    assert_eq!(d.app.screen, Screen::CommentHistory);
                    assert!(d.app.comment_history.is_some());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::CardDetail);
                }
                other => panic!("unexpected zone on the card detail screen: {other:?}"),
            }
        }
    }
}

/// System comments are immutable (`docs/protocol.md`): tapping `[Edit]`/
/// `[Del]` while a `[system]` comment is focused must toast rather than open
/// the form/confirm — the zone stays tappable (not a dead zone), it just
/// routes to the same toast the `e`/`d` keys produce.
#[test]
fn card_detail_system_comment_edit_delete_zones_toast_without_changing_screen() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_card_detail();
        assert_eq!(probe.app.focused_comment().unwrap().author, "system");
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);

        for ((x, y), zone) in zones {
            if !matches!(zone, Zone::CommentEdit | Zone::CommentDelete) {
                continue;
            }
            let mut d = setup_card_detail();
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            assert_eq!(
                d.app.screen,
                Screen::CardDetail,
                "{zone:?} on a system comment must not change screen"
            );
            assert!(d.app.form.is_none());
            assert!(d.app.confirm.is_none());
            assert!(
                d.app
                    .toast
                    .as_ref()
                    .is_some_and(|t| t.is_error && t.text.contains("immutable")),
                "{zone:?} on a system comment must toast"
            );
        }
    }
}

#[test]
fn comment_history_sheet_close_returns_to_detail_not_board() {
    for &(w, h) in &[COMPACT, REGULAR] {
        let mut probe = setup_card_detail();
        probe.handle(key_msg(KeyCode::Char('h')));
        assert_eq!(probe.app.screen, Screen::CommentHistory);
        render_at(&mut probe, w, h);
        let zones = hit_zones(&probe, w, h);
        if w < 60 {
            assert!(
                !zones.is_empty(),
                "Compact comment-history sheet must register a sheet-close zone"
            );
        }

        for ((x, y), zone) in zones {
            let mut d = setup_card_detail();
            d.handle(key_msg(KeyCode::Char('h')));
            render_at(&mut d, w, h);
            d.handle(left_down(x, y));
            match zone {
                Zone::SheetClose => {
                    assert_eq!(
                        d.app.screen,
                        Screen::CardDetail,
                        "closing the history sheet must return to detail, not the board"
                    );
                    assert!(d.app.comment_history.is_none());
                }
                other if is_leaked_board_header_zone(other) => {
                    assert_eq!(d.app.screen, Screen::CommentHistory);
                }
                other => panic!("unexpected zone on the comment-history screen: {other:?}"),
            }
        }
    }
}

// -- wheel scrolls the hovered column, never reorders cards ------------------

/// Add `n` more cards to `column_name` so the column genuinely overflows its
/// viewport (the earlier version of this suite picked a 2-card column, which
/// never overflows at any tested size — that let Bug 1, the selection-follow
/// clamp silently overriding a wheel scroll on the focused column, slip past
/// this test entirely, since a non-overflowing column's offset always clamps
/// to 0 regardless of which code path is right).
fn driver_with_overflowing_column(column_name: &str, n: usize) -> Driver {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let col_id = board
        .columns
        .iter()
        .find(|c| c.name == column_name)
        .unwrap()
        .id;
    for i in 0..n {
        client
            .card_create(&board_core::protocol::CardCreateParams {
                title: format!("overflow card {i}"),
                column_id: Some(col_id),
                harness: Some("claude".into()),
                ..Default::default()
            })
            .unwrap();
    }
    Driver::with_editor(Box::new(client), Box::new(FakeEditor::new("edited"))).unwrap()
}

/// Wheel-scrolling an overflowing column that is NOT the selected one: the
/// rendered window must actually move (asserted on `board_layout`'s card
/// rects, not the internal `col_scroll` map), the selection must be
/// untouched (it isn't this column), and card order must be unchanged.
#[test]
fn wheel_scrolls_a_non_selected_overflowing_column_and_does_not_reorder_cards() {
    let mut d = driver_with_overflowing_column("Execute", 15);
    // Stay on Todo (index 0); Execute (index 2) is the hovered-but-unselected
    // overflowing column.
    assert_eq!(d.app.sel_col, 0);
    let execute_id = d.app.col_id_at(2).unwrap();
    let before_order: Vec<i64> = d.app.cards_of(execute_id).iter().map(|c| c.id).collect();
    assert!(before_order.len() > 10, "must genuinely overflow");

    render_at(&mut d, 120, 35);
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    let col = layout.cols.iter().find(|c| c.idx == 2).unwrap();
    assert!(col.scroll.overflowing(), "Execute must overflow at 120x35");
    let before_first_ci = col.cards.first().map(|&(ci, _)| ci);
    assert_eq!(before_first_ci, Some(0));
    let (x, y) = (col.rect.x + 2, col.rect.y + 2);

    d.handle(mouse(MouseEventKind::ScrollDown, x, y));
    d.handle(mouse(MouseEventKind::ScrollDown, x, y));
    d.handle(mouse(MouseEventKind::ScrollDown, x, y));

    // Re-render: the SAME call `event_loop` would make on the next frame.
    // This is exactly what Bug 1 broke — the window must have actually moved,
    // not silently snapped back.
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    let col = layout.cols.iter().find(|c| c.idx == 2).unwrap();
    let after_first_ci = col.cards.first().map(|&(ci, _)| ci);
    assert_eq!(
        after_first_ci,
        Some(3),
        "3 wheel-downs must move the rendered window forward by 3 rows"
    );
    assert_eq!(
        d.app.sel_col, 0,
        "the wheel must not touch the selected column"
    );
    assert_eq!(
        d.app.sel_card, 0,
        "Execute is not selected; its selection is untouched"
    );

    let after_order: Vec<i64> = d.app.cards_of(execute_id).iter().map(|c| c.id).collect();
    assert_eq!(
        before_order, after_order,
        "the wheel must never reorder cards"
    );
}

/// Bug 1's exact repro: wheel-scrolling the column that currently HAS the
/// selection is the common case (hovering over your own focused column), and
/// it must not be a no-op. The selection moves with the viewport instead of
/// the selection-follow clamp silently overriding the scroll.
#[test]
fn wheel_scrolls_the_selected_column_and_moves_selection_with_the_viewport() {
    let mut d = driver_with_overflowing_column("Todo", 15);
    assert_eq!(d.app.sel_col, 0); // Todo is selected by default
    assert_eq!(d.app.sel_card, 0);
    let todo_id = d.app.col_id_at(0).unwrap();
    let before_order: Vec<i64> = d.app.cards_of(todo_id).iter().map(|c| c.id).collect();

    render_at(&mut d, 120, 35);
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    let col = layout.cols.iter().find(|c| c.idx == 0).unwrap();
    assert!(col.scroll.overflowing(), "Todo must overflow at 120x35");
    let (x, y) = (col.rect.x + 2, col.rect.y + 2);

    d.handle(mouse(MouseEventKind::ScrollDown, x, y));
    d.handle(mouse(MouseEventKind::ScrollDown, x, y));
    d.handle(mouse(MouseEventKind::ScrollDown, x, y));

    assert_eq!(
        d.app.col_scroll.get(&todo_id).copied().unwrap_or(0),
        3,
        "the scroll offset itself must have advanced"
    );
    assert_eq!(
        d.app.sel_card, 3,
        "the selection must follow the viewport (was at 0, now scrolled off-window)"
    );

    // The regression: re-render (the next frame) and confirm the window is
    // still scrolled — the selection-follow clamp must be a no-op now that
    // the selection already sits inside the new window, not an override.
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    let col = layout.cols.iter().find(|c| c.idx == 0).unwrap();
    let first_ci = col.cards.first().map(|&(ci, _)| ci);
    assert_eq!(
        first_ci,
        Some(3),
        "wheel-scrolling the focused column must not be a no-op"
    );
    assert!(
        col.cards.iter().any(|&(ci, _)| ci == d.app.sel_card),
        "the selected card must still have a rect (invariant preserved)"
    );

    let after_order: Vec<i64> = d.app.cards_of(todo_id).iter().map(|c| c.id).collect();
    assert_eq!(
        before_order, after_order,
        "the wheel must never reorder cards"
    );

    // Scrolling back up must pull the selection back with it too.
    d.handle(mouse(MouseEventKind::ScrollUp, x, y));
    d.handle(mouse(MouseEventKind::ScrollUp, x, y));
    d.handle(mouse(MouseEventKind::ScrollUp, x, y));
    assert_eq!(d.app.col_scroll.get(&todo_id).copied().unwrap_or(0), 0);
}

/// Bug 2's repro at the mouse-handling layer: a terminal short enough that
/// zero cards fit must not panic on wheel input, and the resulting layout
/// must honestly report nothing visible rather than pretending one card is.
#[test]
fn wheel_over_a_column_with_zero_visible_slots_does_not_panic() {
    let mut d = driver_with_overflowing_column("Todo", 4);
    d.app.last_area = Rect::new(0, 0, 40, 4); // Compact; inner_h < card_h
    let mut term = Terminal::new(TestBackend::new(40, 4)).unwrap();
    term.draw(|f| view(&d.app, f)).unwrap();
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    assert_eq!(layout.cols[0].scroll.visible, 0);
    assert!(layout.cols[0].cards.is_empty());

    // Must not panic (no divide-by-zero, no underflow) and must leave the
    // column effectively unscrolled — there is no window to move into.
    d.handle(mouse(MouseEventKind::ScrollDown, 5, 2));
    d.handle(mouse(MouseEventKind::ScrollUp, 5, 2));
    let layout = board_tui::view::board_layout(&d.app, d.app.last_area);
    assert_eq!(layout.cols[0].scroll.visible, 0);
    assert!(layout.cols[0].cards.is_empty());
}

// -- click in a zone-less area is a no-op ------------------------------------

#[test]
fn click_on_the_footer_row_is_a_no_op() {
    let mut d = driver();
    render_at(&mut d, 80, 24);
    let before_screen = d.app.screen;
    let before_col = d.app.sel_col;
    let before_card = d.app.sel_card;
    // The footer row (last row) is outside `main_area`, so board_layout and
    // the HitMap both have nothing registered there.
    d.handle(left_down(0, 23));
    assert_eq!(d.app.screen, before_screen);
    assert_eq!(d.app.sel_col, before_col);
    assert_eq!(d.app.sel_card, before_card);
}
