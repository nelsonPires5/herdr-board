//! Pure layout behavior for the mobile-responsive board: `LayoutMode`
//! breakpoints, the Compact single-column layout (card height, header zones,
//! vertical scroll clamping), and the `sheet_area` footer-overlap invariant.
//! No snapshots, no rendering — everything here calls `board_layout` /
//! `sheet_area` directly against a pure `App`.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::CardCreateParams;
use board_tui::app::App;
use board_tui::view::{board_layout, sheet_area, LayoutMode};
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
fn compact_card_height_3_for_short_title_4_for_wrapped_title() {
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
    assert_eq!(r0.height, 3, "single-line title -> 3 rows");
    assert_eq!(ci1, 1);
    assert_eq!(r1.height, 4, "wrapped title -> 4 rows");
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
    for &h in &[8u16, 12, 20, 30] {
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
    let area = Rect::new(0, 0, 40, 10);

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
    // Compact: main_area height 3, header 2 -> inner_h 1 < COMPACT_CARD_H (4).
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
fn sheet_area_never_overlaps_the_footer_row_in_any_mode() {
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
                    // `main_area` (private) is exactly `area` minus the 1-row
                    // footer; the invariant restated without calling it.
                    let footer_top = area.y + area.height.saturating_sub(1);
                    assert!(
                        rect.y + rect.height <= footer_top,
                        "mode={mode:?} w={w} h={h} pref=({pw},{ph}) rect={rect:?} \
                         footer_top={footer_top}"
                    );
                }
            }
        }
    }
}

// -- scrollbar presence ---------------------------------------------------------

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
