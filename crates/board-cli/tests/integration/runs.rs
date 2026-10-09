//! Canonical `card run` verbs against a live run: done, retry, and cancel.
//!
//! The legacy top-level `done` / `retry` / `cancel` aliases exercise the same
//! workflow; see `compat.rs`, which delegates to [`run_done_retry_cancel_workflow`]
//! with [`RunCliStyle::Alias`].

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, CardStatus, ColumnCreateParams, Trigger};

use super::{json_output, poll, todo_id, TestDaemon};

/// CLI spelling under test: canonical `card run <verb>` vs the legacy
/// top-level `<verb>` alias.
#[derive(Clone, Copy)]
pub(crate) enum RunCliStyle {
    Canonical,
    Alias,
}

/// Shared done → retry → cancel workflow for both CLI spellings.
///
/// Asserts outcome/status transitions and distinct run IDs at each step, not
/// just that the echoed card ID matches.
pub(crate) fn run_done_retry_cancel_workflow(style: RunCliStyle) {
    let td = TestDaemon::start(&[("FAKE_AGENT_SLEEP", "10")]);
    let mut client = td.client();
    let todo = todo_id(&mut client);
    let work = client
        .column_create(&ColumnCreateParams {
            name: "run-work".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = client
        .card_create(&CardCreateParams {
            title: "run card".into(),
            harness: Some("fake".into()),
            column_id: Some(todo),
            ..Default::default()
        })
        .unwrap();
    client
        .card_move(&board_core::protocol::CardMoveParams {
            id: card.id,
            column_id: work.id,
            board_id: None,
            position: None,
        })
        .unwrap();
    assert!(poll(&mut client, 10, |c| {
        c.card_get(card.id).unwrap().card.status == CardStatus::Running
    }));

    let first_run_id = client
        .card_get(card.id)
        .unwrap()
        .runs
        .iter()
        .find(|r| r.ended_at.is_none())
        .expect("open run while running")
        .id;

    let id = card.id.to_string();
    let done = match style {
        RunCliStyle::Canonical => {
            json_output(&td.board(&["card", "run", "done", &id, "--outcome", "ok", "--json"]))
        }
        RunCliStyle::Alias => json_output(&td.board(&["done", &id, "--outcome", "ok", "--json"])),
    };
    assert_eq!(done["card"]["id"], card.id);
    assert_eq!(done["run"]["outcome"], "ok", "done should close the run ok");
    assert_eq!(
        done["card"]["status"], "done",
        "done ok with no target column completes the card"
    );
    let done_run_id = done["run"]["id"].as_i64().expect("done run id");
    assert_eq!(done_run_id, first_run_id, "done should close the open run");

    let retried = match style {
        RunCliStyle::Canonical => json_output(&td.board(&["card", "run", "retry", &id, "--json"])),
        RunCliStyle::Alias => json_output(&td.board(&["retry", &id, "--json"])),
    };
    assert_eq!(retried["card"]["id"], card.id);
    let retried_run_id = retried["run"]["id"].as_i64().expect("retry run id");
    assert_ne!(
        retried_run_id, done_run_id,
        "retry must mint a distinct run ID, not reuse the closed run"
    );
    assert!(
        retried["run"]["outcome"].is_null(),
        "retried run should start open"
    );

    assert!(poll(&mut client, 10, |c| {
        c.card_get(card.id).unwrap().runs.len() >= 2
    }));
    let detail = client.card_get(card.id).unwrap();
    let mut run_ids: Vec<i64> = detail.runs.iter().map(|r| r.id).collect();
    run_ids.sort_unstable();
    run_ids.dedup();
    assert_eq!(run_ids.len(), 2, "retry must leave two distinct run rows");
    assert!(run_ids.contains(&done_run_id) && run_ids.contains(&retried_run_id));
    let first = detail.runs.iter().find(|r| r.id == done_run_id).unwrap();
    assert_eq!(
        first.outcome,
        Some(board_core::protocol::RunOutcome::Ok),
        "first run should record the done outcome"
    );

    let cancelled = match style {
        RunCliStyle::Canonical => json_output(&td.board(&["card", "run", "cancel", &id, "--json"])),
        RunCliStyle::Alias => json_output(&td.board(&["cancel", &id, "--json"])),
    };
    assert_eq!(cancelled["card"]["id"], card.id);
    assert_eq!(
        cancelled["run"]["outcome"], "cancelled",
        "cancel should close the retried run as cancelled"
    );
    assert_eq!(
        cancelled["run"]["id"], retried_run_id,
        "cancel should close the retried run, not mint another"
    );
    assert_eq!(
        cancelled["card"]["status"], "failed",
        "cancelled card should report failed status"
    );
}

#[test]
fn canonical_card_run_done_cancel_and_retry() {
    run_done_retry_cancel_workflow(RunCliStyle::Canonical);
}
