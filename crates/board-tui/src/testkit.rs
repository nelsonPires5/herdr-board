//! Demo/seed helpers shared by the example binary and the snapshot tests.
//! Only compiled with the `fake-client` feature.

use board_core::capability::{claude_capabilities, pi_capabilities};
use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{
    CardCreateParams, CardStatus, ColumnCreateParams, Effort, Event, RunOutcome, SessionInfo,
    SessionListResult, SpaceInfo, SpaceKind, SpaceListResult, Trigger,
};
use serde_json::{json, Value};

/// A [`FakeBoardClient`] wrapper that also answers the catalog RPCs
/// (`harness.capabilities` / `session.list` / `space.list`) which the real
/// daemon serves but the bare fake does not. Everything else delegates to the
/// inner fake.
///
/// `space.list` is session-scoped: the default session returns [`demo_spaces`],
/// a named session returns a different set (so tests can observe the workspace
/// list re-fetching when the session field changes).
///
/// Tests can stub failures (`without_caps` / `without_spaces` /
/// `without_sessions`) to exercise the form's fallback paths.
pub struct DemoClient {
    inner: FakeBoardClient,
    caps_available: bool,
    spaces: Option<Vec<SpaceInfo>>,
    sessions: Option<Vec<SessionInfo>>,
}

impl DemoClient {
    pub fn new(inner: FakeBoardClient) -> DemoClient {
        DemoClient {
            inner,
            caps_available: true,
            spaces: Some(demo_spaces()),
            sessions: Some(demo_sessions()),
        }
    }

    /// Make `harness.capabilities` fail (form falls back to free-text model).
    pub fn without_caps(mut self) -> DemoClient {
        self.caps_available = false;
        self
    }

    /// Make `space.list` fail (space ref falls back to free-text).
    pub fn without_spaces(mut self) -> DemoClient {
        self.spaces = None;
        self
    }

    /// Make `session.list` fail (session selector keeps just `(default)`).
    pub fn without_sessions(mut self) -> DemoClient {
        self.sessions = None;
        self
    }

    /// Access the seeded store (parity with `FakeBoardClient::db`).
    pub fn db(&self) -> &board_core::db::Db {
        self.inner.db()
    }
}

impl BoardClient for DemoClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        match method {
            "harness.capabilities" if self.caps_available => {
                match params.get("harness").and_then(Value::as_str) {
                    Some("pi") => Ok(json!(pi_capabilities())),
                    Some("claude") => Ok(json!(claude_capabilities())),
                    Some(other) => anyhow::bail!("unknown harness: {other}"),
                    None => anyhow::bail!("missing harness"),
                }
            }
            "harness.capabilities" => {
                anyhow::bail!("harness.capabilities: stubbed failure")
            }
            "space.list" => match &self.spaces {
                Some(_) => {
                    let session = params.get("session").and_then(|v| v.as_str());
                    Ok(json!(SpaceListResult {
                        spaces: demo_spaces_for(session)
                    }))
                }
                None => anyhow::bail!("space.list: stubbed failure"),
            },
            "session.list" => match &self.sessions {
                Some(s) => Ok(json!(SessionListResult {
                    sessions: s.clone()
                })),
                None => anyhow::bail!("session.list: stubbed failure"),
            },
            _ => self.inner.call(method, params),
        }
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

/// Demo sessions surfaced by the stubbed `session.list`.
pub fn demo_sessions() -> Vec<SessionInfo> {
    vec![
        SessionInfo {
            name: "default".to_string(),
            default: true,
            running: true,
        },
        SessionInfo {
            name: "feature".to_string(),
            default: false,
            running: true,
        },
    ]
}

/// Demo workspaces for the default session. `w4` matches the seeded running
/// card's `space_ref`, so editing it preselects that workspace.
pub fn demo_spaces() -> Vec<SpaceInfo> {
    vec![
        SpaceInfo {
            id: "w4".to_string(),
            label: "MELI scraper".to_string(),
        },
        SpaceInfo {
            id: "w1".to_string(),
            label: "auth refactor".to_string(),
        },
        SpaceInfo {
            id: "w7".to_string(),
            label: "docs site".to_string(),
        },
    ]
}

/// Workspaces for a given session. The default session (`None` / `"default"`)
/// gets [`demo_spaces`]; the `"feature"` session gets its own single workspace
/// so a session change visibly re-scopes the list.
pub fn demo_spaces_for(session: Option<&str>) -> Vec<SpaceInfo> {
    match session {
        Some("feature") => vec![SpaceInfo {
            id: "w9".to_string(),
            label: "feature sandbox".to_string(),
        }],
        _ => demo_spaces(),
    }
}

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
        harness: Some("claude".to_string()),
        ..Default::default()
    }
}

/// A pipeline board with cards in every status, plus comments and run history —
/// enough to exercise every glyph and the detail view. Wrapped in a
/// [`DemoClient`] so the catalog RPCs (capabilities / spaces) resolve.
pub fn demo_client() -> anyhow::Result<DemoClient> {
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
        None,
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
        None,
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
        None,
    )?;
    c.db().start_run(r2.id, Some("w1"), Some("p3"))?;
    c.db()
        .finish_run(r2.id, RunOutcome::Fail, Some("tests failed"))?;

    // Done — idle
    c.card_create(&card("Ship v0.1", done, "Cut the first release."))?;

    // Additional independent boards feed the board picker while Global remains
    // the selected demo board.
    c.board_open("/work/alpha/project")?;
    c.board_open("/Volumes/archive/project")?;

    Ok(DemoClient::new(c))
}
