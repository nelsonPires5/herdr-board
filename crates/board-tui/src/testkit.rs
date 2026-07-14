//! Demo/seed helpers shared by the example binary and the snapshot tests.
//! Only compiled with the `fake-client` feature.

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{
    CardCreateParams, CardStatus, ColumnCreateParams, Effort, RunOutcome, SpaceKind, Trigger,
};

fn col(name: &str, trigger: Trigger) -> ColumnCreateParams {
    ColumnCreateParams {
        name: name.to_string(),
        trigger: Some(trigger),
        ..Default::default()
    }
}

fn card(title: &str, column_id: i64, desc: &str) -> CardCreateParams {
    CardCreateParams {
        title: title.to_string(),
        description: Some(desc.to_string()),
        column_id: Some(column_id),
        ..Default::default()
    }
}

/// A pipeline board with cards in every status, plus comments and run history —
/// enough to exercise every glyph and the detail view.
pub fn demo_client() -> anyhow::Result<FakeBoardClient> {
    let mut c = FakeBoardClient::new()?;

    let todo = c.board_get()?.columns[0].id; // seed "Todo"
    let plan = c.column_create(&col("Plan", Trigger::Auto))?.id;
    let execute = c.column_create(&col("Execute", Trigger::Auto))?.id;
    let review = c.column_create(&col("Review", Trigger::Auto))?.id;
    let _human = c.column_create(&col("Human Review", Trigger::Manual))?.id;
    let done = c.column_create(&col("Done", Trigger::Manual))?.id;

    // Todo — idle
    c.card_create(&card(
        "Update docs",
        todo,
        "Refresh the README and skill docs.",
    ))?;

    // Plan — running
    let running = c
        .card_create(&CardCreateParams {
            model: Some("sonnet".into()),
            effort: Some(Effort::High),
            permission_mode: Some("acceptEdits".into()),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("w4".into()),
            ..card(
                "Add retry to MELI scraper",
                plan,
                "Add exponential backoff to the MELI scraper HTTP client.",
            )
        })?
        .id;
    c.db().set_card_status(running, CardStatus::Running)?;
    let run = c.db().create_run(
        running,
        plan,
        "claude",
        "[\"claude\"]",
        "prompt",
        Some("sess-1"),
    )?;
    c.db().start_run(run.id, Some("w4"), Some("p1"))?;

    // Execute — queued and blocked
    let queued = c
        .card_create(&card(
            "Fix flaky test",
            execute,
            "Stabilise the timing-dependent test.",
        ))?
        .id;
    c.db().set_card_status(queued, CardStatus::Queued)?;
    let blocked = c
        .card_create(&card(
            "Investigate crash",
            execute,
            "Reproduce and fix the null-deref crash.",
        ))?
        .id;
    c.db().set_card_status(blocked, CardStatus::Blocked)?;

    // Review — failed, with comments + run history
    let failed = c
        .card_create(&CardCreateParams {
            model: Some("opus".into()),
            effort: Some(Effort::Medium),
            permission_mode: Some("plan".into()),
            ..card(
                "Refactor auth module",
                review,
                "Split the auth module into token + session layers.",
            )
        })?
        .id;
    c.db().set_card_status(failed, CardStatus::Failed)?;
    c.comment_add(failed, "Plan ready at docs/plans/auth.md", Some("agent:1"))?;
    c.comment_add(
        failed,
        "Reviewer: tests missing for token refresh",
        Some("agent:2"),
    )?;
    c.comment_add(
        failed,
        "Refactor failed in 3m10s -> Execute",
        Some("system"),
    )?;
    let r1 = c.db().create_run(
        failed,
        review,
        "claude",
        "[\"claude\"]",
        "p",
        Some("sess-2"),
    )?;
    c.db().start_run(r1.id, Some("w1"), Some("p2"))?;
    c.db()
        .finish_run(r1.id, RunOutcome::Ok, Some("plan written"))?;
    let r2 = c.db().create_run(
        failed,
        review,
        "claude",
        "[\"claude\"]",
        "p",
        Some("sess-2"),
    )?;
    c.db().start_run(r2.id, Some("w1"), Some("p3"))?;
    c.db()
        .finish_run(r2.id, RunOutcome::Fail, Some("tests failed"))?;

    // Done — idle
    c.card_create(&card("Ship v0.1", done, "Cut the first release."))?;

    Ok(c)
}
