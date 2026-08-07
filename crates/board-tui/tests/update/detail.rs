//! Card detail tests: popup, fullscreen, scrolling, history, awaiting/done.

use super::helpers::{demo_app, demo_app_with_detail, driver_of, key};
/// One mouse-wheel notch over `(x, y)` — the shared `Msg::Mouse` injector.
use super::helpers::{mouse as wheel, rendered_rows};
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

/// Give `card_id` `n` extra finished runs, each recording pane `p-<i>` unless
/// `panes` says otherwise. Returns the new run ids, oldest → newest.
fn seed_runs(
    client: &board_tui::testkit::DemoClient,
    card: &board_core::model::Card,
    n: usize,
    panes: bool,
) -> Vec<i64> {
    (0..n)
        .map(|i| {
            let run = client
                .db()
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness: "claude",
                    argv_json: "[]",
                    prompt_snapshot: "p",
                    system_prompt_snapshot: None,
                    // A durable launch spec is part of what makes a run
                    // reopenable, so seed one: the fake mirrors the daemon's
                    // preconditions.
                    launch_spec_json: Some(
                        &serde_json::to_string(&board_core::launch::RunLaunchSpec::v1(
                            board_core::launch::ExecutionSpec {
                                argv: vec!["claude".into(), "--model".into(), "m".into()],
                                env: vec![],
                                agent_kind: Some("claude".into()),
                                initial_prompt: Some("p".into()),
                                system_prompt: Some("s".into()),
                            },
                        ))
                        .unwrap(),
                    ),
                    session_id: Some(&format!("conv-{i}-0123456789abcdef")),
                    session: None,
                })
                .unwrap();
            let pane = format!("w1:p-{i}");
            client
                .db()
                .promote_run_uow(run.id, Some("w1"), panes.then_some(pane.as_str()), None)
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
            run.id
        })
        .collect()
}

/// A driver with the given card's detail open (opened through the real
/// `Enter` → `LoadDetail` path, so the selection sentinels apply).
fn driver_with_detail_open(
    client: board_tui::testkit::DemoClient,
    card_id: i64,
) -> board_tui::Driver {
    let mut d = driver_of(client);
    let col = d
        .app
        .board
        .cards
        .iter()
        .find(|c| c.id == card_id)
        .unwrap()
        .column_id;
    d.app.sel_col = d
        .app
        .board
        .columns
        .iter()
        .position(|c| c.id == col)
        .unwrap();
    d.app.sel_card = d
        .app
        .cards_of(col)
        .iter()
        .position(|c| c.id == card_id)
        .unwrap();
    d.handle(key(KeyCode::Enter));
    assert_eq!(d.app.screen, Screen::CardDetail);
    // The section-card detail needs more vertical room for its in-card action
    // bars; give the behavior tests a taller frame than the 80x24 default and
    // re-anchor the histories to the new geometry.
    d.app.last_area = Rect::new(0, 0, 110, 44);
    d.app.scroll_detail_to_latest();
    d
}

fn failed_card(client: &mut board_tui::testkit::DemoClient) -> board_core::model::Card {
    client
        .board_get()
        .unwrap()
        .cards
        .iter()
        .find(|card| card.status == CardStatus::Failed)
        .unwrap()
        .clone()
}

#[test]
fn detail_run_selection_defaults_to_newest_and_survives_a_refresh() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    let seeded = seed_runs(&client, &card, 3, true);
    let mut d = driver_with_detail_open(client, card.id);

    // Default selection is the newest run — today's `o` semantics.
    let runs = d.app.detail.as_ref().unwrap().runs.len();
    assert_eq!(d.app.detail_run_sel, runs - 1);
    assert_eq!(d.app.focused_run().unwrap().id, *seeded.last().unwrap());

    // Move the cursor onto an older run, then refresh the open detail: an
    // in-range cursor is preserved, exactly like `detail_comment_sel`.
    d.app.detail_scroll_target = DetailScrollTarget::Runs;
    d.handle(key(KeyCode::Up));
    d.handle(key(KeyCode::Up));
    let picked = d.app.focused_run().unwrap().id;
    assert_ne!(picked, *seeded.last().unwrap());

    // Deleting a comment reloads the open detail (`reload_open_detail`), the
    // same refresh path `detail_comment_sel` survives.
    d.app.detail_scroll_target = DetailScrollTarget::Comments;
    let comments = d.app.detail.as_ref().unwrap().comments.len();
    d.handle(key(KeyCode::Up)); // off the immutable system comment
    d.handle(key(KeyCode::Char('d')));
    d.handle(key(KeyCode::Char('y')));
    assert_eq!(d.app.screen, Screen::CardDetail);
    assert_eq!(
        d.app.detail.as_ref().unwrap().comments.len(),
        comments - 1,
        "the delete must actually have reloaded the detail"
    );
    assert_eq!(
        d.app.focused_run().unwrap().id,
        picked,
        "a detail refresh must not yank the run cursor"
    );
}

#[test]
fn detail_run_selection_clamps_when_the_run_list_shrinks() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    seed_runs(&client, &card, 4, true);
    let mut d = driver_with_detail_open(client, card.id);
    d.app.detail_scroll_target = DetailScrollTarget::Runs;
    let last = d.app.detail.as_ref().unwrap().runs.len() - 1;
    assert_eq!(d.app.detail_run_sel, last);

    // A detail whose run list came back shorter must not leave the cursor out
    // of bounds (nor panic on the next render/`o`).
    let mut detail = d.app.detail.clone().unwrap();
    detail.runs.truncate(2);
    d.app.detail = Some(detail);
    d.app.detail_run_sel = last;
    let effects = update(&mut d.app, key(KeyCode::Char('o')));
    let kept = d.app.detail.as_ref().unwrap().runs[1].id;
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(c, r)] if *c == card.id && *r == kept));
    update(&mut d.app, key(KeyCode::Tab));
    update(&mut d.app, key(KeyCode::Tab));
    assert_eq!(d.app.detail_run_sel, 1);
}

#[test]
fn detail_runs_selection_moves_with_arrows_and_jk_and_saturates() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    seed_runs(&client, &card, 12, true);
    let mut d = driver_with_detail_open(client, card.id);
    d.app.detail_scroll_target = DetailScrollTarget::Runs;
    let len = d.app.detail.as_ref().unwrap().runs.len();
    let visible = {
        let layout = board_tui::view::detail_layout(&d.app, d.app.last_area);
        board_tui::view::runs_viewport_height(&layout).max(1)
    };
    assert!(len > visible, "fixture must overflow the runs viewport");

    // `k` and `Up` both move the selection up one row; the offset follows.
    for expected in (0..len - 1).rev() {
        let msg = if expected % 2 == 0 {
            key(KeyCode::Char('k'))
        } else {
            key(KeyCode::Up)
        };
        update(&mut d.app, msg);
        assert_eq!(d.app.detail_run_sel, expected);
        assert!(
            d.app.detail_run_sel >= d.app.detail_runs_scroll
                && d.app.detail_run_sel < d.app.detail_runs_scroll + visible,
            "selected row {} outside viewport [{}, {})",
            d.app.detail_run_sel,
            d.app.detail_runs_scroll,
            d.app.detail_runs_scroll + visible
        );
    }
    // Saturates at the top: no wrap-around to the newest run.
    update(&mut d.app, key(KeyCode::Up));
    update(&mut d.app, key(KeyCode::Char('k')));
    assert_eq!(d.app.detail_run_sel, 0);
    assert_eq!(d.app.detail_runs_scroll, 0);

    // `j`/`Down` back to the bottom, then saturate there.
    for expected in 1..len {
        let msg = if expected % 2 == 0 {
            key(KeyCode::Char('j'))
        } else {
            key(KeyCode::Down)
        };
        update(&mut d.app, msg);
        assert_eq!(d.app.detail_run_sel, expected);
        assert!(
            d.app.detail_run_sel >= d.app.detail_runs_scroll
                && d.app.detail_run_sel < d.app.detail_runs_scroll + visible
        );
    }
    update(&mut d.app, key(KeyCode::Down));
    update(&mut d.app, key(KeyCode::Char('j')));
    assert_eq!(d.app.detail_run_sel, len - 1);
    assert_eq!(d.app.detail_runs_scroll, len - visible);
}

/// The run row is deliberately minimal: **run number, harness, status, and how
/// long it ran**, and nothing else. The column, the harness conversation id
/// (`conv`) and the `pane ✓|-` marker are not in the row — the identity fields
/// are already elsewhere in the detail, and since a run whose pane is gone is
/// reopened automatically, `pane -` no longer predicts whether `o` works.
#[test]
fn run_rows_show_only_id_harness_status_and_duration() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    let seeded = seed_runs(&client, &card, 2, true);
    let column = client
        .board_get()
        .unwrap()
        .columns
        .iter()
        .find(|c| c.id == card.column_id)
        .unwrap()
        .name
        .clone();
    let d = driver_with_detail_open(client, card.id);
    let newest = *seeded.last().unwrap();
    let rows = rendered_rows(&d.app);
    let row = rows
        .iter()
        .find(|r| r.contains(&format!("#{newest} ")))
        .unwrap_or_else(|| panic!("no rendered row for run #{newest}: {rows:#?}"));

    let prefix = format!("#{newest} claude · ok · ");
    assert!(
        row.contains(&prefix),
        "row must read `#<id> <harness> · <status> · <duration>`: {row}"
    );
    // The trailing field is the duration and the row ends there: exactly two
    // ` · ` separators, so no dropped field can creep back in.
    let duration = row
        .split(&prefix)
        .nth(1)
        .unwrap()
        .trim_end_matches(['"', '│', ' ']);
    assert!(
        duration.ends_with('s') && duration.starts_with(|c: char| c.is_ascii_digit()),
        "last field must be the duration, got {duration:?}: {row}"
    );
    assert_eq!(
        row.matches(" · ").count(),
        2,
        "the row carries exactly id+harness, status and duration: {row}"
    );
    // The explicitly dropped fields.
    assert!(!row.contains("conv"), "conversation id dropped: {row}");
    assert!(!row.contains("pane"), "pane marker dropped: {row}");
    assert!(
        !row.contains(&column),
        "column name dropped (column is {column:?}): {row}"
    );
}

/// A run that has not ended yet reads `active` and reports how long it has been
/// running, measured from the injected `app.now` — not `-` and not a frozen 0s.
#[test]
fn an_active_run_row_reports_how_long_it_has_been_running() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
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
            session_id: Some("conv-active-0123456789"),
            session: None,
        })
        .unwrap();
    // Promoted but never finalized: started, still open.
    client
        .db()
        .promote_run_uow(run.id, Some("w1"), Some("w1:p-active"), None)
        .unwrap();
    let mut d = driver_with_detail_open(client, card.id);
    let started = d
        .app
        .detail
        .as_ref()
        .unwrap()
        .runs
        .iter()
        .find(|r| r.id == run.id)
        .and_then(|r| r.started_at.as_deref())
        .and_then(board_core::protocol::parse_timestamp)
        .expect("a promoted run records started_at");
    d.app.now = started + 95;

    let rows = rendered_rows(&d.app);
    let want = format!("#{} claude · active · 1m35s", run.id);
    assert!(
        rows.iter().any(|r| r.contains(&want)),
        "expected {want:?} in: {rows:#?}"
    );
}

#[test]
fn wheel_scrolling_the_runs_section_carries_the_selection_into_the_window() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    seed_runs(&client, &card, 20, true);
    let mut d = driver_with_detail_open(client, card.id);
    d.app.detail_scroll_target = DetailScrollTarget::Runs;
    let ids: Vec<i64> = d
        .app
        .detail
        .as_ref()
        .unwrap()
        .runs
        .iter()
        .map(|r| r.id)
        .collect();
    let layout = board_tui::view::detail_layout(&d.app, d.app.last_area);
    let visible = board_tui::view::runs_viewport_height(&layout).max(1);
    assert!(
        ids.len() > visible,
        "fixture must overflow the runs viewport"
    );
    assert_eq!(
        d.app.detail_run_sel,
        ids.len() - 1,
        "opens on the newest run"
    );

    // Wheel far up over the runs section: the offset moves, and the cursor is
    // carried into the rows the wheel brought into view (it must not stay on an
    // off-screen run).
    for _ in 0..10 {
        update(
            &mut d.app,
            wheel(
                MouseEventKind::ScrollUp,
                layout.runs.x + 1,
                layout.runs.y + 1,
            ),
        );
    }
    let offset = d.app.detail_runs_scroll;
    assert!(
        offset + visible < ids.len(),
        "the wheel must have scrolled away from the bottom"
    );
    assert!(
        d.app.detail_run_sel >= offset && d.app.detail_run_sel < offset + visible,
        "selected run {} outside the wheel-scrolled window [{}, {})",
        d.app.detail_run_sel,
        offset,
        offset + visible
    );

    // The marker is really on screen, on the selected run's row.
    let selected_id = d.app.focused_run().unwrap().id;
    let rows = rendered_rows(&d.app);
    let marked: Vec<&String> = rows.iter().filter(|r| r.contains('▸')).collect();
    assert_eq!(marked.len(), 1, "exactly one focus marker: {rows:#?}");
    assert!(
        marked[0].contains(&format!("#{selected_id} ")),
        "the marker must sit on the selected run #{selected_id}: {}",
        marked[0]
    );

    // And `o` targets that visible run, never the now-off-screen newest one.
    let effects = update(&mut d.app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(c, r)] if *c == card.id && *r == selected_id));
    assert_ne!(selected_id, *ids.last().unwrap());

    // Wheeling back down keeps the invariant from the other direction.
    let selected_run_before_scroll_down = d.app.detail_run_sel;
    for _ in 0..40 {
        update(
            &mut d.app,
            wheel(
                MouseEventKind::ScrollDown,
                layout.runs.x + 1,
                layout.runs.y + 1,
            ),
        );
    }
    // Scrolling *past* the cursor drags it along by the nearest edge, so it is
    // still inside the window and has moved forward — never stranded above it.
    let offset = d.app.detail_runs_scroll;
    assert_eq!(offset, ids.len() - visible, "wheel clamps at the bottom");
    assert!(
        d.app.detail_run_sel >= offset && d.app.detail_run_sel < offset + visible,
        "selected run {} outside [{}, {})",
        d.app.detail_run_sel,
        offset,
        offset + visible
    );
    // The cursor never moves backwards under a downward wheel: it stays where
    // it is while still visible, and is dragged forward by the window's top
    // edge otherwise.
    assert!(d.app.detail_run_sel >= selected_run_before_scroll_down);
    assert_eq!(
        rendered_rows(&d.app)
            .iter()
            .filter(|r| r.contains('▸'))
            .count(),
        1
    );
}

#[test]
fn wheel_scrolling_the_comments_section_carries_the_comment_focus_the_same_way() {
    // The comments list is the convention runs follow: a wheel notch is a raw
    // offset move, and the cursor is pulled into the new window afterwards.
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    for i in 0..25 {
        client
            .comment_add(card.id, &format!("wheel comment {i}"), Some("test"))
            .unwrap();
    }
    let mut d = driver_with_detail_open(client, card.id);
    assert_eq!(d.app.detail_scroll_target, DetailScrollTarget::Comments);
    let layout = board_tui::view::detail_layout(&d.app, d.app.last_area);
    let (_, visible) = board_tui::view::comments_viewport(&d.app, &layout);

    for _ in 0..12 {
        update(
            &mut d.app,
            wheel(
                MouseEventKind::ScrollUp,
                layout.comments.x + 1,
                layout.comments.y + 1,
            ),
        );
    }
    let spans =
        board_tui::view::comment_row_spans(d.app.detail.as_ref().unwrap(), layout.comments.width);
    let (start, len) = spans[d.app.detail_comment_sel];
    let lo = d.app.detail_comments_scroll;
    assert!(
        start < lo + visible && start + len > lo,
        "focused comment rows [{start}, {}) outside the window [{lo}, {})",
        start + len,
        lo + visible
    );
    let rows = rendered_rows(&d.app);
    assert_eq!(
        rows.iter().filter(|r| r.contains('▸')).count(),
        1,
        "the focused-comment marker must stay on screen: {rows:#?}"
    );
}

#[test]
fn o_without_a_loaded_detail_toasts_instead_of_doing_nothing() {
    let mut app = demo_app();
    app.screen = Screen::CardDetail;
    app.detail = None;
    let effects = update(&mut app, key(KeyCode::Char('o')));
    assert!(effects.is_empty(), "no run can be named without a detail");
    let toast = app.toast.as_ref().expect("o must explain itself");
    assert!(toast.is_error);
    assert!(
        toast.text.contains("not loaded"),
        "toast should say the detail has not loaded: {}",
        toast.text
    );
}

#[test]
fn o_focuses_the_selected_older_run_not_the_newest() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    let seeded = seed_runs(&client, &card, 3, true);
    let mut d = driver_with_detail_open(client, card.id);
    d.app.detail_scroll_target = DetailScrollTarget::Runs;
    let runs: Vec<i64> = d
        .app
        .detail
        .as_ref()
        .unwrap()
        .runs
        .iter()
        .map(|r| r.id)
        .collect();

    // Walk the cursor onto the *oldest* run and jump to it.
    for _ in 0..runs.len() {
        update(&mut d.app, key(KeyCode::Up));
    }
    let effects = update(&mut d.app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(c, r)] if *c == card.id && *r == runs[0]));
    assert_ne!(runs[0], *seeded.last().unwrap());

    // One row down: the emitted run id tracks the highlighted row exactly.
    update(&mut d.app, key(KeyCode::Down));
    let effects = update(&mut d.app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(_, r)] if *r == runs[1]));

    // `o` also works while the comments section holds key focus, still on the
    // selected run.
    update(&mut d.app, key(KeyCode::Tab));
    assert_eq!(d.app.detail_scroll_target, DetailScrollTarget::Comments);
    let effects = update(&mut d.app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(_, r)] if *r == runs[1]));
}

#[test]
fn o_on_a_run_whose_pane_is_gone_toasts_the_rescue_and_keeps_the_board_usable() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    // The newest run records no pane but does record a claude conversation id,
    // so the daemon (here the fake client) reopens it. The TUI must explain
    // that instead of silently exiting to a pane the user never asked for.
    let paneless = *seed_runs(&client, &card, 1, false).last().unwrap();
    let mut d = driver_with_detail_open(client, card.id);
    d.set_origin_socket(Some("/tmp/herdr.sock".into()));
    assert_eq!(d.app.focused_run().unwrap().id, paneless);

    d.handle(key(KeyCode::Char('o')));
    assert!(
        !d.app.should_quit,
        "a rescue must keep the board up so its explanation is readable"
    );
    let toast = d.app.toast.as_ref().expect("rescue toast");
    assert!(!toast.is_error, "a successful rescue is not an error");
    assert!(
        toast.text.contains(&format!("#{paneless}")),
        "toast must name the run: {}",
        toast.text
    );
    assert!(
        toast.text.contains("resumed"),
        "toast must say the session was resumed: {}",
        toast.text
    );
    assert!(
        toast.text.contains("ephemeral"),
        "toast must say the new pane is not tracked as a run: {}",
        toast.text
    );
    assert_eq!(d.app.screen, Screen::CardDetail);
}

#[test]
fn o_on_a_run_that_cannot_be_reopened_toasts_an_error_without_quitting() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    // No pane and no conversation id: nothing to focus, nothing to resume.
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
        .promote_run_uow(run.id, Some("w1"), None, None)
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
    let mut d = driver_with_detail_open(client, card.id);
    d.set_origin_socket(Some("/tmp/herdr.sock".into()));

    d.handle(key(KeyCode::Char('o')));
    assert!(!d.app.should_quit, "a refusal must not exit the board");
    let toast = d.app.toast.as_ref().expect("error toast");
    assert!(toast.is_error);
    assert!(
        toast.text.contains(&format!("#{}", run.id)),
        "toast must name the run: {}",
        toast.text
    );
    // The daemon owns the diagnosis; the TUI renders it verbatim and never adds
    // a stale "not available yet" disclaimer.
    assert!(
        toast.text.contains("conversation id"),
        "toast must carry the daemon's reason: {}",
        toast.text
    );
    assert!(
        !toast.text.contains("not available yet"),
        "the pre-rescue wording must be gone: {}",
        toast.text
    );
    assert_eq!(d.app.screen, Screen::CardDetail);
}

#[test]
fn o_on_a_run_whose_harness_cannot_resume_names_the_harness() {
    let mut client = super::helpers::demo_client().unwrap();
    let card = failed_card(&mut client);
    let run = client
        .db()
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            // Not a built-in and not declared in config ⇒ resume unsupported.
            harness: "ghost",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("conv-ghost"),
            session: None,
        })
        .unwrap();
    client
        .db()
        .promote_run_uow(run.id, Some("w1"), None, None)
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
    let mut d = driver_with_detail_open(client, card.id);
    d.set_origin_socket(Some("/tmp/herdr.sock".into()));

    d.handle(key(KeyCode::Char('o')));
    assert!(!d.app.should_quit);
    let toast = d.app.toast.as_ref().expect("error toast");
    assert!(toast.is_error);
    assert!(
        toast.text.contains("ghost"),
        "toast must name the harness that cannot resume: {}",
        toast.text
    );
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
    // `o` emits exactly the selected run — never a re-derived "latest with a
    // pane" (see `o_focuses_the_selected_older_run_not_the_newest`).
    let expected_run = app.focused_run().expect("running card has a run").id;
    let effects = update(&mut app, key(KeyCode::Char('o')));
    assert!(matches!(effects.as_slice(),
            [Effect::FocusRun(card, run)] if *card == running.id && *run == expected_run));

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

    // Runs focused: `Down` moves the *run* cursor and leaves the comment
    // cursor alone; the two sections keep independent state.
    let comment_sel = app.detail_comment_sel;
    update(&mut app, key(KeyCode::Tab));
    assert_eq!(app.detail_scroll_target, DetailScrollTarget::Runs);
    let run_sel = app.detail_run_sel;
    update(&mut app, key(KeyCode::Down));
    assert_eq!(app.detail_comment_sel, comment_sel);
    assert_eq!(app.detail_run_sel, run_sel + 1);
    assert_eq!(
        app.detail_comments_scroll, 0,
        "the runs cursor must not scroll the comments section"
    );
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
    let runs_visible = board_tui::view::runs_viewport_height(&layout);
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
    let runs_visible = board_tui::view::runs_viewport_height(&layout);
    assert_eq!(
        app.detail_comments_scroll,
        detail.comments.len().saturating_sub(comments_visible),
        "comments re-anchor to the latest visible row"
    );
    assert_eq!(
        app.detail_runs_scroll,
        detail.runs.len().saturating_sub(runs_visible),
        "runs re-anchor to the latest visible row"
    );
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
