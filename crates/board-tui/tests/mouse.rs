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
use board_tui::app::{update, Effect, Msg, Screen};
use board_tui::forms::FieldId;
use board_tui::testkit::{
    demo_client, demo_driver, driver_with_editor, key as key_msg, left_down, mouse, render_at,
};
use board_tui::widgets::{UiAction, Zone};
use board_tui::Driver;
use crossterm::event::{KeyCode, MouseEventKind};
use ratatui::layout::Rect;

// -- helpers ------------------------------------------------------------------

/// This suite's canned `$EDITOR` text.
const EDITED: &str = "edited";

fn driver() -> Driver {
    demo_driver(EDITED)
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
    d.handle(key_msg(KeyCode::Char('b')));
    assert_eq!(d.app.screen, Screen::BoardPicker);
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

    for ((x, y), zone) in zones.into_iter().filter(|(_, zone)| {
        matches!(
            zone,
            Zone::HeaderPrev | Zone::HeaderNext | Zone::HeaderSwitch
        )
    }) {
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
        hit_zones(&d, w, h).into_iter().all(|(_, zone)| !matches!(
            zone,
            Zone::HeaderPrev | Zone::HeaderNext | Zone::HeaderSwitch
        )),
        "Regular board must not register the Compact-only header zones"
    );
}

/// The header's project and board chips are real click targets at every
/// breakpoint: the project chip opens the project picker, the board chip the
/// board picker.
#[test]
fn header_project_and_board_chips_open_their_pickers_at_every_breakpoint() {
    for (w, h) in [(40u16, 20u16), (52, 24), (60, 24), (80, 24), (120, 35)] {
        let mut project = setup_board();
        render_at(&mut project, w, h);
        let project_pt = hit_zones(&project, w, h)
            .into_iter()
            .find_map(|(point, zone)| {
                (zone == Zone::Action(UiAction::SwitchProject)).then_some(point)
            })
            .unwrap_or_else(|| panic!("project chip zone missing at {w}x{h}"));
        project.handle(left_down(project_pt.0, project_pt.1));
        assert_eq!(
            project.app.screen,
            Screen::ProjectPicker,
            "project chip must open the project picker at {w}x{h}"
        );

        let mut board = setup_board();
        render_at(&mut board, w, h);
        let board_pt = hit_zones(&board, w, h)
            .into_iter()
            .find_map(|(point, zone)| {
                (zone == Zone::Action(UiAction::SwitchBoard)).then_some(point)
            })
            .unwrap_or_else(|| panic!("board chip zone missing at {w}x{h}"));
        board.handle(left_down(board_pt.0, board_pt.1));
        assert_eq!(
            board.app.screen,
            Screen::BoardPicker,
            "board chip must open the board picker at {w}x{h}"
        );
    }
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

        for ((x, y), zone) in zones.into_iter().filter(|(_, zone)| {
            matches!(
                zone,
                Zone::SwitcherRow(_)
                    | Zone::SwitcherSwitchBoard
                    | Zone::SwitcherApplyTemplate
                    | Zone::SheetClose
            )
        }) {
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
                    // The trailing row opens the board picker for the current
                    // project; the sheet stays open underneath so Esc from the
                    // picker returns to the columns.
                    assert_eq!(d.app.screen, Screen::BoardPicker);
                    assert!(d.app.picker.is_some());
                    assert!(d.app.switcher.is_some());
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

        for ((x, y), zone) in zones
            .into_iter()
            .filter(|(_, zone)| matches!(zone, Zone::BarSave | Zone::BarCancel | Zone::SheetClose))
        {
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
        for ((x, y), zone) in zones
            .into_iter()
            .filter(|(_, zone)| matches!(zone, Zone::SheetClose))
        {
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
        for ((x, y), zone) in zones
            .into_iter()
            .filter(|(_, zone)| matches!(zone, Zone::SheetClose))
        {
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
        for ((x, y), zone) in zones
            .into_iter()
            .filter(|(_, zone)| matches!(zone, Zone::SheetClose))
        {
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

        for ((x, y), zone) in zones.into_iter().filter(|(_, zone)| {
            matches!(
                zone,
                Zone::CommentRow(_)
                    | Zone::CommentEdit
                    | Zone::CommentDelete
                    | Zone::CommentHistory
            )
        }) {
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

/// System comments are immutable (`docs/protocol.md`): tapping `[ Edit ]`/
/// `[ Delete ]` while a `[system]` comment is focused must toast rather than open
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

        for ((x, y), zone) in zones
            .into_iter()
            .filter(|(_, zone)| matches!(zone, Zone::SheetClose))
        {
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
    driver_with_editor(client, EDITED)
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
    render_at(&mut d, 40, 4); // Compact; inner_h < card_h
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
fn idle_board_footer_is_inert_and_help_stays_in_the_header_and_action_row() {
    let mut blank = driver();
    let output = render_at(&mut blank, 80, 24);
    let before_col = blank.app.sel_col;
    let before_card = blank.app.sel_card;
    assert!(
        output
            .lines()
            .all(|line| { !line.contains("drag card to move") && !line.contains("? help") }),
        "no persistent footer hint may be rendered:\n{output}"
    );
    // The old footer row is now reclaimed. A blank cell immediately above the
    // action rail remains inert, while the rail itself stays clickable.
    blank.handle(left_down(20, 21));
    assert_eq!(blank.app.screen, Screen::Board);
    assert_eq!(blank.app.sel_col, before_col);
    assert_eq!(blank.app.sel_card, before_card);
    // Help remains reachable from the rendered action row instead.
    let mut help = driver();
    render_at(&mut help, 80, 24);
    let (x, y) = hit_zones(&help, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| (zone == Zone::Action(UiAction::Help)).then_some(point))
        .expect("Help action must be rendered");
    help.handle(left_down(x, y));
    assert_eq!(help.app.screen, Screen::Help);
}

// -- shared semantic action zones --------------------------------------------

fn click_semantic_action(d: &mut Driver, action: UiAction) {
    let rect = Rect::new(2, 2, 8, 1);
    d.app.hit_map.borrow_mut().push(rect, Zone::Action(action));
    d.handle(left_down(3, 2));
}

#[test]
fn semantic_board_action_zones_follow_the_existing_key_paths() {
    let mut new_by_click = setup_board();
    click_semantic_action(&mut new_by_click, UiAction::NewCard);
    assert_eq!(new_by_click.app.screen, Screen::CardForm);

    let mut open_by_click = setup_board();
    click_semantic_action(&mut open_by_click, UiAction::OpenCard);
    assert_eq!(open_by_click.app.screen, Screen::CardDetail);

    let mut help_by_click = setup_board();
    click_semantic_action(&mut help_by_click, UiAction::Help);
    assert_eq!(help_by_click.app.screen, Screen::Help);

    let mut filter_by_click = setup_board();
    let before = filter_by_click.app.card_filter;
    click_semantic_action(&mut filter_by_click, UiAction::CycleFilter);
    assert_ne!(filter_by_click.app.card_filter, before);
}

#[test]
fn semantic_detail_action_zones_follow_the_existing_key_paths() {
    let mut close_by_click = setup_card_detail();
    click_semantic_action(&mut close_by_click, UiAction::CloseDetail);
    assert_eq!(close_by_click.app.screen, Screen::Board);

    let mut comment_by_click = setup_card_detail();
    click_semantic_action(&mut comment_by_click, UiAction::AddComment);
    assert_eq!(comment_by_click.app.screen, Screen::CardForm);
}

#[test]
fn semantic_action_is_inert_on_the_wrong_screen() {
    let mut d = setup_board();
    click_semantic_action(&mut d, UiAction::DeleteComment);
    assert_eq!(d.app.screen, Screen::Board);
}

#[test]
fn board_chrome_exposes_the_reduced_creation_and_column_actions_at_every_breakpoint() {
    let expected = [
        UiAction::NewCard,
        UiAction::OpenCard,
        UiAction::ArchiveCard,
        UiAction::Help,
        UiAction::NewColumn,
        UiAction::EditColumn,
        UiAction::DeleteColumn,
        UiAction::MoveColumn,
        UiAction::ApplyTemplate,
        UiAction::SwitchProject,
    ];
    let expected_wide = [UiAction::SwitchBoard];
    for (w, h) in [(40, 20), (80, 24), (120, 35)] {
        let mut d = setup_board();
        render_at(&mut d, w, h);
        let zones = hit_zones(&d, w, h);
        for action in expected {
            assert!(
                zones.iter().any(|(_, zone)| *zone == Zone::Action(action)),
                "{w}x{h} must expose {action:?}"
            );
        }
        if w >= 80 {
            for action in expected_wide {
                assert!(
                    zones.iter().any(|(_, zone)| *zone == Zone::Action(action)),
                    "{w}x{h} must expose {action:?}"
                );
            }
        }
    }
}

#[test]
fn rendered_board_actions_reuse_keyboard_behavior() {
    for (action, expected_screen) in [
        (UiAction::NewCard, Screen::CardForm),
        (UiAction::OpenCard, Screen::CardDetail),
        (UiAction::Help, Screen::Help),
    ] {
        let mut d = setup_board();
        render_at(&mut d, 40, 20);
        let (x, y) = hit_zones(&d, 40, 20)
            .into_iter()
            .find_map(|(point, zone)| (zone == Zone::Action(action)).then_some(point))
            .unwrap_or_else(|| panic!("missing rendered {action:?}"));
        d.handle(left_down(x, y));
        assert_eq!(d.app.screen, expected_screen, "action={action:?}");
    }
}

#[test]
fn mandatory_board_breakpoints_keep_filter_labels_complete_and_action_zone_tight() {
    use board_tui::app::CardFilter;
    for (w, h) in [(40, 20), (52, 24), (60, 24), (80, 24), (120, 35)] {
        let mut d = setup_board();
        let output = render_at(&mut d, w, h);
        let expected_filters = match w {
            0..=47 => ["[ Act ]", "[ All ]", "[ Arc ]"],
            48..=59 => ["[ Active ]", "[ All ]", "[ Archived ]"],
            60..=71 => ["[ A ]", "[ All ]", "[ R ]"],
            _ => ["[ Active ]", "[ All ]", "[ Archived ]"],
        };
        for filter in expected_filters {
            assert!(
                output.contains(filter),
                "filter {filter:?} clipped at {w}x{h}"
            );
        }
        if w >= 60 {
            assert!(
                !output.contains("Board:"),
                "redundant Board label at {w}x{h}"
            );
        }
        assert!(
            !output.contains("Visible:"),
            "redundant Visible label at {w}x{h}"
        );
        for label in ["Edit col", "Del col", "Move col", "Template", "? Help"] {
            let label = if label == "Del col" && output.contains("Delete column") {
                "Delete column"
            } else {
                label
            };
            assert!(
                output.contains(label),
                "semantic board action {label:?} clipped at {w}x{h}"
            );
        }
        assert!(
            output.contains("New card") || output.contains("+ Card"),
            "New card action clipped at {w}x{h}"
        );
        for action in [UiAction::Refresh, UiAction::Quit] {
            assert!(
                !output
                    .lines()
                    .any(|l| l.contains("Refresh") || l.contains("Quit")),
                "{action:?} must not be rendered at {w}x{h}"
            );
        }
        for filter in [CardFilter::Active, CardFilter::All, CardFilter::Archived] {
            let cells = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter(|(x, y)| d.app.hit_map.borrow().hit(*x, *y) == Some(Zone::Filter(filter)))
                .collect::<Vec<_>>();
            assert!(!cells.is_empty(), "filter {filter:?} missing at {w}x{h}");
        }
    }
}

#[test]
fn rendered_board_actions_follow_existing_guards_and_screens() {
    for (action, expected_screen) in [
        (UiAction::NewColumn, Screen::ColumnForm),
        (UiAction::DeleteColumn, Screen::Picker),
        (UiAction::MoveColumn, Screen::MoveColumn),
        (UiAction::EditColumn, Screen::ColumnForm),
        (UiAction::ApplyTemplate, Screen::Board),
    ] {
        let mut d = setup_board();
        render_at(&mut d, 80, 24);
        let (x, y) = hit_zones(&d, 80, 24)
            .into_iter()
            .find_map(|(point, zone)| (zone == Zone::Action(action)).then_some(point))
            .unwrap_or_else(|| panic!("missing rendered {action:?}"));
        d.handle(left_down(x, y));
        assert_eq!(d.app.screen, expected_screen, "action={action:?}");
    }
}

#[test]
fn card_detail_run_row_click_focuses_runs_and_selects_exact_older_run() {
    let mut d = setup_card_detail();
    let runs = &mut d.app.detail.as_mut().unwrap().runs;
    assert!(!runs.is_empty(), "demo failed card must have a run");
    let mut older = runs[0].clone();
    older.id = 90_001;
    let mut newer = runs[0].clone();
    newer.id = 90_002;
    *runs = vec![older, newer];
    d.app.detail_run_sel = 1;
    d.app.detail_runs_scroll = 0;
    d.app.detail_scroll_target = board_tui::app::DetailScrollTarget::Comments;

    render_at(&mut d, 120, 35);
    let (x, y) = hit_zones(&d, 120, 35)
        .into_iter()
        .find_map(|(point, zone)| (zone == Zone::RunRow(0)).then_some(point))
        .expect("older run row must be clickable");
    d.handle(left_down(x, y));
    assert_eq!(
        d.app.detail_scroll_target,
        board_tui::app::DetailScrollTarget::Runs
    );
    assert_eq!(d.app.detail_run_sel, 0);
    let effects = update(
        &mut d.app,
        Msg::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('o'),
            crossterm::event::KeyModifiers::NONE,
        )),
    );
    assert!(
        matches!(effects.as_slice(), [Effect::FocusRun(card_id, 90_001)]
        if *card_id == d.app.detail.as_ref().unwrap().card.id)
    );
}

#[test]
fn card_detail_title_close_and_toggle_actions_follow_keyboard_paths() {
    let mut toggle = setup_card_detail();
    render_at(&mut toggle, 80, 24);
    let point = hit_zones(&toggle, 80, 24)
        .into_iter()
        .find_map(|(p, z)| (z == Zone::Action(UiAction::ToggleDetail)).then_some(p))
        .expect("toggle title action");
    toggle.handle(left_down(point.0, point.1));
    assert!(toggle.app.detail_fullscreen);

    let mut close = setup_card_detail();
    render_at(&mut close, 80, 24);
    let point = hit_zones(&close, 80, 24)
        .into_iter()
        .find_map(|(p, z)| (z == Zone::Action(UiAction::CloseDetail)).then_some(p))
        .expect("close title action");
    close.handle(left_down(point.0, point.1));
    assert_eq!(close.app.screen, Screen::Board);
    assert!(close.app.detail.is_none());
}

#[test]
fn card_detail_exposes_card_comment_and_run_action_zones_at_all_breakpoints() {
    let expected = [
        UiAction::EditCard,
        UiAction::ArchiveCard,
        UiAction::AddComment,
        UiAction::FocusRunPane,
        UiAction::RetryRun,
        UiAction::CancelRun,
        UiAction::CloseDetail,
        UiAction::ToggleDetail,
    ];
    for (w, h) in [(40, 20), (52, 24), (60, 24), (80, 24), (120, 35)] {
        let mut d = setup_card_detail_non_system_focus();
        render_at(&mut d, w, h);
        let zones = hit_zones(&d, w, h);
        for action in expected {
            assert!(
                zones.iter().any(|(_, z)| *z == Zone::Action(action)),
                "{w}x{h} missing {action:?}"
            );
        }
        // The persistent chrome leaves no run-row viewport at 40x20 or
        // 80x24; the run actions remain in the card rail/section action row.
        // The wide three-pane detail keeps a rendered clickable run row.
        if w >= 120 {
            assert!(zones.iter().any(|(_, z)| matches!(z, Zone::RunRow(_))));
        }
        // At 40x20 the comments card collapses to a hint without the in-card
        // action bar, so the Edit/Delete/History bar is only guaranteed where
        // the section has room for it.
        if w >= 80 {
            assert!(
                zones.iter().any(|(_, z)| matches!(z, Zone::CommentEdit)),
                "{w}x{h} missing CommentEdit"
            );
        }
    }
}

#[test]
fn card_detail_awaiting_confirm_zone_is_conditional_and_uses_run_done_path() {
    for (w, h) in [(40, 20), (80, 24)] {
        let mut d = setup_card_detail();
        assert!(!hit_zones_after_render(&mut d, w, h)
            .iter()
            .any(|(_, z)| *z == Zone::Action(UiAction::ConfirmAwaiting)));
        d.app.detail.as_mut().unwrap().card.status = board_core::protocol::CardStatus::Awaiting;
        let zones = hit_zones_after_render(&mut d, w, h);
        let point = zones
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::ConfirmAwaiting)).then_some(p))
            .expect("awaiting confirm");
        let card_id = d.app.detail.as_ref().unwrap().card.id;
        // HitMap routing itself is covered by title/actions above; use the same
        // reducer event here so the exact completion effect stays observable.
        let effects = update(
            &mut d.app,
            Msg::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        assert!(
            matches!(effects.as_slice(), [Effect::RunDone(id, board_core::protocol::RunOutcome::Ok)] if *id == card_id)
        );
        assert!(point.0 < w);
    }
}

fn hit_zones_after_render(d: &mut Driver, w: u16, h: u16) -> Vec<((u16, u16), Zone)> {
    render_at(d, w, h);
    hit_zones(d, w, h)
}

#[test]
fn rendered_form_fields_and_choice_arrows_update_the_real_form_focus_and_value() {
    let mut d = setup_form();
    render_at(&mut d, 80, 24);
    let zones = hit_zones(&d, 80, 24);
    let (field_point, field_idx) = zones
        .iter()
        .find_map(|(point, zone)| match zone {
            Zone::FormField(idx) if *idx != d.app.form.as_ref().unwrap().focus => {
                Some((*point, *idx))
            }
            _ => None,
        })
        .expect("a second visible form field");
    d.handle(left_down(field_point.0, field_point.1));
    assert_eq!(d.app.form.as_ref().unwrap().focus, field_idx);

    render_at(&mut d, 80, 24);
    let (next_point, choice_idx) = hit_zones(&d, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| match zone {
            Zone::FormChoiceNext(idx) => Some((point, idx)),
            _ => None,
        })
        .expect("visible choice next control");
    let before = d.app.form.as_ref().unwrap().fields[choice_idx].display();
    d.handle(left_down(next_point.0, next_point.1));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.focus, choice_idx);
    assert_ne!(form.fields[choice_idx].display(), before);
}

#[test]
fn rendered_picker_rows_and_confirm_buttons_use_existing_reducers() {
    // The board picker's item row activates through Enter (SelectBoard).
    let mut picker = setup_picker();
    render_at(&mut picker, 80, 24);
    let (point, selected) = hit_zones(&picker, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| match zone {
            Zone::PickerRow(idx) if idx > 0 => Some((point, idx)),
            _ => None,
        })
        .expect("second picker row");
    picker.handle(left_down(point.0, point.1));
    // Row 1 is the "⇄ Other projects…" action: it drills into the project
    // picker rather than closing (only an item row selects a board).
    assert_eq!(picker.app.screen, Screen::ProjectPicker);
    assert!(picker.app.picker.is_some());

    // An item row (0 = the current project's board) selects and closes.
    let mut picker = setup_picker();
    render_at(&mut picker, 80, 24);
    let (point, _) = hit_zones(&picker, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| match zone {
            Zone::PickerRow(0) => Some((point, 0)),
            _ => None,
        })
        .expect("first picker row");
    picker.handle(left_down(point.0, point.1));
    assert_eq!(picker.app.screen, Screen::Board);
    assert!(
        picker.app.picker.is_none(),
        "row {selected} must activate through Enter"
    );

    let mut no = setup_confirm();
    render_at(&mut no, 80, 24);
    let no_point = hit_zones(&no, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| (zone == Zone::Action(UiAction::ConfirmNo)).then_some(point))
        .expect("No control");
    no.handle(left_down(no_point.0, no_point.1));
    assert_eq!(no.app.screen, Screen::Board);
    assert!(no.app.confirm.is_none());

    let mut yes = setup_confirm();
    render_at(&mut yes, 80, 24);
    let yes_point = hit_zones(&yes, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| (zone == Zone::Action(UiAction::ConfirmYes)).then_some(point))
        .expect("Yes control");
    yes.handle(left_down(yes_point.0, yes_point.1));
    assert_eq!(yes.app.screen, Screen::Board);
    assert!(yes.app.confirm.is_none());
}

#[test]
fn move_column_controls_and_contextual_footer_help_follow_keyboard_paths() {
    let mut moving = setup_board();
    moving.handle(key_msg(KeyCode::Char('M')));
    assert_eq!(moving.app.screen, Screen::MoveColumn);
    render_at(&mut moving, 80, 24);
    let right = hit_zones(&moving, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| {
            (zone == Zone::Action(UiAction::StageColumnRight)).then_some(point)
        })
        .expect("Right control");
    moving.handle(left_down(right.0, right.1));
    assert_eq!(moving.app.screen, Screen::MoveColumn);
    render_at(&mut moving, 80, 24);
    let commit = hit_zones(&moving, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| {
            (zone == Zone::Action(UiAction::CommitColumnMove)).then_some(point)
        })
        .expect("Confirm control");
    moving.handle(left_down(commit.0, commit.1));
    assert_eq!(moving.app.screen, Screen::Board);

    let mut picker = setup_picker();
    render_at(&mut picker, 80, 24);
    let help = hit_zones(&picker, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| (zone == Zone::Action(UiAction::Help)).then_some(point))
        .expect("contextual footer Help");
    picker.handle(left_down(help.0, help.1));
    assert_eq!(picker.app.screen, Screen::Help);
    picker.handle(key_msg(KeyCode::Esc));
    assert_eq!(picker.app.screen, Screen::BoardPicker);
}

#[test]
fn help_pointer_scroll_uses_the_existing_clamped_scroll_reducer() {
    let mut d = setup_help();
    render_at(&mut d, 40, 20);
    assert_eq!(d.app.help_scroll, 0);
    d.handle(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert!(d.app.help_scroll > 0);
    for _ in 0..200 {
        d.handle(mouse(MouseEventKind::ScrollUp, 5, 5));
    }
    assert_eq!(d.app.help_scroll, 0);
}

#[test]
fn form_editor_control_focuses_multiline_field_and_uses_existing_editor_effect() {
    let mut d = driver();
    d.handle(key_msg(KeyCode::Char('n')));
    assert_eq!(d.app.screen, Screen::CardForm);
    render_at(&mut d, 80, 24);
    let (point, idx) = hit_zones(&d, 80, 24)
        .into_iter()
        .find_map(|(point, zone)| match zone {
            Zone::FormEditor(idx) => Some((point, idx)),
            _ => None,
        })
        .expect("visible $EDITOR control");
    d.handle(left_down(point.0, point.1));
    let form = d.app.form.as_ref().unwrap();
    assert_eq!(form.focus, idx);
    assert_eq!(form.fields[idx].get_text(), EDITED);
}

#[test]
fn board_visible_filters_are_independent_click_targets() {
    use board_tui::app::CardFilter;
    for filter in [CardFilter::Active, CardFilter::All, CardFilter::Archived] {
        let mut d = setup_board();
        render_at(&mut d, 80, 24);
        let (x, y) = hit_zones(&d, 80, 24)
            .into_iter()
            .find_map(|(point, zone)| (zone == Zone::Filter(filter)).then_some(point))
            .unwrap_or_else(|| panic!("missing direct {filter:?} filter target"));
        d.handle(left_down(x, y));
        assert_eq!(d.app.card_filter, filter);
    }
}

#[test]
fn board_cards_have_no_edit_delete_controls_or_mouse_hit_zones() {
    for (w, h) in [(40, 20), (80, 24), (120, 35)] {
        let mut d = setup_board();
        let output = render_at(&mut d, w, h);
        assert!(
            !output.contains("[ Edit ]"),
            "Board cards must not render an Edit chip at {w}x{h}:\n{output}"
        );
        assert!(
            !output.contains("[ Delete ]"),
            "Board cards must not render a Delete chip at {w}x{h}:\n{output}"
        );
        assert!(
            hit_zones(&d, w, h).iter().all(|(_, zone)| {
                !matches!(zone, Zone::CardAction { .. })
                    && !matches!(
                        zone,
                        Zone::Action(UiAction::EditCard | UiAction::DeleteCard)
                    )
            }),
            "Board cards must not expose Edit/Delete mouse zones at {w}x{h}"
        );
    }

    // Removing the visual controls must not remove the established keyboard
    // paths: e opens the card form and d opens the confirmation.
    let mut edit = setup_board();
    edit.handle(key_msg(KeyCode::Char('e')));
    assert!(matches!(
        edit.app.form.as_ref().map(|form| form.kind),
        Some(board_tui::forms::FormKind::CardEdit { .. })
    ));

    let mut delete = setup_board();
    delete.handle(key_msg(KeyCode::Char('d')));
    assert_eq!(delete.app.screen, Screen::Confirm);
}

#[test]
fn board_chrome_omits_actions_that_are_keyboard_or_gesture_only() {
    let hidden = [
        UiAction::MoveCard,
        UiAction::ShoveCardLeft,
        UiAction::ShoveCardRight,
        UiAction::Refresh,
        UiAction::Quit,
    ];
    for (w, h) in [(40, 20), (80, 24), (120, 35)] {
        let mut d = setup_board();
        render_at(&mut d, w, h);
        let zones = hit_zones(&d, w, h);
        for action in hidden {
            assert!(
                zones.iter().all(|(_, zone)| *zone != Zone::Action(action)),
                "{action:?} must not be rendered at {w}x{h}"
            );
        }
    }
}

// -- same-column drag reorder through the real mouse path ---------------------

/// Drag the Execute column's first card onto its second card with real mouse
/// events (Down → Drag → Up) and assert the driver persists a same-column
/// reorder through `card.move {column_id, position}`. Execute is the second
/// visible column at 80x24 (26px minimum column width shows three).
#[test]
fn drag_card_onto_another_card_in_the_same_column_reorders() {
    use board_tui::view::board_layout;
    use crossterm::event::MouseButton;

    let mut d = driver();
    d.app.last_area = Rect::new(0, 0, REGULAR.0, REGULAR.1);
    render_at(&mut d, REGULAR.0, REGULAR.1);
    let layout = board_layout(&d.app, Rect::new(0, 0, REGULAR.0, REGULAR.1));
    let col = layout
        .cols
        .iter()
        .find(|c| c.idx == 2)
        .expect("Execute column visible at 80x24");
    let (c0, r0) = col.cards[0];
    let (c1, r1) = col.cards[1];
    let column_id = d.app.col_id_at(2).unwrap();
    let cards = d.app.cards_of(column_id);
    let first = cards[c0].id;
    let second = cards[c1].id;

    // Down on the first card begins the drag; drag onto the second card's row.
    d.handle(left_down(r0.x.saturating_add(1), r0.y.saturating_add(1)));
    assert!(d.app.drag.is_some(), "drag began on mouse down");
    d.handle(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        r1.x.saturating_add(1),
        r1.y.saturating_add(1),
    ));
    d.handle(mouse(
        MouseEventKind::Up(MouseButton::Left),
        r1.x.saturating_add(1),
        r1.y.saturating_add(1),
    ));
    assert!(d.app.drag.is_none(), "drag cleared on mouse up");

    // The driver's fake client applies the reorder and the refetch already
    // landed: the refetched snapshot order must have flipped inside the same
    // column.
    let order: Vec<i64> = d.app.cards_of(column_id).iter().map(|c| c.id).collect();
    assert_eq!(order, vec![second, first], "same-column reorder persisted");
}

/// A drop back on the dragged card's own row stays a no-op (no RPC, order
/// unchanged) — the drag's origin-slot behavior.
#[test]
fn drag_card_dropped_on_its_own_row_is_noop() {
    use board_tui::view::board_layout;
    use crossterm::event::MouseButton;

    let mut d = driver();
    d.app.last_area = Rect::new(0, 0, REGULAR.0, REGULAR.1);
    render_at(&mut d, REGULAR.0, REGULAR.1);
    let layout = board_layout(&d.app, Rect::new(0, 0, REGULAR.0, REGULAR.1));
    let col = layout
        .cols
        .iter()
        .find(|c| c.idx == 2)
        .expect("Execute column visible at 80x24");
    let (_c0, r0) = col.cards[0];
    let column_id = d.app.col_id_at(2).unwrap();
    let before: Vec<i64> = d.app.cards_of(column_id).iter().map(|c| c.id).collect();

    d.handle(left_down(r0.x.saturating_add(1), r0.y.saturating_add(1)));
    d.handle(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        r0.x.saturating_add(1),
        r0.y.saturating_add(3).min(r0.bottom().saturating_sub(1)),
    ));
    d.handle(mouse(
        MouseEventKind::Up(MouseButton::Left),
        r0.x.saturating_add(1),
        r0.y.saturating_add(3).min(r0.bottom().saturating_sub(1)),
    ));

    let after: Vec<i64> = d.app.cards_of(column_id).iter().map(|c| c.id).collect();
    assert_eq!(
        before, after,
        "dropping on the origin slot must not reorder"
    );
}

#[test]
fn card_detail_card_action_button_click_runs_the_action() {
    use board_tui::command::FakeCommand;

    for (w, h) in [(40, 20), (80, 24)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        assert!(!hit_zones_after_render(&mut d, w, h)
            .iter()
            .any(|(_, z)| matches!(z, Zone::Action(UiAction::CardAction(_)))));

        d.app.detail.as_mut().unwrap().actions = vec![board_core::protocol::CardAction {
            label: "Review".into(),
            argv: vec!["review-tool".into()],
        }];
        let (x, y) = hit_zones_after_render(&mut d, w, h)
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::CardAction(0))).then_some(p))
            .unwrap_or_else(|| panic!("{w}x{h}: card action button"));
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(calls.len(), 1, "{w}x{h}");
        assert_eq!(calls[0].argv, vec!["review-tool".to_string()]);
        assert_eq!(d.app.screen, Screen::CardDetail);
    }
}

#[test]
fn card_detail_nine_actions_stay_clickable_in_every_layout() {
    use board_tui::command::FakeCommand;

    for (w, h) in [(40, 20), (80, 24), (120, 40)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        // The card is on `Done` (no space), so the actions are allowed to run
        // from the TUI's own directory.
        d.app.detail.as_mut().unwrap().actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Act {i}"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();

        let text = render_at(&mut d, w, h);
        for i in 1..=9 {
            assert!(
                text.contains(&format!("[ Act {i} ]")),
                "{w}x{h}: action {i} not rendered\n{text}"
            );
        }

        let zones = hit_zones(&d, w, h);
        let action_zones: Vec<u8> = zones
            .iter()
            .filter_map(|(_, z)| match z {
                Zone::Action(UiAction::CardAction(i)) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(
            action_zones.len(),
            9,
            "{w}x{h}: every configured action needs exactly one hit zone: {action_zones:?}"
        );
        // No action hit zone may live outside the allocated rail rect.
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert!(!rail.is_empty(), "{w}x{h}: rail must be allocated");
        for ((x, y), zone) in zones.iter() {
            if matches!(zone, Zone::Action(UiAction::CardAction(_))) {
                assert!(
                    *x >= rail.x
                        && *x < rail.x.saturating_add(rail.width)
                        && *y >= rail.y
                        && *y < rail.y.saturating_add(rail.height),
                    "{w}x{h}: action zone {zone:?} at ({x},{y}) outside rail {rail:?}"
                );
            }
        }

        // Clicking the ninth control runs the ninth argv.
        let (x, y) = zones
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::CardAction(8))).then_some(p))
            .unwrap_or_else(|| panic!("{w}x{h}: ninth action button"));
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(calls.len(), 1, "{w}x{h}");
        assert_eq!(calls[0].argv, vec!["tool-9".to_string()]);
    }
}

#[test]
fn card_detail_nine_long_actions_awaiting_stay_clickable_and_bounded() {
    use board_tui::command::FakeCommand;

    // Awaiting adds `[ Confirm done ]`; long labels force the Regular/Wide
    // rail onto several measured rows. Every control must stay identifiable
    // with its hit zone inside the allocated rail.
    for (w, h) in [(80, 24), (120, 40)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        let detail = d.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Awaiting;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Deploy service {i}"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();

        let text = render_at(&mut d, w, h);
        for i in 1..=9 {
            assert!(
                text.contains(&format!("Deploy service {i}")),
                "{w}x{h}: action {i} not identifiable\n{text}"
            );
        }
        assert!(
            text.contains("[ Confirm"),
            "{w}x{h}: Confirm missing\n{text}"
        );
        assert!(text.contains("[ Edit"), "{w}x{h}: Edit missing\n{text}");

        let zones = hit_zones(&d, w, h);
        let action_zones: Vec<u8> = zones
            .iter()
            .filter_map(|(_, z)| match z {
                Zone::Action(UiAction::CardAction(i)) => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(
            action_zones.len(),
            9,
            "{w}x{h}: every configured action needs exactly one hit zone: {action_zones:?}"
        );
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert!(
            rail.height > 1,
            "{w}x{h}: long nine-action rail must wrap (height {0})",
            rail.height
        );
        // Regular/Wide have room to grow here, so no pager may appear.
        assert!(
            !text.contains("‹") && !text.contains("more"),
            "{w}x{h}: uncapped rail must not show the pager"
        );
        for ((x, y), zone) in zones.iter() {
            if matches!(zone, Zone::Action(UiAction::CardAction(_))) {
                assert!(
                    *x >= rail.x
                        && *x < rail.x.saturating_add(rail.width)
                        && *y >= rail.y
                        && *y < rail.y.saturating_add(rail.height),
                    "{w}x{h}: action zone {zone:?} at ({x},{y}) outside rail {rail:?}"
                );
            }
        }

        let (x, y) = zones
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::CardAction(8))).then_some(p))
            .unwrap_or_else(|| panic!("{w}x{h}: ninth action button"));
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(calls.len(), 1, "{w}x{h}");
        assert_eq!(calls[0].argv, vec!["tool-9".to_string()]);
    }
}

#[test]
fn card_detail_compact_long_actions_overflow_pages_with_mouse_and_keys() {
    use board_tui::command::FakeCommand;

    // Compact + Awaiting + nine long labels exhausts the capped rail: the
    // last rail row shows the `+N more ‹i/n›` pager with clickable `‹`/`›`
    // halves, every page keeps hit zones inside the rail, and paging reveals
    // every label with a mouse target — including the ninth argv and the
    // built-ins. Hidden rows keep no mouse zones; digits work on all pages.
    for (w, h) in [(40, 20), (40, 14)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        let detail = d.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Awaiting;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Deploy service {i} to production"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();

        let text = render_at(&mut d, w, h);
        assert!(
            text.contains("more") && text.contains("‹") && text.contains("›"),
            "{w}x{h}: capped rail must show the pager\n{text}"
        );
        assert_eq!(d.app.detail_rail_page, 0);
        // Built-ins are reachable on the first page: `[ Edit ]` has a zone.
        let zones = hit_zones(&d, w, h);
        let edit_point = zones
            .iter()
            .find_map(|(p, z)| (z == &Zone::Action(UiAction::EditCard)).then_some(*p))
            .unwrap_or_else(|| panic!("{w}x{h}: first page must keep Edit"));
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert!(!rail.is_empty(), "{w}x{h}: rail must be allocated");
        // The pager halves themselves are clickable.
        let next_point = zones
            .iter()
            .find_map(|(p, z)| (z == &Zone::RailPageNext).then_some(*p))
            .unwrap_or_else(|| panic!("{w}x{h}: pager needs a next zone"));
        assert!(
            zones.iter().any(|(_, z)| z == &Zone::RailPagePrev),
            "{w}x{h}: pager needs a prev zone"
        );
        // The ninth action starts with no mouse zone anywhere.
        assert!(
            !zones
                .iter()
                .any(|(_, z)| z == &Zone::Action(UiAction::CardAction(8))),
            "{w}x{h}: hidden ninth action must keep no mouse zone on page 0"
        );
        // Clicking `›` pages forward and reveals new labels.
        d.handle(left_down(next_point.0, next_point.1));
        assert_eq!(d.app.detail_rail_page, 1, "{w}x{h}: pager click must page");
        // The wheel over the rail pages too (back up to the first page).
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        d.handle(board_tui::testkit::mouse(
            MouseEventKind::ScrollUp,
            rail.x + 1,
            rail.y,
        ));
        assert_eq!(d.app.detail_rail_page, 0, "{w}x{h}: rail wheel must page");
        // Page to the last page: the trailing built-ins (`[ Add ]` plus
        // `[ Open ] [ Retry ] [ Cancel ]`, which join the rail here because
        // the Runs section has no room) must all keep mouse zones there.
        for _ in 0..16 {
            let before = d.app.detail_rail_page;
            d.handle(key_msg(KeyCode::Right));
            if d.app.detail_rail_page == before {
                break;
            }
        }
        let last_zones = hit_zones_after_render(&mut d, w, h);
        for (label, action) in [
            ("Add", UiAction::AddComment),
            ("Open", UiAction::FocusRunPane),
            ("Retry", UiAction::RetryRun),
            ("Cancel", UiAction::CancelRun),
        ] {
            assert!(
                last_zones.iter().any(|(_, z)| z == &Zone::Action(action)),
                "{w}x{h}: last page must keep {label} clickable"
            );
        }
        // Page back (bounded) until the ninth control gains a mouse zone
        // inside the rail, then click it: the ninth argv runs by mouse.
        let mut ninth: Option<(u16, u16)> = None;
        for _ in 0..16 {
            let zones = hit_zones_after_render(&mut d, w, h);
            if let Some((p, _)) = zones
                .iter()
                .find(|(_, z)| z == &Zone::Action(UiAction::CardAction(8)))
            {
                ninth = Some(*p);
                break;
            }
            d.handle(key_msg(KeyCode::Left));
        }
        let (x, y) = ninth.unwrap_or_else(|| {
            panic!(
                "{w}x{h}: paging must reveal the ninth mouse zone\n{}",
                render_at(&mut d, w, h)
            )
        });
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert!(
            x >= rail.x
                && x < rail.x.saturating_add(rail.width)
                && y >= rail.y
                && y < rail.y.saturating_add(rail.height),
            "{w}x{h}: ninth zone at ({x},{y}) outside rail {rail:?}"
        );
        assert!(
            render_at(&mut d, w, h).contains("Deploy service 9"),
            "{w}x{h}: ninth label must be revealed before clicking"
        );
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(calls.len(), 1, "{w}x{h}: ninth action click must run");
        assert_eq!(calls[0].argv, vec!["tool-9".to_string()]);
        drop(calls);
        // Every zone on every visited page stayed inside the rail.
        for ((zx, zy), zone) in hit_zones(&d, w, h) {
            if matches!(zone, Zone::Action(UiAction::CardAction(_))) {
                assert!(
                    zx >= rail.x
                        && zx < rail.x.saturating_add(rail.width)
                        && zy >= rail.y
                        && zy < rail.y.saturating_add(rail.height),
                    "{w}x{h}: action zone {zone:?} at ({zx},{zy}) outside rail {rail:?}"
                );
            }
        }
        // Built-in mouse execution at short height: a fresh detail keeps
        // `[ Edit ]` clickable on page 0 and opens the card form.
        let mut e = setup_card_detail();
        e.set_command_launcher(Box::new(FakeCommand::default()));
        let detail = e.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Awaiting;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Deploy service {i} to production"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();
        render_at(&mut e, w, h);
        let (ex, ey) = hit_zones(&e, w, h)
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::EditCard)).then_some(p))
            .unwrap_or_else(|| panic!("{w}x{h}: Edit must stay clickable"));
        // Silence unused-variable clarity: verified above via edit_point.
        let _ = edit_point;
        e.handle(left_down(ex, ey));
        assert_eq!(
            e.app.screen,
            Screen::CardForm,
            "{w}x{h}: clicking Edit must open the card form"
        );
    }
}

#[test]
fn card_detail_compact_single_row_rail_keeps_action_and_pager_clickable() {
    use board_tui::command::FakeCommand;

    // Single-row rail (40x13, or 40x14 with a toast): the row is split between
    // the page's packed row and the pager, so the first action and both pager
    // halves stay clickable, paging reveals the ninth label with an in-bounds
    // zone that runs tool-9 by mouse, and the trailing built-ins stay reachable.
    for (w, h, toast) in [(40, 13, false), (40, 14, true)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        let detail = d.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Awaiting;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Deploy service {i} to production"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();
        if toast {
            d.app
                .set_toast("card action refused: no configured cwd", true);
        }

        let text = render_at(&mut d, w, h);
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert_eq!(
            rail.height, 1,
            "{w}x{h} toast={toast}: single-row rail fixture"
        );
        assert!(
            text.contains("[ Edit"),
            "{w}x{h} toast={toast}: first action must stay visible\n{text}"
        );
        assert!(
            text.contains("more") && text.contains("‹") && text.contains("›"),
            "{w}x{h} toast={toast}: pager must stay visible\n{text}"
        );
        assert!(
            !text.contains("Deploy service 9"),
            "{w}x{h} toast={toast}: ninth action must start hidden\n{text}"
        );
        let zones = hit_zones(&d, w, h);
        let edit_point = zones
            .iter()
            .find_map(|(p, z)| (z == &Zone::Action(UiAction::EditCard)).then_some(*p))
            .unwrap_or_else(|| panic!("{w}x{h} toast={toast}: Edit must stay clickable"));
        assert!(
            edit_point.0 >= rail.x
                && edit_point.0 < rail.x.saturating_add(rail.width)
                && edit_point.1 == rail.y,
            "{w}x{h} toast={toast}: Edit at {edit_point:?} outside rail {rail:?}"
        );
        let next_point = zones
            .iter()
            .find_map(|(p, z)| (z == &Zone::RailPageNext).then_some(*p))
            .unwrap_or_else(|| panic!("{w}x{h} toast={toast}: pager needs a next zone"));
        assert!(
            zones.iter().any(|(_, z)| z == &Zone::RailPagePrev),
            "{w}x{h} toast={toast}: pager needs a prev zone"
        );
        assert!(
            !zones
                .iter()
                .any(|(_, z)| z == &Zone::Action(UiAction::CardAction(8))),
            "{w}x{h} toast={toast}: hidden ninth must keep no zone on page 0"
        );

        // Clicking › pages forward; the wheel pages back.
        d.handle(left_down(next_point.0, next_point.1));
        assert_eq!(
            d.app.detail_rail_page, 1,
            "{w}x{h} toast={toast}: pager click must page"
        );
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        d.handle(board_tui::testkit::mouse(
            MouseEventKind::ScrollUp,
            rail.x + 1,
            rail.y,
        ));
        assert_eq!(
            d.app.detail_rail_page, 0,
            "{w}x{h} toast={toast}: rail wheel must page"
        );

        // Single-row pages are repacked for the 22-cell action half, so the
        // trailing run controls no longer share one squeezed row: each keeps
        // a labelled, in-rail zone on its own page. Scan every page and
        // require each label + zone to appear together somewhere.
        for _ in 0..32 {
            let before = d.app.detail_rail_page;
            d.handle(key_msg(KeyCode::Right));
            if d.app.detail_rail_page == before {
                break;
            }
        }
        let last_page = d.app.detail_rail_page;
        for _ in 0..32 {
            d.handle(key_msg(KeyCode::Left));
            if d.app.detail_rail_page == 0 {
                break;
            }
        }
        let mut seen: Vec<(Zone, String, (u16, u16))> = Vec::new();
        for _ in 0..=last_page {
            let text = render_at(&mut d, w, h);
            let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
            for (pt, zone) in hit_zones(&d, w, h) {
                if matches!(
                    zone,
                    Zone::Action(
                        UiAction::AddComment
                            | UiAction::FocusRunPane
                            | UiAction::RetryRun
                            | UiAction::CancelRun
                    )
                ) && pt.1 == rail.y
                    && pt.0 >= rail.x
                    && pt.0 < rail.x.saturating_add(rail.width)
                    && !seen.iter().any(|(z, _, _)| *z == zone)
                {
                    seen.push((zone, text.clone(), pt));
                }
            }
            d.handle(key_msg(KeyCode::Right));
        }
        for (label, action) in [
            ("Open", UiAction::FocusRunPane),
            ("Retry", UiAction::RetryRun),
            ("Cancel", UiAction::CancelRun),
        ] {
            let (_, text, pt) = seen
                .iter()
                .find(|(z, _, _)| *z == Zone::Action(action))
                .unwrap_or_else(|| {
                    panic!(
                        "{w}x{h} toast={toast}: {label} must be reachable on some single-row page"
                    )
                });
            assert!(
                text.contains(&format!("[ {label}")),
                "{w}x{h} toast={toast}: {label} zone at {pt:?} needs its label visible\n{text}"
            );
        }

        // Page back until the ninth control gains an in-bounds zone, then
        // click it: the ninth argv runs by mouse with its label revealed.
        let mut ninth: Option<(u16, u16)> = None;
        for _ in 0..32 {
            let zones = hit_zones_after_render(&mut d, w, h);
            if let Some((p, _)) = zones
                .iter()
                .find(|(_, z)| z == &Zone::Action(UiAction::CardAction(8)))
            {
                ninth = Some(*p);
                break;
            }
            d.handle(key_msg(KeyCode::Left));
        }
        let (x, y) = ninth.unwrap_or_else(|| {
            panic!(
                "{w}x{h} toast={toast}: paging must reveal the ninth mouse zone\n{}",
                render_at(&mut d, w, h)
            )
        });
        let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
        assert!(
            x >= rail.x
                && x < rail.x.saturating_add(rail.width)
                && y >= rail.y
                && y < rail.y.saturating_add(rail.height),
            "{w}x{h} toast={toast}: ninth zone at ({x},{y}) outside rail {rail:?}"
        );
        assert!(
            render_at(&mut d, w, h).contains("Deploy service 9"),
            "{w}x{h} toast={toast}: ninth label must be revealed before clicking"
        );
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(
            calls.len(),
            1,
            "{w}x{h} toast={toast}: ninth click must run"
        );
        assert_eq!(calls[0].argv, vec!["tool-9".to_string()]);
        drop(calls);

        // Built-in mouse execution at single-row height: a fresh detail keeps
        // `[ Edit ]` clickable on page 0 and opens the card form.
        let mut e = setup_card_detail();
        e.set_command_launcher(Box::new(FakeCommand::default()));
        let detail = e.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Awaiting;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("Deploy service {i} to production"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();
        if toast {
            e.app
                .set_toast("card action refused: no configured cwd", true);
        }
        render_at(&mut e, w, h);
        let (ex, ey) = hit_zones(&e, w, h)
            .into_iter()
            .find_map(|(p, z)| (z == Zone::Action(UiAction::EditCard)).then_some(p))
            .unwrap_or_else(|| panic!("{w}x{h} toast={toast}: Edit must stay clickable"));
        e.handle(left_down(ex, ey));
        assert_eq!(
            e.app.screen,
            Screen::CardForm,
            "{w}x{h} toast={toast}: clicking Edit must open the card form"
        );
    }
}
#[test]
fn card_detail_compact_single_row_short_labels_stay_clickable() {
    use board_tui::command::FakeCommand;
    for (w, h, toast) in [(40, 13, false), (40, 14, true)] {
        let mut d = setup_card_detail();
        let commands = FakeCommand::default();
        d.set_command_launcher(Box::new(commands.clone()));
        let detail = d.app.detail.as_mut().unwrap();
        detail.card.status = board_core::protocol::CardStatus::Done;
        detail.card.space_ref = None;
        detail.card.space_cwd = None;
        detail.actions = (1..=9)
            .map(|i| board_core::protocol::CardAction {
                label: format!("A{i}"),
                argv: vec![format!("tool-{i}")],
            })
            .collect();
        if toast {
            d.app
                .set_toast("card action refused: no configured cwd", true);
        }
        let text = render_at(&mut d, w, h);
        let layout = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h));
        let rail = layout.card_actions;
        assert_eq!(rail.height, 1, "{w}x{h} toast={toast}: single-row rail");
        let (action_rect, pager_rect) = board_tui::view::rail_single_row_split(rail);
        assert_eq!(action_rect.width, 22);
        let single_owned: Vec<Vec<(String, UiAction)>> =
            board_tui::view::detail_rail_single_row_rows(
                d.app.detail.as_ref().unwrap(),
                &layout,
                true,
                action_rect.width,
            )
            .iter()
            .map(|row| {
                row.iter()
                    .map(|b| (b.compact_label.to_string(), b.action))
                    .collect()
            })
            .collect();
        assert!(single_owned.len() > 1);
        assert!(
            text.contains("[ Edit"),
            "{w}x{h} toast={toast}: Edit visible\n{text}"
        );
        assert!(
            text.contains("more") && text.contains("\u{2039}") && text.contains("\u{203a}"),
            "{w}x{h} toast={toast}: pager visible\n{text}"
        );
        let rail_row = text.lines().nth(rail.y as usize).unwrap_or("");
        assert!(!rail_row.contains("[  ]"), "blank chip\n{rail_row}");
        assert!(
            !rail_row.contains("[ \u{2026} ]") && !rail_row.contains("[ \u{2026}"),
            "anonymous chip\n{rail_row}"
        );
        let mut seen: Vec<String> = Vec::new();
        let last_page = {
            let mut last = 0;
            for _ in 0..32 {
                let before = d.app.detail_rail_page;
                d.handle(key_msg(KeyCode::Right));
                if d.app.detail_rail_page == before {
                    break;
                }
                last = d.app.detail_rail_page;
            }
            for _ in 0..32 {
                d.handle(key_msg(KeyCode::Left));
                if d.app.detail_rail_page == 0 {
                    break;
                }
            }
            last
        };
        let mut ninth_point: Option<(u16, u16)> = None;
        for page in 0..=last_page {
            let t = render_at(&mut d, w, h);
            let rail = board_tui::view::detail_layout(&d.app, Rect::new(0, 0, w, h)).card_actions;
            let rail_row = t.lines().nth(rail.y as usize).unwrap_or("");
            assert!(!rail_row.contains("[  ]"), "blank page {page}\n{rail_row}");
            assert!(
                !rail_row.contains("[ \u{2026} ]"),
                "anonymous page {page}\n{rail_row}"
            );
            let zones = hit_zones(&d, w, h);
            let expected = single_owned[d.app.detail_rail_page].clone();
            for (label, _action) in &expected {
                let want = format!("[ {label} ]");
                assert!(t.contains(&want), "{want:?} missing page {page}\n{t}");
                if label.starts_with('A') && !seen.contains(label) {
                    seen.push(label.clone());
                }
            }
            for (pt, zone) in &zones {
                if pt.1 != rail.y || pt.0 < rail.x || pt.0 >= rail.x + rail.width {
                    continue;
                }
                match zone {
                    Zone::Action(a) => {
                        assert!(
                            pt.0 >= action_rect.x && pt.0 < action_rect.x + action_rect.width,
                            "action {a:?} at {pt:?} outside {action_rect:?}"
                        );
                        let owned_has = expected.iter().any(|(_, ea)| ea == a);
                        assert!(owned_has, "zone {a:?} not on page {page}");
                        let label = expected
                            .iter()
                            .find(|(_, ea)| ea == a)
                            .map(|(l, _)| l.clone())
                            .unwrap();
                        assert!(
                            rail_row.contains(label.as_str()),
                            "zone {a:?} without label page {page}\n{rail_row}"
                        );
                        if *a == UiAction::CardAction(8) && ninth_point.is_none() {
                            ninth_point = Some(*pt);
                        }
                    }
                    Zone::RailPagePrev | Zone::RailPageNext => {
                        assert!(
                            pt.0 >= pager_rect.x && pt.0 < pager_rect.x + pager_rect.width,
                            "pager {zone:?} at {pt:?} outside {pager_rect:?}"
                        );
                    }
                    _ => {}
                }
            }
            let rail_actions: Vec<Zone> = zones
                .iter()
                .filter(|(pt, z)| {
                    pt.1 == rail.y
                        && pt.0 >= action_rect.x
                        && pt.0 < action_rect.x + action_rect.width
                        && matches!(z, Zone::Action(_))
                })
                .map(|(_, z)| *z)
                .collect();
            assert_eq!(rail_actions.len(), expected.len());
            d.handle(key_msg(KeyCode::Right));
        }
        for i in 1..=9 {
            assert!(seen.contains(&format!("A{i}")), "A{i} never visible");
        }
        assert!(
            ninth_point.is_some(),
            "{w}x{h} toast={toast}: ninth must gain a zone on its page"
        );
        // Page back until the ninth zone is current, then click it with its
        // label freshly verified visible.
        let (x, y) = loop {
            let zones = hit_zones_after_render(&mut d, w, h);
            if let Some((pt, _)) = zones
                .iter()
                .find(|(_, z)| *z == Zone::Action(UiAction::CardAction(8)))
            {
                let text = render_at(&mut d, w, h);
                assert!(
                    text.contains("[ A9 ]"),
                    "{w}x{h} toast={toast}: ninth label visible before click\n{text}"
                );
                break *pt;
            }
            d.handle(key_msg(KeyCode::Left));
        };
        d.handle(left_down(x, y));
        let calls = commands.calls.borrow();
        assert_eq!(calls.len(), 1, "ninth click must run");
        assert_eq!(calls[0].argv, vec!["tool-9".to_string()]);
        drop(calls);
    }
}
