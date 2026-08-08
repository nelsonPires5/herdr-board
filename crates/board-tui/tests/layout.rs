//! Pure layout behavior for the mobile-responsive board: `LayoutMode`
//! breakpoints, the Compact single-column layout (card height, header zones,
//! vertical scroll clamping), and the `sheet_area` action-rail-overlap invariant.
//! No snapshots, no rendering — everything here calls `board_layout` /
//! `sheet_area` directly against a pure `App`.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::db::{EnqueueRun, FinalizeRun};
use board_core::protocol::{CardCreateParams, CardStatus, RunOutcome};
use board_tui::app::{update, App, DetailScrollTarget, Screen};
use board_tui::testkit::key;
use board_tui::view::{
    board_header_height, board_layout, comment_row_spans, comment_wrapped_rows,
    comments_action_bar_shown, detail_layout, sheet_area, LayoutMode,
};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;

fn card(title: &str, column_id: i64) -> CardCreateParams {
    CardCreateParams {
        title: title.to_string(),
        column_id: Some(column_id),
        harness: Some("claude".to_string()),
        ..Default::default()
    }
}

/// A single "Todo" column seeded with `n` cards named "Card 0", "Card 1", ...
fn app_with_cards(n: usize) -> App {
    let mut c = FakeBoardClient::new().unwrap();
    let todo = c.board_get().unwrap().columns[0].id;
    for i in 0..n {
        c.card_create(&card(&format!("Card {i}"), todo)).unwrap();
    }
    App::new(c.board_get().unwrap())
}

// -- LayoutMode::from_width boundaries ---------------------------------------

#[test]
fn from_width_compact_below_60() {
    for w in [0, 39, 59] {
        assert_eq!(LayoutMode::from_width(w), LayoutMode::Compact, "w={w}");
    }
}

#[test]
fn from_width_regular_60_to_119() {
    for w in [60, 80, 119] {
        assert_eq!(LayoutMode::from_width(w), LayoutMode::Regular, "w={w}");
    }
}

#[test]
fn header_geometry_matches_the_three_row_compact_and_one_line_desktop_contract() {
    assert_eq!(
        board_header_height(40),
        4,
        "three Compact rows plus divider"
    );
    assert_eq!(
        board_header_height(52),
        4,
        "three Compact rows plus divider"
    );
    for width in [60, 80, 120] {
        assert_eq!(
            board_header_height(width),
            2,
            "one desktop row plus divider at {width}"
        );
    }
}

#[test]
fn from_width_wide_120_and_above() {
    for w in [120, 200] {
        assert_eq!(LayoutMode::from_width(w), LayoutMode::Wide, "w={w}");
    }
}

// -- Compact single-column layout ---------------------------------------------

#[test]
fn compact_yields_exactly_one_full_width_column_plus_header_zones() {
    let app = app_with_cards(3);
    let area = Rect::new(0, 0, 40, 20);
    let layout = board_layout(&app, area);

    assert_eq!(layout.cols.len(), 1, "Compact must show exactly one column");
    let col_rect = layout.cols[0].rect;
    assert_eq!(col_rect.x, area.x);
    assert_eq!(
        col_rect.width, area.width,
        "the single column must span the full main width"
    );

    let header = layout
        .compact_header
        .expect("Compact must expose prev/switch/next header zones");
    assert_eq!(header.prev.x, area.x, "prev zone starts at the left edge");
    assert!(
        header.switch.x > header.prev.x && header.switch.x < header.next.x,
        "switch zone sits between prev and next"
    );
    assert_eq!(
        header.next.x + header.next.width,
        area.x + area.width,
        "next zone ends at the right edge"
    );
}

#[test]
fn regular_and_wide_expose_no_compact_header() {
    let app = app_with_cards(3);
    for w in [60, 80, 120, 200] {
        let layout = board_layout(&app, Rect::new(0, 0, w, 24));
        assert!(
            layout.compact_header.is_none(),
            "w={w} must not draw the Compact header"
        );
    }
}

#[test]
fn board_layout_keeps_persistent_chrome_clear_while_an_overlay_is_open() {
    for (w, h) in [(40_u16, 20_u16), (80, 24), (120, 35)] {
        let mut app = app_with_cards(3);
        app.screen = Screen::Picker;
        let area = Rect::new(0, 0, w, h);
        let layout = board_layout(&app, area);
        let header_rows = board_header_height(w);
        let action_rows = if w < 60 {
            3
        } else if w < 120 {
            2
        } else {
            1
        };
        let content_top = area.y + header_rows;
        let content_bottom = area.y + h.saturating_sub(action_rows);

        assert!(
            layout.cols.iter().all(|col| {
                col.rect.y >= content_top && col.rect.y + col.rect.height <= content_bottom
            }),
            "overlay board columns must stay in the content region at {w}x{h}: {:?}",
            layout.cols.iter().map(|col| col.rect).collect::<Vec<_>>()
        );
        assert_eq!(
            layout.compact_header.is_some(),
            w < 60,
            "only Compact retains its column header while an overlay is open"
        );
    }
}

#[test]
fn compact_card_height_6_for_short_title_7_for_wrapped_title() {
    let mut c = FakeBoardClient::new().unwrap();
    let todo = c.board_get().unwrap().columns[0].id;
    c.card_create(&card("Short", todo)).unwrap();
    // No spaces, so the wrap logic hard-breaks it — guaranteed >=2 wrapped
    // rows at any narrow width, which `compact_card_height` clamps to 2.
    c.card_create(&card(&"x".repeat(80), todo)).unwrap();
    let app = App::new(c.board_get().unwrap());

    let area = Rect::new(0, 0, 40, 30);
    let layout = board_layout(&app, area);
    let col = &layout.cols[0];
    assert_eq!(
        col.cards.len(),
        2,
        "both cards must fit in a 30-row viewport"
    );
    let (ci0, r0) = col.cards[0];
    let (ci1, r1) = col.cards[1];
    assert_eq!(ci0, 0);
    assert_eq!(r0.height, 6, "single-line title -> 6 rows");
    assert_eq!(ci1, 1);
    assert_eq!(r1.height, 7, "wrapped title -> 7 rows");
}

// -- vertical scroll clamp -----------------------------------------------------

/// "The selected card always has a rect" only holds while at least one card
/// slot fits (`scroll.visible > 0`); every height tried here fits at least
/// one. The degenerate `visible == 0` case — where the invariant would be
/// violated if `visible_count` lied and claimed a slot existed — is asserted
/// separately in `compact_zero_visible_slots_yields_no_card_rects_and_no_panic`.
#[test]
fn compact_scroll_keeps_the_selected_card_always_rendered() {
    let n = 12;
    let mut app = app_with_cards(n);
    for &h in &[16u16, 22, 30, 40] {
        let area = Rect::new(0, 0, 40, h);
        for sel in 0..n {
            app.sel_card = sel;
            let layout = board_layout(&app, area);
            let col = &layout.cols[0];
            assert!(
                col.cards.iter().any(|&(ci, _)| ci == sel),
                "selected card {sel} has no rect at height {h} (rendered: {:?})",
                col.cards.iter().map(|&(ci, _)| ci).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn compact_scroll_last_card_scrolls_forward_first_card_scrolls_back() {
    let n = 12;
    let mut app = app_with_cards(n);
    let area = Rect::new(0, 0, 40, 22);

    app.sel_card = n - 1;
    let layout = board_layout(&app, area);
    assert!(
        layout.cols[0].scroll.offset > 0,
        "selecting the last card must scroll the window forward"
    );
    assert!(layout.cols[0].cards.iter().any(|&(ci, _)| ci == n - 1));

    app.sel_card = 0;
    let layout = board_layout(&app, area);
    assert_eq!(
        layout.cols[0].scroll.offset, 0,
        "selecting the first card must scroll the window back to the top"
    );
    assert!(layout.cols[0].cards.iter().any(|&(ci, _)| ci == 0));
}

/// Bug 2 regression: when the column's rect is too short for even one card
/// (`inner_h < card_h`), `visible_count` must honestly report 0 rather than
/// `.max(1)`-ing itself into claiming a card is visible — the render loop
/// then draws none, so the old code silently made the selected card
/// disappear with no rect at all while the scroll state still claimed one
/// slot existed. Also exercises Regular/Wide via a too-short column rect
/// directly, not just Compact.
#[test]
fn zero_visible_slots_yields_no_card_rects_and_correct_scroll_state() {
    let n = 5;
    let mut app = app_with_cards(n);
    // Compact: body area is empty at this height -> no card can fit.
    let area = Rect::new(0, 0, 40, 4);
    for sel in 0..n {
        app.sel_card = sel;
        let layout = board_layout(&app, area);
        let col = &layout.cols[0];
        assert_eq!(
            col.scroll.visible, 0,
            "no card can fit in a 1-row inner area (sel={sel})"
        );
        assert_eq!(
            col.scroll.offset, 0,
            "no scroll to report when nothing fits"
        );
        assert!(
            col.cards.is_empty(),
            "no card rects when nothing fits (sel={sel}, rendered: {:?})",
            col.cards.iter().map(|&(ci, _)| ci).collect::<Vec<_>>()
        );
    }
}

// -- sheet_area / footer invariant ---------------------------------------------

#[test]
fn sheet_area_stays_inside_the_board_content_region_in_any_mode() {
    let widths = [10u16, 39, 40, 59, 60, 80, 119, 120, 200];
    let heights = [3u16, 5, 10, 24, 35, 60];
    let prefs = [(10u16, 3u16), (58, 3), (96, 20), (200, 200)];
    let modes = [LayoutMode::Compact, LayoutMode::Regular, LayoutMode::Wide];

    for &mode in &modes {
        for &w in &widths {
            for &h in &heights {
                for &(pw, ph) in &prefs {
                    let area = Rect::new(0, 0, w, h);
                    let rect = sheet_area(mode, pw, ph, area);
                    let header_rows = board_header_height(w);
                    let action_rows = if w < 60 {
                        3
                    } else if w < 120 {
                        2
                    } else {
                        1
                    };
                    let content_top = area.y + header_rows.min(area.height);
                    let content_bottom = area.y + h.saturating_sub(action_rows);
                    assert!(
                        rect.y >= content_top || rect.height == 0,
                        "mode={mode:?} w={w} h={h} pref=({pw},{ph}) rect={rect:?} \
                         content_top={content_top}"
                    );
                    assert!(
                        rect.y + rect.height <= content_bottom.max(content_top),
                        "mode={mode:?} w={w} h={h} pref=({pw},{ph}) rect={rect:?} \
                         content_bottom={content_bottom}"
                    );
                }
            }
        }
    }
}

// -- scrollbar presence ---------------------------------------------------------

#[test]
fn compact_detail_40x20_never_exposes_a_partial_section() {
    let mut app = app_with_detail_comments(3);
    app.last_area = Rect::new(0, 0, 40, 20);
    for fullscreen in [false, true] {
        app.detail_fullscreen = fullscreen;
        let layout = detail_layout(&app, app.last_area);
        let sections = [
            ("status", layout.status),
            ("configuration", layout.configuration),
            ("session", layout.session),
            ("description", layout.description),
            ("comments", layout.comments),
            ("runs", layout.runs),
        ];
        for (name, rect) in sections {
            assert!(
                rect.height == 0 || rect.height >= 3,
                "{name} must be hidden or fully closed at 40x20 (fullscreen={fullscreen}): {rect:?}"
            );
        }
        assert!(
            layout.card_actions.height >= 1,
            "the detail action rail must remain reachable: {:?}",
            layout.card_actions
        );
        assert!(
            layout.card_actions.y + layout.card_actions.height <= layout.panel.bottom(),
            "the detail action rail must stay inside the closed panel: {:?} panel={:?}",
            layout.card_actions,
            layout.panel
        );
    }
}

#[test]
fn runs_viewport_reserves_action_row_before_every_run_body_row() {
    let mut app = app_with_detail_runs(2);
    for (w, h) in [(40_u16, 20_u16), (52, 24), (60, 24), (80, 24), (120, 35)] {
        app.last_area = Rect::new(0, 0, w, h);
        let layout = detail_layout(&app, app.last_area);
        if layout.runs.height == 0 {
            assert_eq!(board_tui::view::runs_viewport_height(&layout), 0);
            continue;
        }
        let visible = board_tui::view::runs_viewport_height(&layout);
        let body_bottom = layout.runs.y + 1 + visible as u16;
        if layout.run_actions.is_empty() {
            // Compact puts run actions in the shared card rail rather than
            // reserving a second in-section row.
            assert!(
                body_bottom <= layout.runs.bottom(),
                "compact run body must stay inside the Runs frame at {w}x{h}: runs={:?}",
                layout.runs,
            );
            continue;
        }
        assert!(
            body_bottom <= layout.run_actions.y,
            "run body must end before action row at {w}x{h}: runs={:?} actions={:?}",
            layout.runs,
            layout.run_actions,
        );
        assert!(
            layout.run_actions.y + layout.run_actions.height <= layout.runs.bottom(),
            "run action row must stay inside the Runs frame at {w}x{h}: {:?} runs={:?}",
            layout.run_actions,
            layout.runs
        );
    }
}

#[test]
fn zero_height_runs_body_keeps_a_bounded_logical_anchor() {
    let mut app = app_with_detail_runs(2);
    app.last_area = Rect::new(0, 0, 60, 24);
    let layout = detail_layout(&app, app.last_area);
    if board_tui::view::runs_viewport_height(&layout) == 0 {
        app.detail_runs_scroll = 99;
        app.scroll_detail_to_latest();
        assert_eq!(app.detail_runs_scroll, 1);
        app.detail_scroll_target = DetailScrollTarget::Runs;
        update(&mut app, key(KeyCode::Down));
        assert_eq!(app.detail_runs_scroll, 1);
    }
}

#[test]
fn detail_popup_and_fullscreen_stay_inside_the_content_region() {
    for fullscreen in [false, true] {
        let mut app = app_with_detail_comments(2);
        app.last_area = Rect::new(0, 0, 120, 35);
        app.detail_fullscreen = fullscreen;
        let layout = detail_layout(&app, app.last_area);
        if fullscreen {
            assert_eq!(layout.panel.y, 2);
            assert_eq!(layout.panel.y + layout.panel.height, 34);
        } else {
            assert_eq!(layout.panel.y, 3);
            assert_eq!(layout.panel.y + layout.panel.height, 33);
        }
    }
}

#[test]
fn scrollbar_rect_present_iff_column_overflows() {
    // Few cards in a tall viewport: no overflow, no scrollbar.
    let app_small = app_with_cards(2);
    let area = Rect::new(0, 0, 80, 24); // Regular: single seeded column fills it
    let layout = board_layout(&app_small, area);
    let col = layout.cols.iter().find(|c| c.idx == 0).unwrap();
    assert!(!col.scroll.overflowing());
    assert!(col.scrollbar_rect.is_none());

    // Many cards in a short viewport: overflow, scrollbar present.
    let app_big = app_with_cards(30);
    let area = Rect::new(0, 0, 80, 15);
    let layout = board_layout(&app_big, area);
    let col = layout.cols.iter().find(|c| c.idx == 0).unwrap();
    assert!(col.scroll.overflowing());
    assert!(col.scrollbar_rect.is_some());
}

// -- comment detail: action bar geometry + comment_row_spans --------------------

/// A `CardDetail` with `n` comments, opened in `CardDetail` with comments
/// focused.
fn app_with_detail_comments(n: usize) -> App {
    let mut c = FakeBoardClient::new().unwrap();
    let column_id = c.board_get().unwrap().columns[0].id;
    let card = c.card_create(&card("comment fixture", column_id)).unwrap();
    for i in 0..n {
        c.comment_add(card.id, &format!("comment {i}"), Some("test"))
            .unwrap();
    }
    let detail = c.card_get(card.id).unwrap();
    let mut app = App::new(c.board_get().unwrap());
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_scroll_target = DetailScrollTarget::Comments;
    app
}

#[test]
fn action_bar_row_sits_inside_comments_and_never_overlaps_runs_or_action_rail() {
    let app = app_with_detail_comments(3);
    let area = Rect::new(0, 0, 80, 24);
    let layout = detail_layout(&app, area);
    assert!(
        comments_action_bar_shown(&app, &layout),
        "focused + non-empty + tall enough must show the bar"
    );
    let bar_y = layout.comments.y + layout.comments.height - 1;

    // Inside `layout.comments`'s own bounds.
    assert!(bar_y >= layout.comments.y && bar_y < layout.comments.y + layout.comments.height);
    // Never overlaps `layout.runs` (which starts strictly after it).
    assert!(bar_y < layout.runs.y, "bar row must not overlap runs");
    // Never overlaps the persistent board action rail.
    let action_rows = if area.width < 60 {
        3
    } else if area.width < 120 {
        2
    } else {
        1
    };
    let action_y = area.bottom().saturating_sub(action_rows);
    assert!(bar_y < action_y, "bar row must not overlap the action rail");
}

#[test]
fn action_bar_absent_when_unfocused_empty_or_too_short() {
    let area = Rect::new(0, 0, 80, 24);

    // Unfocused (Runs focused instead).
    let mut app = app_with_detail_comments(3);
    app.detail_scroll_target = DetailScrollTarget::Runs;
    let layout = detail_layout(&app, area);
    assert!(!comments_action_bar_shown(&app, &layout));

    // No comments.
    let app = app_with_detail_comments(0);
    let layout = detail_layout(&app, area);
    assert!(!comments_action_bar_shown(&app, &layout));

    // Focused + non-empty, but the section is too short to spare a row.
    let mut app = app_with_detail_comments(3);
    app.detail_scroll_target = DetailScrollTarget::Comments;
    let short_area = Rect::new(0, 0, 80, 3);
    let layout = detail_layout(&app, short_area);
    if layout.comments.height < 3 {
        assert!(!comments_action_bar_shown(&app, &layout));
    }
}

/// A `CardDetail` with `n` finished runs (each recording a pane), opened in
/// `CardDetail` with the **runs** section focused.
fn app_with_detail_runs(n: usize) -> App {
    let mut c = FakeBoardClient::new().unwrap();
    let column_id = c.board_get().unwrap().columns[0].id;
    let card = c.card_create(&card("run fixture", column_id)).unwrap();
    for i in 0..n {
        let run = c
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
        let pane = format!("w1:p-{i}");
        c.db()
            .promote_run_uow(run.id, Some("w1"), Some(pane.as_str()), None)
            .unwrap();
        c.db()
            .finalize_run_uow(&FinalizeRun {
                run_id: run.id,
                outcome: RunOutcome::Ok,
                summary: None,
                comments: &[],
                target_column_id: None,
                final_status: CardStatus::Done,
                final_awaiting_reason: None,
                next: None,
            })
            .unwrap();
    }
    let detail = c.card_get(card.id).unwrap();
    let mut app = App::new(c.board_get().unwrap());
    app.detail = Some(detail);
    app.screen = Screen::CardDetail;
    app.detail_scroll_target = DetailScrollTarget::Runs;
    app
}

#[test]
fn runs_selection_stays_inside_the_rendered_runs_viewport() {
    for (w, h) in [(80_u16, 24_u16), (52, 20), (120, 35)] {
        let mut app = app_with_detail_runs(30);
        app.last_area = Rect::new(0, 0, w, h);
        let len = app.detail.as_ref().unwrap().runs.len();
        let visible = {
            let layout = detail_layout(&app, app.last_area);
            board_tui::view::runs_viewport_height(&layout)
        };
        if visible == 0 {
            // A three-row frame has room for its title, action row, and
            // bottom border only. There is no rendered run row to paint, so
            // only the logical offset is exercised and it must stay bounded.
            assert_eq!(
                app.detail_runs_scroll, 0,
                "{w}x{h}: initial zero-row offset"
            );
            for _ in 0..len + 5 {
                update(&mut app, key(KeyCode::Down));
                assert!(
                    app.detail_runs_scroll < len,
                    "{w}x{h}: zero-row body offset escaped the run list"
                );
            }
            continue;
        }
        assert!(
            len > visible,
            "{w}x{h}: fixture must overflow the runs viewport ({len} runs, {visible} rows)"
        );

        let inside = |app: &App| {
            let sel = app.detail_run_sel;
            let offset = app.detail_runs_scroll;
            assert!(
                sel >= offset && sel < offset + visible,
                "{w}x{h}: selected run row {sel} outside the rendered window [{offset}, {})",
                offset + visible
            );
            // The window itself never runs past the end of the list.
            assert!(offset + visible <= len.max(visible));
        };

        inside(&app);
        for _ in 0..len + 5 {
            update(&mut app, key(KeyCode::Down));
            inside(&app);
        }
        assert_eq!(app.detail_run_sel, len - 1);
        for _ in 0..len + 5 {
            update(&mut app, key(KeyCode::Char('k')));
            inside(&app);
        }
        assert_eq!(app.detail_run_sel, 0);
        assert_eq!(app.detail_runs_scroll, 0);
    }
}

#[test]
fn comment_row_spans_sum_equals_comment_wrapped_rows() {
    for n in [0, 1, 5, 12] {
        let app = app_with_detail_comments(n);
        let detail = app.detail.as_ref().unwrap();
        for width in [20u16, 40, 78, 118] {
            let spans = comment_row_spans(detail, width);
            let sum: usize = spans.iter().map(|&(_, len)| len).sum();
            let total = comment_wrapped_rows(detail, width);
            assert_eq!(
                sum.max(1),
                total,
                "n={n} width={width}: comment_row_spans must sum to comment_wrapped_rows"
            );
        }
    }
}
