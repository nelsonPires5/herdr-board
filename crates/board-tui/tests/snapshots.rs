//! ratatui `TestBackend` + `insta` snapshots driven through the real `Driver`
//! and `FakeBoardClient`. Everything is deterministic: a fixed `now`, fixed
//! terminal sizes, and running-card timers pinned by rewriting the active-run
//! summary start time.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::db::{EnqueueRun, FinalizeRun};
use board_core::protocol::parse_timestamp;
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, Effort, RunOutcome, SpaceKind,
};
use board_tui::app::{App, CardFilter, Msg, Screen, SwitcherLevel, SwitcherState, Toast};
use board_tui::forms::{FieldId, FieldKind};
use board_tui::testkit::{demo_client, driver_with_origin, hostile_origin, DemoClient};
use board_tui::widgets::Zone;
use board_tui::Driver;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

const NOW_STR: &str = "2026-07-14 12:00:00";
const RUN_START: &str = "2026-07-14 11:58:00"; // 2m before NOW

fn now() -> i64 {
    parse_timestamp(NOW_STR).unwrap()
}

/// Pin `now` and rewrite active-run starts so timers are stable (a board fetch
/// resets them, so callers re-run this right before rendering).
fn pin(app: &mut App) {
    app.now = now();
    for run in &mut app.board.active_runs {
        run.started_at = RUN_START.to_string();
    }
    for c in &mut app.board.cards {
        if c.status == CardStatus::Running {
            // Deliberately disagree with the summary: ordinary card activity
            // must not reset the run timer.
            c.updated_at = NOW_STR.to_string();
        }
    }
    if let Some(detail) = &mut app.detail {
        for run in &mut detail.runs {
            if run.started_at.is_some() && run.ended_at.is_none() {
                run.started_at = Some(NOW_STR.to_string());
            }
        }
    }
}

/// This suite's canned `$EDITOR` text; asserted on in the editor snapshots.
const EDITED: &str = "edited via $EDITOR";

fn driver<C: BoardClient + 'static>(client: C) -> Driver {
    board_tui::testkit::driver_with_editor(client, EDITED)
}

fn key(d: &mut Driver, code: KeyCode) {
    d.handle(Msg::Key(KeyEvent::new(code, KeyModifiers::empty())));
}

fn render(d: &mut Driver, w: u16, h: u16) -> String {
    pin(&mut d.app);
    board_tui::testkit::draw(&d.app, w, h)
}

#[test]
fn active_run_summary_wins_over_card_updated_at_for_timer() {
    let mut d = driver(demo_client().unwrap());
    let output = render(&mut d, 120, 35);
    assert!(output.contains("running · 2m"), "{output}");
}

#[test]
fn empty_board() {
    let mut d = driver(FakeBoardClient::new().unwrap());
    insta::assert_snapshot!("empty_board", render(&mut d, 80, 24));
}

#[test]
fn empty_board_copy_matches_the_active_filter() {
    let expected = [
        (
            CardFilter::Active,
            "No active cards.",
            "v: show all / archived",
        ),
        (CardFilter::All, "No cards.", "v: show active / archived"),
        (
            CardFilter::Archived,
            "No archived cards.",
            "v: show active / all",
        ),
    ];
    for (filter, message, hint) in expected {
        let mut d = driver(demo_client().unwrap());
        d.app.card_filter = filter;
        // Review has no archived card in the default fixture; Archived is the
        // regression case, while All also proves its empty copy is distinct.
        d.app.sel_col = 3;
        d.app.board.cards.clear();
        let output = render_sized(&mut d, 40, 20);
        assert!(
            output.contains(message),
            "{filter:?} copy missing:\n{output}"
        );
        assert!(output.contains(hint), "{filter:?} hint missing:\n{output}");
        assert!(
            !(filter == CardFilter::Archived && output.contains("No active cards.")),
            "Archived must not use active-card copy:\n{output}"
        );
    }
}

#[test]
fn set_origin_socket_updates_context_used_by_later_new_card_form() {
    let mut d = driver(FakeBoardClient::new().unwrap());
    d.set_origin_socket(Some("/tmp/herdr/sessions/feature/herdr.sock".to_string()));
    key(&mut d, KeyCode::Char('n'));
    let form = d.app.form.as_ref().expect("new-card form");
    assert_eq!(form.current_session().as_deref(), Some("feature"));
}

#[test]
fn default_context_ignores_ambient_herdr_session() {
    let mut d = driver(FakeBoardClient::new().unwrap());
    key(&mut d, KeyCode::Char('n'));
    let form = d.app.form.as_ref().expect("new-card form");
    assert_eq!(form.current_session(), None);
}

#[test]
fn explicit_hostile_origin_context_keeps_default_rendering_byte_identical() {
    let mut default = driver(FakeBoardClient::new().unwrap());
    let mut hostile = driver_with_origin(FakeBoardClient::new().unwrap(), EDITED, hostile_origin());

    let default_output = render(&mut default, 80, 24);
    let hostile_output = render(&mut hostile, 80, 24);
    assert_eq!(hostile_output, default_output);
}

#[test]
fn seeded_board_glyphs_80x24() {
    let mut d = driver(demo_client().unwrap());
    insta::assert_snapshot!("seeded_board_80x24", render(&mut d, 80, 24));
}

/// Header controls stay on the documented rows without redundant Board/Visible
/// prose, and the old persistent footer hint is gone. Narrow rails use
/// unambiguous filter prefixes while retaining all three hit zones.
#[test]
fn responsive_header_and_minimal_footer_in_every_layout() {
    for (w, h) in [(40u16, 20u16), (65, 24), (120, 35)] {
        let mut d = driver(demo_client().unwrap());
        let out = render(&mut d, w, h);
        let header: Vec<&str> = out.lines().take(if w < 60 { 3 } else { 1 }).collect();
        assert!(
            header.iter().any(|line| line.contains("herdr-board")),
            "{w}x{h}: product identity missing from header: {header:?}"
        );
        assert!(
            !header.iter().any(|line| line.contains("Board:")),
            "{w}x{h}: redundant Board label remains: {header:?}"
        );
        assert!(
            !header.iter().any(|line| line.contains("Visible:")),
            "{w}x{h}: redundant Visible label remains: {header:?}"
        );
        assert!(
            out.lines()
                .all(|line| { !line.contains("? help") && !line.contains("drag card to move") }),
            "{w}x{h}: persistent footer hint still visible: {out}"
        );
    }
}

#[test]
fn seeded_board_glyphs_120x35() {
    let mut d = driver(demo_client().unwrap());
    insta::assert_snapshot!("seeded_board_120x35", render(&mut d, 120, 35));
}

#[test]
fn compact_visibility_filters_fit_without_dropping_archived() {
    let mut d = driver(demo_client().unwrap());
    let output = render_sized(&mut d, 37, 24);
    for label in ["[ Act ]", "[ All ]", "[ Arc ]"] {
        assert!(
            output.contains(label),
            "Compact filter {label:?} must remain discoverable at 37 columns:\n{output}"
        );
    }

    let (row, line) = output
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("[ Arc ]"))
        .expect("Archived chip row");
    let column = line.find("[ Arc ]").expect("Archived chip column") as u16;
    assert_eq!(
        d.app.hit_map.borrow().hit(column, row as u16),
        Some(Zone::Filter(CardFilter::Archived)),
        "the wrapped Archived chip must retain its click zone"
    );
}

#[test]
fn board_card_status_glyph_is_once_on_the_next_line_at_each_breakpoint() {
    let fixtures = [
        (40_u16, 20_u16, vec![("#1 Update docs", "idle", '·')]),
        (
            60,
            24,
            vec![
                ("#1 Update docs", "idle", '·'),
                ("#2 Add retry", "running", '▶'),
            ],
        ),
        (
            120,
            35,
            vec![
                ("#1 Update docs", "idle", '·'),
                ("#2 Add retry", "running", '▶'),
                ("#3 Fix flaky test", "queued", '⧗'),
                ("#4 Investigate crash", "blocked", '⏸'),
                ("#5 Refactor auth module", "failed", '✗'),
                ("#6 Tune retry backoff", "awaiting", '?'),
            ],
        ),
    ];

    for (width, height, cards) in fixtures {
        let mut d = driver(demo_client().unwrap());
        let output = render_sized(&mut d, width, height);
        let lines: Vec<&str> = output.lines().collect();
        for (title, status, glyph) in cards {
            let title_row = lines
                .iter()
                .position(|line| line.contains(title))
                .unwrap_or_else(|| panic!("{title:?} missing at {width}x{height}:\n{output}"));
            let title_byte = lines[title_row]
                .find(title)
                .expect("title position must match the title row");
            let title_col = lines[title_row][..title_byte].chars().count();
            let title_chars: Vec<char> = lines[title_row].chars().collect();
            let card_right = title_chars[title_col..]
                .iter()
                .position(|&ch| ch == '│')
                .map(|offset| title_col + offset)
                .unwrap_or(title_chars.len());
            let title_cell: String = title_chars[title_col..card_right].iter().collect();
            let status_row = title_row + 1;
            let status_chars: Vec<char> = lines[status_row].chars().collect();
            let status_cell: String = status_chars[title_col..card_right].iter().collect();
            assert!(
                !title_cell.contains(glyph),
                "{title:?} has status glyph on title row at {width}x{height}:\n{output}"
            );
            assert!(
                status_cell.contains(status),
                "{title:?} status is not on the next row at {width}x{height} (title_col={title_col}, card_right={card_right}, status_cell={status_cell:?}):\n{output}"
            );
            let glyph_count =
                title_cell.matches(glyph).count() + status_cell.matches(glyph).count();
            assert!(
                glyph_count == 1,
                "{title:?} must have exactly one status glyph at {width}x{height} (count={glyph_count}, title={title_cell:?}, status={status_cell:?}):\n{output}"
            );
        }
    }
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

/// Regression for the lost-discoverability bug: at 80x24 the edit-card form's
/// 9 visible fields (one multiline, capped at 5 rows) need 26 rows including
/// borders/button-bar, but `main_area` only has 23 — the fields genuinely
/// cannot all fit (not an under-requested `content_h`; see `draw_form`'s
/// `content_h` comment), so the field window must show a scrollbar rather
/// than silently dropping `space ref` with no visual trace.
#[test]
fn edit_card_modal_shows_scrollbar_when_fields_overflow() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // Plan
    key(&mut d, KeyCode::Char('e'));
    let output = render(&mut d, 80, 24);
    assert!(
        output.contains('█'),
        "an overflowing field window must render a scrollbar thumb:\n{output}"
    );
    insta::assert_snapshot!("form_scrollbar_overflowing_80x24", output);
}

/// Counterpart: a form whose fields fit entirely within `main_area` at this
/// size must render with no scrollbar column reserved at all (the reservation
/// is conditional on `overflowing`, so the non-overflowing path never loses a
/// content column).
#[test]
fn column_form_no_scrollbar_when_fields_fit() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('N'));
    // The grouped (section-card) column form is taller than the old flat list,
    // so use a taller viewport to verify the no-scrollbar guarantee.
    let output = render(&mut d, 100, 34);
    assert!(
        !output.contains('█'),
        "a fully-visible field window must not render a scrollbar thumb:\n{output}"
    );
    insta::assert_snapshot!("form_no_scrollbar_fits_100x34", output);
}

#[test]
fn column_form() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('N'));
    insta::assert_snapshot!("column_form", render(&mut d, 80, 24));
}

#[test]
fn column_form_trigger_auto_shows_system_prompt() {
    // Manual (default) hides the system prompt field; switching the trigger to
    // auto reveals it. See snapshots__column_form.snap for the hidden baseline.
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('N'));
    {
        let form = d.app.form.as_mut().expect("column form");
        if let FieldKind::Choice { opts, idx } = &mut form
            .fields
            .iter_mut()
            .find(|f| f.id == FieldId::Trigger)
            .expect("trigger field")
            .kind
        {
            *idx = opts.iter().position(|o| o.label == "auto").expect("auto");
        }
        form.on_trigger_changed();
    }
    insta::assert_snapshot!("column_form_trigger_auto", render(&mut d, 80, 24));
}

#[test]
fn column_form_hostile_origin_is_metadata_only() {
    let mut baseline = driver(demo_client().unwrap());
    key(&mut baseline, KeyCode::Char('N'));
    let baseline_output = render(&mut baseline, 80, 24);

    let mut hostile = driver_with_origin(demo_client().unwrap(), EDITED, hostile_origin());
    key(&mut hostile, KeyCode::Char('N'));
    let hostile_output = render(&mut hostile, 80, 24);
    assert_eq!(hostile_output, baseline_output);
    insta::assert_snapshot!("column_form_hostile", hostile_output);
}

#[test]
fn card_detail_wraps_long_description_and_comment_popup_and_fullscreen() {
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
            title: "Wrap demo".into(),
            description: Some(
                "This description is intentionally long so it must word-wrap across \
                 several rendered rows at the panel borders instead of being cut off, \
                 both inside the centered popup and inside the fullscreen detail view."
                    .into(),
            ),
            column_id: Some(todo),
            harness: Some("claude".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    client
        .comment_add(
            id,
            "Likewise this comment body is long enough to wrap across multiple rows \
             instead of being truncated to a single ellipsized line, demonstrating \
             per-comment word wrap at the section border.",
            Some("reviewer"),
        )
        .unwrap();

    let mut d = driver(client);
    // The newly created card is the second card in Todo (after "Update docs").
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("card_detail_wrap_popup_80x24", render(&mut d, 80, 24));
    insta::assert_snapshot!("card_detail_wrap_popup_120x35", render(&mut d, 120, 35));
    key(&mut d, KeyCode::Char('f'));
    insta::assert_snapshot!(
        "card_detail_wrap_fullscreen_120x35",
        render(&mut d, 120, 35)
    );
}

#[test]
fn card_detail_metadata_wraps_wide_and_ellipsizes_when_compact() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let todo = board
        .columns
        .iter()
        .find(|column| column.name == "Todo")
        .unwrap()
        .id;
    client
        .card_create(&CardCreateParams {
            title: "Long metadata values".into(),
            column_id: Some(todo),
            harness: Some("pi".into()),
            model: Some("model-with-a-very-long-name".into()),
            effort: Some(Effort::Max),
            permission_mode: Some("permission-mode-with-a-very-long-name".into()),
            session: Some("session-with-a-very-long-name".into()),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("workspace-with-a-very-long-reference".into()),
            ..Default::default()
        })
        .unwrap();

    let mut d = driver(client);
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);

    let wide = render_sized(&mut d, 120, 35);
    for prefix in ["Harness · Model:", "Herdr session:"] {
        let line = wide
            .lines()
            .find(|line| line.contains(prefix))
            .unwrap_or_else(|| panic!("{prefix:?} missing from wide detail:\n{wide}"));
        assert!(
            !line.contains('…'),
            "wide metadata should wrap before it ellipsizes:\n{wide}"
        );
    }
    assert!(wide.contains("permission-mode-with-a-very-long-name"));
    assert!(
        wide.contains("eference"),
        "the wrapped session value is incomplete:\n{wide}"
    );
    insta::assert_snapshot!("card_detail_metadata_wrap_120x35", wide);

    let compact = render_sized(&mut d, 40, 20);
    for prefix in ["Harness · Model:", "Herdr session:"] {
        let line = compact
            .lines()
            .find(|line| line.contains(prefix))
            .unwrap_or_else(|| panic!("{prefix:?} missing from compact detail:\n{compact}"));
        assert!(
            line.contains('…'),
            "compact metadata must use an explicit ellipsis when it cannot wrap:\n{compact}"
        );
    }
    insta::assert_snapshot!("card_detail_metadata_ellipsis_40x20", compact);
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
fn compact_detail_actions_keep_complete_named_chips() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Enter); // Todo's idle card has no run history.
    let output = render_sized(&mut d, 40, 20);
    for label in [
        "[ Edit ]",
        "[ Archive ]",
        "[ Add ]",
        "[ Open ]",
        "[ Retry ]",
        "[ Cancel ]",
    ] {
        assert!(
            output.contains(label),
            "compact detail action {label:?} is not discoverable:\n{output}"
        );
    }
    assert!(
        !output.contains("[ E… ]")
            && !output.contains("[ A… ]")
            && !output.contains("[ O… ]")
            && !output.contains("[ R… ]")
            && !output.contains("[ C… ]"),
        "compact detail actions must never ellipsize:\n{output}"
    );

    // Awaiting adds one card action but must still keep every action named.
    d.app.detail.as_mut().unwrap().card.status = CardStatus::Awaiting;
    let awaiting = render_sized(&mut d, 40, 20);
    assert!(
        awaiting.contains("[ Confirm ]"),
        "awaiting action missing:\n{awaiting}"
    );
    for label in [
        "[ Edit ]",
        "[ Archive ]",
        "[ Add ]",
        "[ Open ]",
        "[ Retry ]",
        "[ Cancel ]",
    ] {
        assert!(
            awaiting.contains(label),
            "awaiting detail action {label:?} is not discoverable:\n{awaiting}"
        );
    }
}

#[test]
fn empty_runs_body_never_shares_a_row_with_run_actions() {
    let sizes = [(40_u16, 20_u16), (52, 24), (60, 24), (80, 24), (120, 35)];
    for (w, h) in sizes {
        let mut d = driver(demo_client().unwrap());
        key(&mut d, KeyCode::Enter); // Todo's idle card has no runs.
        let output = render_sized(&mut d, w, h);
        assert!(
            !output
                .lines()
                .any(|line| line.contains("(no run[") || line.contains("(no[")),
            "empty Runs content overlaps its action row at {w}x{h}:\n{output}"
        );
    }
}

#[test]
fn detail_action_label_snapshots_cover_mobile_regular_and_wide_sizes() {
    for (w, h) in [(40_u16, 20_u16), (52, 24), (60, 24), (80, 24), (120, 35)] {
        let mut d = driver(demo_client().unwrap());
        key(&mut d, KeyCode::Enter);
        insta::assert_snapshot!(
            format!("detail_actions_{w}x{h}"),
            render_sized(&mut d, w, h)
        );
    }
}

#[test]
fn overlays_preserve_board_chrome_and_use_icon_run_controls() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    let output = render(&mut d, 120, 35);
    let top: Vec<&str> = output.lines().take(3).collect();
    assert!(
        top.iter().any(|line| line.contains("herdr-board")),
        "{output}"
    );
    assert!(
        top.iter().any(|line| line.contains("[ Global ▾ ]")),
        "{output}"
    );
    assert!(!top.iter().any(|line| line.contains("Board:")), "{output}");
    assert!(
        !top.iter().any(|line| line.contains("Visible:")),
        "{output}"
    );
    let legacy_close = format!("[ {} ]", "Close");
    assert!(
        !output.contains(&legacy_close),
        "legacy close label remains:\n{output}"
    );
    assert!(
        output.contains("[ X ]"),
        "detail close icon missing:\n{output}"
    );
    assert!(
        output.contains("[ □ ]"),
        "detail toggle icon missing:\n{output}"
    );
    assert!(
        output.contains("[ Open ]"),
        "run open label missing:\n{output}"
    );
    assert!(
        output.contains("[ Retry ]"),
        "run retry label missing:\n{output}"
    );
    assert!(
        output.contains("[ Cancel ]"),
        "run cancel label missing:\n{output}"
    );
    assert!(
        output.contains("[ + Card ]"),
        "board action chrome missing:\n{output}"
    );
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
    // The cycle of enqueue→promote→finalize advances the card status;
    // restore the original Failed status so the snapshot matches.
    client
        .db()
        .set_card_status(card.id, CardStatus::Failed)
        .unwrap();
    // The fixture's own runs above use wall-clock `datetime('now')`; pin
    // elapsed to 0 so a promote→finalize pair straddling a second boundary
    // (slow/loaded CI) cannot flip a deterministic `0s` row to `1s`.
    client.db().pin_finalized_run_elapsed().unwrap();

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
fn comment_history_sheet_40x20_and_80x24() {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone();
    let comment = client
        .comment_add(card.id, "first draft of the note", Some("reviewer"))
        .unwrap();
    client
        .comment_update(comment.id, "revised wording of the note", None)
        .unwrap();
    client
        .comment_update(comment.id, "final text of the note", None)
        .unwrap();

    let mut d = driver(client);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    // Opening detail focuses the newest comment (the one just edited twice).
    key(&mut d, KeyCode::Char('h'));
    assert_eq!(d.app.screen, Screen::CommentHistory);
    // The audit trail's timestamps are real wall-clock time (unlike
    // `board`/`detail`, `pin()` has nothing to rewrite them from); stabilize
    // them here so the snapshot is deterministic.
    if let Some(state) = d.app.comment_history.as_mut() {
        for (i, entry) in state.entries.iter_mut().enumerate() {
            entry.created_at = format!("2026-07-14 12:0{}:00", i);
        }
    }
    insta::assert_snapshot!("comment_history_40x20", render_sized(&mut d, 40, 20));
    insta::assert_snapshot!("comment_history_80x24", render_sized(&mut d, 80, 24));
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
fn compact_switcher_long_scoped_row_ellipsizes_before_border_and_desktop_chevron_survives() {
    let long_scope = "/private/tmp/hb-visual.HWoEPU/scope";
    let mut client = demo_client().unwrap();
    client.board_open(long_scope).unwrap();

    let mut compact = driver(client);
    compact.app.last_area = Rect::new(0, 0, 40, 20);
    key(&mut compact, KeyCode::Char('b'));
    let selected = compact
        .app
        .switcher
        .as_ref()
        .and_then(|state| {
            state
                .boards
                .iter()
                .position(|(label, _)| label.contains(long_scope))
        })
        .expect("long scoped board in compact switcher");
    compact.app.switcher.as_mut().unwrap().sel = selected;
    let output = render_sized(&mut compact, 40, 20);
    let row = output
        .lines()
        .find(|line| line.contains("scope —"))
        .expect("long scoped board row");
    assert!(
        row.contains('…'),
        "long scoped board row must visibly ellipsize before the border:\n{output}"
    );
    assert!(
        row.contains("… │"),
        "long scoped board row must place its ellipsis before the sheet border:\n{output}"
    );
    insta::assert_snapshot!("compact_switcher_long_scoped_row", output);

    let mut desktop = driver(demo_client().unwrap());
    desktop.app.board.board.name = long_scope.into();
    let desktop_output = render_sized(&mut desktop, 60, 24);
    let header = desktop_output.lines().next().expect("desktop header");
    assert!(
        header.contains("… ▾"),
        "truncated desktop board chip must retain its dropdown chevron: {header:?}"
    );
}

#[test]
fn help_overlay() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('?'));
    let output = render(&mut d, 80, 24);
    assert!(!output.contains("archiv forms"));
    assert!(!output.contains('…'));
    assert!(!output.contains("boar  Esc"));
    assert!(!output.contains("column│"));
    assert!(output
        .lines()
        .all(|line| !line.contains("j/k scroll · Esc close")));
    insta::assert_snapshot!("help_overlay", output);
}

#[test]
fn delete_column_with_cards_picker() {
    let mut d = driver(demo_client().unwrap());
    // Todo: has cards, but none of them has an open run — so `D` asks where
    // the cards should go. (A column WITH an open run is refused outright; see
    // `delete_column_with_an_open_run_is_refused_before_the_picker`.)
    key(&mut d, KeyCode::Char('D'));
    insta::assert_snapshot!("delete_column_picker", render(&mut d, 80, 24));
}

/// A10: the daemon refuses `column.delete` while any card in the column has an
/// open run, so the TUI must not first collect a "move cards where?" answer it
/// is going to throw away.
#[test]
fn delete_column_with_an_open_run_is_refused_before_the_picker() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // Plan (has the running card)
    key(&mut d, KeyCode::Char('D'));
    assert_eq!(d.app.screen, Screen::Board);
    assert!(d.app.picker.is_none(), "no destination picker is opened");
    assert!(d.app.confirm.is_none());
    let toast = d.app.toast.as_ref().expect("refusal is explained");
    assert!(toast.is_error);
    assert_eq!(toast.text, "column has a card with an open run");
}

#[test]
fn move_card_flow() {
    let mut d = driver(demo_client().unwrap());
    // "before": Todo's card is selected.
    insta::assert_snapshot!("move_before", render(&mut d, 80, 24));
    // `m` opens the active board's column picker directly (same-board fast path).
    key(&mut d, KeyCode::Char('m'));
    insta::assert_snapshot!("move_pick_column", render(&mut d, 80, 24));
    // Move the card to Plan (Down once from Todo).
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!("move_after", render(&mut d, 80, 24));
}

#[test]
fn move_card_cross_board_via_b_toggle() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('m')); // active board (Global) column picker
    key(&mut d, KeyCode::Char('b')); // -> destination-board picker
    insta::assert_snapshot!("move_pick_board", render(&mut d, 80, 24));
}

/// A client wrapper that simulates the daemon's blocking sanity check
/// rejecting a cross-board move (e.g. an unresolvable session), so the TUI
/// surfaces the error as a toast.
struct FailingMoveClient(DemoClient);
impl BoardClient for FailingMoveClient {
    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if method == "card.move" {
            anyhow::bail!(
                "boardd error 3: invalid state: cannot move: session does not resolve: \
                 herdr session 'ghost' not found"
            );
        }
        self.0.call(method, params)
    }
    fn subscribe(
        &mut self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = board_core::protocol::Event> + Send>> {
        self.0.subscribe()
    }
}

#[test]
fn move_blocked_shows_error_toast() {
    let mut d = driver(FailingMoveClient(demo_client().unwrap()));
    key(&mut d, KeyCode::Char('m')); // active board column picker
    key(&mut d, KeyCode::Down); // Plan
    key(&mut d, KeyCode::Enter); // commit -> daemon rejects -> toast
    assert!(d.app.toast.as_ref().is_some_and(|t| t.is_error));
    assert_eq!(d.app.screen, Screen::Board);
    insta::assert_snapshot!("move_blocked_toast", render(&mut d, 80, 24));
}

#[test]
fn move_column_mini_mode_then_enter() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // focus Plan
    key(&mut d, KeyCode::Char('M')); // enter move-column mini-mode
    key(&mut d, KeyCode::Right); // Plan -> index 2
    let in_mode = render(&mut d, 120, 35);
    assert_eq!(d.app.screen, Screen::MoveColumn);
    assert!(in_mode.contains("Move column"), "banner visible: {in_mode}");
    insta::assert_snapshot!("move_column_mini_mode", in_mode);

    key(&mut d, KeyCode::Enter); // commit one column.reorder
    assert_eq!(d.app.screen, Screen::Board);
    // Plan committed at index 2 after the refetch.
    assert_eq!(
        d.app.board.columns.iter().position(|c| c.name == "Plan"),
        Some(2)
    );
    insta::assert_snapshot!("move_column_committed", render(&mut d, 120, 35));
}

#[test]
fn move_column_mini_mode_esc_cancels() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right); // focus Plan
    let original: Vec<String> = d.app.board.columns.iter().map(|c| c.name.clone()).collect();
    key(&mut d, KeyCode::Char('M'));
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Esc);
    assert_eq!(d.app.screen, Screen::Board);
    let after: Vec<String> = d.app.board.columns.iter().map(|c| c.name.clone()).collect();
    assert_eq!(
        after, original,
        "Esc must restore the original column order"
    );
}

#[test]
fn toast_does_not_overwrite_chrome_when_no_toast_row_fits() {
    let mut d = driver(demo_client().unwrap());
    d.app.toast = Some(Toast {
        text: "must stay hidden".into(),
        is_error: true,
        at: now(),
    });

    let output = render(&mut d, 40, 5);
    assert!(
        output.contains("herdr-board"),
        "header was overwritten:\n{output}"
    );
    assert!(
        !output.contains("must stay hidden"),
        "toast must not overwrite header/action chrome:\n{output}"
    );
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
    // Retry confirms first (it relaunches a real agent); `y` fires the RPC.
    assert_eq!(d.app.screen, Screen::Confirm);
    key(&mut d, KeyCode::Char('y'));
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
    let output = render(&mut d, 80, 24);
    assert!(output.contains("? awaiting (agent reported done)"));
    assert!(output.contains("Harness · Model: claude · default"));
    assert!(output.contains("Herdr session: default"));
    assert!(output.contains("Space: workspace:-"));
    insta::assert_snapshot!("awaiting_card_detail", output);
}

#[test]
fn awaiting_card_detail_stays_compact_when_wide() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    let output = render(&mut d, 120, 35);
    assert!(output.contains("? awaiting (agent reported done)"));
    assert!(output.contains("Harness · Model: claude · default"));
    insta::assert_snapshot!("awaiting_card_detail_120x35", output);
}

#[test]
fn enter_on_awaiting_detail_runs_done_and_refreshes_driver_state() {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Enter);
    assert_eq!(
        d.app.detail.as_ref().unwrap().card.status,
        CardStatus::Awaiting
    );

    key(&mut d, KeyCode::Enter);

    let detail = d.app.detail.as_ref().unwrap();
    assert_eq!(detail.card.status, CardStatus::Done);
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.runs[0].outcome, Some(RunOutcome::Ok));
    assert_eq!(
        d.app
            .board
            .cards
            .iter()
            .find(|card| card.id == detail.card.id)
            .unwrap()
            .status,
        CardStatus::Done
    );
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
    let output = render(&mut d, 80, 24);
    assert!(output.contains("? awaiting (idle timeout)"));
    assert!(output.contains("Harness · Model: claude · default"));
    assert!(output.contains("Herdr session: default"));
    assert!(output.contains("Space: workspace:-"));
    insta::assert_snapshot!("awaiting_idle_detail", output);
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

// -- mobile-responsive size matrix -------------------------------------------
//
// The sizes below join the existing 80x24 (Regular) / 120x35 (Wide) coverage
// with three narrower points: 40x20 and 52x24 are Compact
// (`LayoutMode::from_width` < 60), 60x24 is the narrowest Regular width. Every
// screen here uses one of the new Compact-mode widgets (fullscreen sheet,
// compact header, windowed fields, switcher), so — unlike `render()` alone —
// `app.last_area` must be set to the same size being drawn: view-layer mode
// decisions (`App::layout_mode`, used by `sheet_area`/`draw_switcher`/
// `draw_form` etc.) read `app.last_area`, not the `TestBackend` frame size.

/// Generate one `#[test]` per matrix size instead of looping inside a single
/// test: `insta`'s pending-review snapshots still panic the first time they
/// see a name with no `.snap` baseline (even under `INSTA_UPDATE=new`), and a
/// panic mid-loop would silently skip every later size in that same test.
macro_rules! size_matrix_test {
    ($mod_name:ident, |$w:ident, $h:ident| $body:block) => {
        mod $mod_name {
            use super::*;
            #[test]
            fn compact_40x20() {
                let $w: u16 = 40;
                let $h: u16 = 20;
                $body
            }
            #[test]
            fn compact_52x24() {
                let $w: u16 = 52;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn regular_60x24() {
                let $w: u16 = 60;
                let $h: u16 = 24;
                $body
            }
        }
    };
}

/// Form-specific matrix includes the desktop sizes where large forms may still
/// overflow and the Wide sheet must stop at its preferred width.
macro_rules! form_size_matrix_test {
    ($mod_name:ident, |$w:ident, $h:ident| $body:block) => {
        mod $mod_name {
            use super::*;
            #[test]
            fn compact_40x20() {
                let $w: u16 = 40;
                let $h: u16 = 20;
                $body
            }
            #[test]
            fn compact_52x24() {
                let $w: u16 = 52;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn regular_60x24() {
                let $w: u16 = 60;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn regular_80x24() {
                let $w: u16 = 80;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn wide_120x35() {
                let $w: u16 = 120;
                let $h: u16 = 35;
                $body
            }
        }
    };
}

/// Overlay-only matrix includes the Regular and Wide design targets without
/// multiplying unrelated board/detail fixtures.
macro_rules! sheet_size_matrix_test {
    ($mod_name:ident, |$w:ident, $h:ident| $body:block) => {
        mod $mod_name {
            use super::*;
            #[test]
            fn compact_40x20() {
                let $w: u16 = 40;
                let $h: u16 = 20;
                $body
            }
            #[test]
            fn compact_52x24() {
                let $w: u16 = 52;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn regular_60x24() {
                let $w: u16 = 60;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn regular_80x24() {
                let $w: u16 = 80;
                let $h: u16 = 24;
                $body
            }
            #[test]
            fn wide_120x35() {
                let $w: u16 = 120;
                let $h: u16 = 35;
                $body
            }
        }
    };
}

/// A >=300-char multi-line description: the Bug B regression target. Before
/// the fix this rendered as `lines().join("  ⏎  ")` truncated to one
/// ellipsized line; now it must render as real wrapped paragraph text.
const LONG_MULTILINE_DESC: &str = "\
This is the first paragraph of an intentionally long card description that \
must word-wrap across many rendered rows instead of being cut off, which is \
exactly the bug this change fixes for multiline text fields inside the edit \
form.\n\
This second paragraph, introduced by an explicit newline, is also long \
enough to wrap multiple times at every terminal width this suite exercises, \
from the narrowest Compact size up to the Wide desktop layout, and it keeps \
going a bit further to comfortably clear the 300-character floor.\n\
A short third line closes it out.";

fn render_sized(d: &mut Driver, w: u16, h: u16) -> String {
    d.app.last_area = Rect::new(0, 0, w, h);
    render(d, w, h)
}

#[test]
fn long_multiline_desc_is_at_least_300_chars_with_newlines() {
    assert!(LONG_MULTILINE_DESC.len() >= 300, "fixture too short");
    assert!(
        LONG_MULTILINE_DESC.contains('\n'),
        "fixture must be multi-line"
    );
}

size_matrix_test!(size_matrix_board, |w, h| {
    let mut d = driver(demo_client().unwrap());
    insta::assert_snapshot!(format!("board_{w}x{h}"), render_sized(&mut d, w, h));
});

size_matrix_test!(size_matrix_card_detail_popup_and_fullscreen, |w, h| {
    let mut d = driver(demo_client().unwrap());
    // Review (idx 3): the failed card, first in the column.
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Right);
    key(&mut d, KeyCode::Enter);
    insta::assert_snapshot!(
        format!("card_detail_popup_{w}x{h}"),
        render_sized(&mut d, w, h)
    );
    key(&mut d, KeyCode::Char('f'));
    insta::assert_snapshot!(
        format!("card_detail_fullscreen_{w}x{h}"),
        render_sized(&mut d, w, h)
    );
});

form_size_matrix_test!(size_matrix_edit_form_long_multiline_description, |w, h| {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let todo = board
        .columns
        .iter()
        .find(|column| column.name == "Todo")
        .unwrap()
        .id;
    client
        .card_create(&CardCreateParams {
            title: "Long description demo".into(),
            description: Some(LONG_MULTILINE_DESC.into()),
            column_id: Some(todo),
            harness: Some("claude".into()),
            ..Default::default()
        })
        .unwrap();

    let mut d = driver(client);
    // The newly created card is the second card in Todo (after "Update docs").
    key(&mut d, KeyCode::Down);
    key(&mut d, KeyCode::Char('e'));
    assert_eq!(d.app.screen, Screen::CardForm);
    {
        let form = d.app.form.as_mut().expect("edit-card form");
        let idx = form
            .fields
            .iter()
            .position(|f| f.id == FieldId::Description)
            .expect("Description field present");
        form.focus = idx; // keep it inside the windowed field view
    }

    let output = render_sized(&mut d, w, h);
    // Essential form chrome remains complete at every responsive width;
    // only hostile dynamic values may ellipsize. Choice fields may be in a
    // different complete section-card window at short heights, so assert
    // their chip shape whenever a choice is visible rather than requiring a
    // hidden section to be rendered.
    for label in [
        "description (base prompt)",
        "[ $EDITOR ]",
        "[ Save ]",
        "[ Cancel ]",
    ] {
        assert!(
            output.contains(label),
            "form control {label:?} must be complete at {w}x{h}:\n{output}"
        );
    }
    assert!(
        !output.contains("Ctrl+E:"),
        "the clipping-prone inline editor hint must be replaced at {w}x{h}:\n{output}"
    );
    if output.contains('‹') || output.contains('›') {
        assert!(
            output.contains("[ ‹ ]"),
            "left choice chip is malformed at {w}x{h}:\n{output}"
        );
        assert!(
            output.contains("[ › ]"),
            "right choice chip is malformed at {w}x{h}:\n{output}"
        );
    }
    assert!(
        !output
            .lines()
            .any(|line| line.contains("base prompt") && line.contains('…')),
        "the focused field label must not ellipsize at {w}x{h}:\n{output}"
    );
    assert!(
        output.contains("first paragraph"),
        "wrapped description text must be visible at {w}x{h}:\n{output}"
    );
    insta::assert_snapshot!(format!("edit_form_long_desc_{w}x{h}"), output);
});

sheet_size_matrix_test!(size_matrix_help, |w, h| {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('?'));
    insta::assert_snapshot!(format!("help_{w}x{h}"), render_sized(&mut d, w, h));
});

sheet_size_matrix_test!(size_matrix_picker, |w, h| {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('m'));
    insta::assert_snapshot!(format!("picker_{w}x{h}"), render_sized(&mut d, w, h));
});

sheet_size_matrix_test!(size_matrix_confirm, |w, h| {
    let mut d = driver(demo_client().unwrap());
    key(&mut d, KeyCode::Char('d'));
    insta::assert_snapshot!(format!("confirm_{w}x{h}"), render_sized(&mut d, w, h));
});

sheet_size_matrix_test!(size_matrix_switcher_columns_and_boards, |w, h| {
    let mut d = driver(demo_client().unwrap());
    // Force Compact just long enough to open the switcher sheet at the
    // Columns level (Regular/Wide keep the classic `b` -> board `Picker`).
    // This mirrors the header's center-button tap (`Zone::HeaderSwitch`),
    // NOT `b` — `b` now opens directly at the Boards level (it means
    // "switch board"), so it can no longer reach Columns. The actual matrix
    // size is applied by `render_sized` right before each draw.
    d.app.last_area = Rect::new(0, 0, 40, 20);
    d.app.switcher = Some(SwitcherState {
        level: SwitcherLevel::Columns,
        sel: d.app.sel_col,
        columns_sel: d.app.sel_col,
        boards: Vec::new(),
        entered_at_boards: false,
        return_to: Screen::Board,
    });
    d.app.screen = Screen::Switcher;
    assert_eq!(d.app.screen, Screen::Switcher);
    insta::assert_snapshot!(
        format!("switcher_columns_{w}x{h}"),
        render_sized(&mut d, w, h)
    );

    let n = d.app.board.columns.len();
    d.app.switcher.as_mut().unwrap().sel = n; // trailing "switch board" row
    key(&mut d, KeyCode::Enter);
    assert_eq!(
        d.app.switcher.as_ref().unwrap().level,
        SwitcherLevel::Boards
    );
    insta::assert_snapshot!(
        format!("switcher_boards_{w}x{h}"),
        render_sized(&mut d, w, h)
    );
});
