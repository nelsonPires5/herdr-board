use board_core::client::BoardClient;
use board_core::protocol::{
    CardCreateParams, CardMoveParams, CardStatus, ColumnCreateParams, RunOutcome, SpaceKind,
    Trigger,
};

use super::{col, fake_card, poll, todo_id, TestDaemon};

#[test]
fn happy_pipeline() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let review = c.column_create(&col("review-h", Trigger::Manual)).unwrap();
    let work = c
        .column_create(&ColumnCreateParams {
            on_success_column_id: Some(review.id),
            ..col("work", Trigger::Auto)
        })
        .unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let done = poll(&mut c, 15, |c| {
        let d = c.card_get(card.id).unwrap();
        d.card.column_id == review.id && d.card.status == CardStatus::Idle
    });
    assert!(done, "card should auto-move to review-h and go idle");

    let d = c.card_get(card.id).unwrap();
    assert!(
        d.comments
            .iter()
            .any(|cm| cm.body == "fake: done work" && cm.author.starts_with("agent:")),
        "agent comment present with agent author"
    );
    assert!(
        d.comments.iter().any(|cm| cm.author == "system"),
        "system transition comment present"
    );
    let run = d.runs.iter().find(|r| r.column_id == work.id).unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Ok));
    assert!(run.started_at.is_some() && run.ended_at.is_some());
}

#[test]
fn fail_path_applies_on_fail() {
    let td = TestDaemon::start(&[("FAKE_AGENT_OUTCOME", "fail")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let back = c.column_create(&col("back", Trigger::Manual)).unwrap();
    let work = c
        .column_create(&ColumnCreateParams {
            on_fail_column_id: Some(back.id),
            ..col("work", Trigger::Auto)
        })
        .unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let landed = poll(&mut c, 15, |c| {
        c.card_get(card.id).unwrap().card.column_id == back.id
    });
    assert!(landed, "failed card should land in on_fail column");
    let d = c.card_get(card.id).unwrap();
    let run = d.runs.iter().find(|r| r.column_id == work.id).unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Fail));
    assert!(d.comments.iter().any(|cm| cm.author == "system"));
}

#[test]
fn process_exit_without_done() {
    let td = TestDaemon::start(&[("FAKE_AGENT_SILENT", "1")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let review = c.column_create(&col("review-h", Trigger::Manual)).unwrap();
    let back = c.column_create(&col("back", Trigger::Manual)).unwrap();
    let work = c
        .column_create(&ColumnCreateParams {
            on_success_column_id: Some(review.id),
            on_fail_column_id: Some(back.id),
            ..col("work", Trigger::Auto)
        })
        .unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let failed = poll(&mut c, 15, |c| {
        c.card_get(card.id).unwrap().card.status == CardStatus::Failed
    });
    assert!(failed, "silent-exit card should end failed");
    let d = c.card_get(card.id).unwrap();
    assert_eq!(d.card.column_id, work.id, "no transition on pane exit");
    let run = d.runs.iter().find(|r| r.column_id == work.id).unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Fail));
    assert!(d
        .comments
        .iter()
        .any(|cm| cm.body.contains("pane exited without board done")));
}

#[test]
fn timeout_kills_and_applies_on_fail() {
    let td = TestDaemon::start(&[("BOARD_TIMEOUT_UNIT_SECS", "1"), ("FAKE_AGENT_SLEEP", "10")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let back = c.column_create(&col("back", Trigger::Manual)).unwrap();
    let work = c
        .column_create(&ColumnCreateParams {
            on_fail_column_id: Some(back.id),
            timeout_minutes: Some(1), // 1 * 1s unit = ~1s
            ..col("work", Trigger::Auto)
        })
        .unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let landed = poll(&mut c, 15, |c| {
        c.card_get(card.id).unwrap().card.column_id == back.id
    });
    assert!(
        landed,
        "timed-out card should be killed and moved to on_fail"
    );
    let d = c.card_get(card.id).unwrap();
    let run = d.runs.iter().find(|r| r.column_id == work.id).unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Fail));
    assert!(d.comments.iter().any(|cm| cm.body.contains("timed out")));
}

#[test]
fn queue_serialization_same_space() {
    let td = TestDaemon::start(&[("FAKE_AGENT_SLEEP", "2")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let review = c.column_create(&col("review-h", Trigger::Manual)).unwrap();
    let work = c
        .column_create(&ColumnCreateParams {
            on_success_column_id: Some(review.id),
            ..col("work", Trigger::Auto)
        })
        .unwrap();
    // Two cards with the same (default) space key -> must run serially.
    let a = c.card_create(&fake_card(todo)).unwrap();
    let b = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: a.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();
    c.card_move(&CardMoveParams {
        id: b.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let both_done = poll(&mut c, 25, |c| {
        c.card_get(a.id).unwrap().card.column_id == review.id
            && c.card_get(b.id).unwrap().card.column_id == review.id
    });
    assert!(both_done, "both cards should complete");

    let mut runs: Vec<_> = c
        .card_get(a.id)
        .unwrap()
        .runs
        .into_iter()
        .chain(c.card_get(b.id).unwrap().runs)
        .filter(|r| r.column_id == work.id)
        .collect();
    runs.sort_by(|x, y| x.started_at.cmp(&y.started_at));
    assert_eq!(runs.len(), 2);
    let first_end = runs[0].ended_at.clone().unwrap();
    let second_start = runs[1].started_at.clone().unwrap();
    assert!(
        second_start >= first_end,
        "second run ({second_start}) should start after first ends ({first_end})"
    );
}

#[test]
fn cancel_running_card() {
    let td = TestDaemon::start(&[("FAKE_AGENT_SLEEP", "10")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let work = c.column_create(&col("work", Trigger::Auto)).unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let running = poll(&mut c, 10, |c| {
        c.card_get(card.id).unwrap().card.status == CardStatus::Running
    });
    assert!(running, "card should reach running");

    let res = c.run_cancel(card.id).unwrap();
    assert_eq!(res.run.outcome, Some(RunOutcome::Cancelled));
    let d = c.card_get(card.id).unwrap();
    assert_eq!(d.card.status, CardStatus::Failed);
    assert_eq!(d.card.column_id, work.id, "cancel does not transition");
}

#[test]
fn retry_creates_new_forked_run() {
    let td = TestDaemon::start(&[("FAKE_AGENT_OUTCOME", "ok")]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    let work = c.column_create(&col("work", Trigger::Auto)).unwrap();
    let card = c.card_create(&fake_card(todo)).unwrap();
    c.card_move(&CardMoveParams {
        id: card.id,
        column_id: work.id,
        position: None,
    })
    .unwrap();

    let done = poll(&mut c, 15, |c| {
        let d = c.card_get(card.id).unwrap();
        d.card.status == CardStatus::Done && d.runs.iter().any(|r| r.ended_at.is_some())
    });
    assert!(
        done,
        "first run should finish and the card go done (ok, no target column)"
    );
    let first = c.card_get(card.id).unwrap();
    let session = first.card.session_id.clone();
    assert!(session.is_some(), "first run mints a session");
    assert_eq!(first.runs.len(), 1);

    c.run_retry(card.id).unwrap();
    let two = poll(&mut c, 15, |c| c.card_get(card.id).unwrap().runs.len() == 2);
    assert!(two, "retry creates a new run row");
    let d = c.card_get(card.id).unwrap();
    let new_run = d.runs.iter().max_by_key(|r| r.id).unwrap();
    assert_eq!(
        new_run.session_id, session,
        "retry forks/reuses the same session id"
    );
}

#[test]
fn local_spawner_missing_pi_surfaces_clean_run_failure() {
    let td = TestDaemon::start(&[("PATH", "/usr/bin:/bin")]);
    let mut c = td.client();
    let board = c
        .board_open(td._dir.path().canonicalize().unwrap().to_str().unwrap())
        .unwrap()
        .board;
    c.column_create(&ColumnCreateParams {
        board_id: Some(board.id),
        ..col("work", Trigger::Auto)
    })
    .unwrap();
    let out = td.board(&[
        "card", "new", "--title", "missing", "--column", "work", "--json",
    ]);
    assert!(out.status.success());
    let card: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = card["id"].as_i64().unwrap();
    assert!(poll(&mut c, 10, |client| {
        client.card_get(id).unwrap().card.status == CardStatus::Failed
    }));
    let detail = c.card_get(id).unwrap();
    assert_eq!(detail.runs[0].outcome, Some(RunOutcome::Fail));
    assert!(detail.comments.iter().any(|comment| {
        comment.author == "system"
            && comment.body.contains("spawn failed")
            && comment.body.contains("pi")
    }));
}

#[test]
fn scoped_template_dispatches_and_transitions_with_local_spawner() {
    let td = TestDaemon::start(&[]);
    let scope = td._dir.path().join("scoped-pipeline");
    std::fs::create_dir_all(&scope).unwrap();
    let scope = scope.canonicalize().unwrap();
    let mut client = td.client();
    let board = client.board_open(scope.to_str().unwrap()).unwrap().board;
    let columns = client
        .template_apply_for_board("pipeline", Some(board.id))
        .unwrap();
    let todo = columns.iter().find(|c| c.name == "Todo").unwrap().id;
    let execute = columns.iter().find(|c| c.name == "Execute").unwrap().id;
    let human = columns
        .iter()
        .find(|c| c.name == "Human Review")
        .unwrap()
        .id;
    let card = client
        .card_create(&CardCreateParams {
            board_id: Some(board.id),
            title: "scoped dispatch".into(),
            description: Some("do scoped work".into()),
            harness: Some("fake".into()),
            column_id: Some(todo),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("scoped-space".into()),
            ..Default::default()
        })
        .unwrap();
    client
        .card_move(&CardMoveParams {
            id: card.id,
            column_id: execute,
            position: None,
        })
        .unwrap();

    assert!(poll(&mut client, 8, |c| {
        let card = c.card_get(card.id).unwrap().card;
        card.board_id == board.id && card.column_id == human && card.status == CardStatus::Idle
    }));
}
