//! Daemon integration tests exercising the real `board` binary and boardd with
//! the `LocalSpawner` and the fake harness (no herdr, no Claude cost). Each test
//! gets its own temp DB, socket, config, and daemon process.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{
    CardCreateParams, CardMoveParams, CardStatus, ColumnCreateParams, Event, RunOutcome, Trigger,
};

const BOARD_BIN: &str = env!("CARGO_BIN_EXE_board");

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A daemon under test, torn down on drop.
struct TestDaemon {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl TestDaemon {
    fn start(extra: &[(&str, &str)]) -> TestDaemon {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("board.db");
        let socket = dir.path().join("boardd.sock");
        let cfg = dir.path().join("config.toml");
        let fake = fixtures_dir().join("fake-agent.sh");
        std::fs::write(
            &cfg,
            format!(
                "[harness.fake]\nargv = [\"bash\", \"{}\"]\n\n[daemon]\nspawner = \"local\"\n",
                fake.display()
            ),
        )
        .unwrap();

        let mut cmd = Command::new(BOARD_BIN);
        cmd.arg("daemon").arg("--foreground");
        cmd.env("BOARD_DB", &db)
            .env("BOARD_SOCKET", &socket)
            .env("HERDR_BOARD_CONFIG", &cfg)
            .env("BOARD_SPAWNER", "local")
            .env("BOARD_BIN", BOARD_BIN)
            .env("HOME", dir.path())
            .env("BOARD_TICK_MS", "150")
            .env("BOARD_LOCAL_POLL_MS", "150")
            .env("FAKE_AGENT_SLEEP", "0.3")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn daemon");

        let td = TestDaemon {
            child,
            socket,
            _dir: dir,
        };
        td.wait_ready();
        td
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(mut c) = UnixClient::connect(&self.socket) {
                if c.daemon_status().is_ok() {
                    return;
                }
            }
            if Instant::now() >= deadline {
                panic!("daemon did not become ready");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn client(&self) -> UnixClient {
        UnixClient::connect(&self.socket).expect("connect")
    }

    /// Run the `board` binary against this daemon's socket and capture its output.
    fn board(&self, args: &[&str]) -> std::process::Output {
        self.board_in(self._dir.path(), args)
    }

    fn board_in(&self, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new(BOARD_BIN)
            .args(args)
            .current_dir(cwd)
            .env("BOARD_SOCKET", &self.socket)
            .env("BOARD_DB", self._dir.path().join("board.db"))
            .env("HERDR_BOARD_CONFIG", self._dir.path().join("config.toml"))
            .env("HOME", self._dir.path())
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .stdin(Stdio::null())
            .output()
            .expect("run board binary")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// -- helpers -----------------------------------------------------------------

fn col(name: &str, trigger: Trigger) -> ColumnCreateParams {
    ColumnCreateParams {
        name: name.to_string(),
        trigger: Some(trigger),
        ..Default::default()
    }
}

fn fake_card(column_id: i64) -> CardCreateParams {
    CardCreateParams {
        title: "task".to_string(),
        description: Some("do the thing".to_string()),
        harness: Some("fake".to_string()),
        column_id: Some(column_id),
        ..Default::default()
    }
}

fn todo_id(c: &mut UnixClient) -> i64 {
    c.board_get().unwrap().columns[0].id
}

/// Poll `pred` until it returns true or the timeout elapses.
fn poll(c: &mut UnixClient, secs: u64, mut pred: impl FnMut(&mut UnixClient) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred(c) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

// -- tests -------------------------------------------------------------------

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
    // Auto column with no transitions: card stays put after an ok run.
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
        d.card.status == CardStatus::Idle && d.runs.iter().any(|r| r.ended_at.is_some())
    });
    assert!(done, "first run should finish and card go idle");
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
fn template_apply_on_empty_board() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let cols = c.template_apply("pipeline").unwrap();
    let names: Vec<&str> = cols.iter().map(|x| x.name.as_str()).collect();
    for expected in ["Todo", "Plan", "Execute", "Review", "Human Review", "Done"] {
        assert!(names.contains(&expected), "missing column {expected}");
    }
    let find = |n: &str| cols.iter().find(|x| x.name == n).unwrap();
    assert_eq!(find("Plan").on_success_column_id, Some(find("Execute").id));
    assert_eq!(find("Plan").on_fail_column_id, Some(find("Todo").id));
    assert_eq!(
        find("Review").on_success_column_id,
        Some(find("Human Review").id)
    );
    assert_eq!(find("Review").on_fail_column_id, Some(find("Execute").id));
    assert_eq!(find("Review").model_override.as_deref(), Some("opus"));
}

#[test]
fn template_refused_on_non_empty_board() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    c.card_create(&fake_card(todo)).unwrap();
    let err = c.template_apply("pipeline").unwrap_err();
    assert!(
        err.to_string().contains("error 3"),
        "expected invalid-state error, got: {err}"
    );
}

#[test]
fn single_instance_second_exits_zero() {
    let td = TestDaemon::start(&[]);
    // A second daemon on the same DB must lose the flock race and exit 0.
    let dir = td._dir.path();
    let mut cmd = Command::new(BOARD_BIN);
    cmd.arg("daemon")
        .env("BOARD_DB", dir.join("board.db"))
        .env("BOARD_SOCKET", dir.join("boardd.sock"))
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut second = cmd.spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = second.try_wait().unwrap() {
            assert!(status.success(), "second daemon should exit 0");
            break;
        }
        if Instant::now() >= deadline {
            let _ = second.kill();
            panic!("second daemon did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Original daemon still serving.
    assert!(td.client().daemon_status().is_ok());
}

#[test]
fn subscribe_receives_board_changed_on_card_create() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let todo = todo_id(&mut c);

    let mut sub = c.subscribe().unwrap();
    // Give the daemon a moment to register the subscription's forwarder.
    std::thread::sleep(Duration::from_millis(300));

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let handle = std::thread::spawn(move || {
        if let Some(ev) = sub.next() {
            let _ = tx.send(ev);
        }
    });

    // Trigger an event on a separate connection.
    let mut c2 = td.client();
    c2.card_create(&fake_card(todo)).unwrap();

    let ev = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive an event");
    assert!(matches!(ev, Event::BoardChanged { .. }));
    let _ = handle.join();
}

// -- harness / space CLI verbs -----------------------------------------------

#[test]
fn harness_models_claude_json_and_human() {
    let td = TestDaemon::start(&[]);

    // --json: full HarnessCapabilities — 4 models, 5 efforts each, freeform.
    let out = td.board(&["harness", "models", "claude", "--json"]);
    assert!(out.status.success(), "harness models --json should succeed");
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_slice(&out.stdout).expect("parse HarnessCapabilities");
    assert_eq!(caps.harness, "claude");
    assert!(caps.model_freeform);
    assert_eq!(caps.models.len(), 4, "claude has 4 known models");
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    for expected in ["fable", "opus", "sonnet", "haiku"] {
        assert!(ids.contains(&expected), "missing model {expected}");
    }
    for m in &caps.models {
        assert_eq!(m.efforts.len(), 5, "{} should list 5 efforts", m.id);
    }

    // human: one line per model with its efforts, plus the freeform note.
    let out = td.board(&["harness", "models", "claude"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines()
            .any(|l| l.starts_with("fable") && l.contains("low medium high xhigh max")),
        "human output lists model efforts; got:\n{text}"
    );
    assert!(
        text.contains("any model string accepted"),
        "human output notes model_freeform; got:\n{text}"
    );
}

#[test]
fn harness_models_default_is_pi() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "models", "--json"]);
    assert!(out.status.success());
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(caps.harness, "pi");
    assert!(caps.models.is_empty());
    assert!(caps.model_freeform);
    assert!(caps
        .default_efforts
        .iter()
        .any(|effort| effort.as_str() == "low"));
}

#[test]
fn harness_models_unknown_harness_errors() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "models", "ghost"]);
    assert!(
        !out.status.success(),
        "unknown harness should exit non-zero"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ghost"), "error names the harness; got: {err}");
    assert!(
        err.contains("error 2") || err.contains("unknown harness"),
        "error surfaces not-found; got: {err}"
    );
}

#[test]
fn harness_efforts_known_and_unknown_model() {
    let td = TestDaemon::start(&[]);

    // Known model: efforts from the catalog, known:true.
    let out = td.board(&[
        "harness", "efforts", "claude", "--model", "sonnet", "--json",
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["model"], "sonnet");
    assert_eq!(v["known"], true);
    assert_eq!(v["efforts"].as_array().unwrap().len(), 5);

    // Unknown-but-freeform model: all efforts, known:false.
    let out = td.board(&["harness", "efforts", "claude", "--model", "gpt-x", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["model"], "gpt-x");
    assert_eq!(v["known"], false);
    assert_eq!(v["efforts"].as_array().unwrap().len(), 5);

    // Human output notes the unknown-but-accepted model.
    let out = td.board(&["harness", "efforts", "claude", "--model", "gpt-x"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("unknown"),
        "notes unknown model; got:\n{text}"
    );
}

#[test]
fn harness_efforts_pi_freeform_model_includes_low() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "harness",
        "efforts",
        "pi",
        "--model",
        "openai-codex/example",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["known"], false);
    assert!(v["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effort| effort == "low"));
}

#[test]
fn harness_permissions_pi_is_empty() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "permissions", "--json"]);
    assert!(out.status.success());
    let modes: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(modes.is_empty());
}

#[test]
fn harness_permissions_matches_claude_modes() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "permissions", "claude", "--json"]);
    assert!(out.status.success());
    let modes: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        modes,
        vec![
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan"
        ]
    );

    // Human output: one mode per line.
    let out = td.board(&["harness", "permissions", "claude"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for mode in [
        "acceptEdits",
        "auto",
        "bypassPermissions",
        "manual",
        "dontAsk",
        "plan",
    ] {
        assert!(
            text.lines().any(|l| l == mode),
            "missing permission line {mode}; got:\n{text}"
        );
    }
}

#[test]
fn space_list_without_herdr_surfaces_error() {
    // The test daemon has no herdr, so space.list yields the herdr-unavailable
    // error (code 4); the CLI must surface it cleanly (non-zero exit + message).
    let td = TestDaemon::start(&[]);
    let out = td.board(&["space", "list"]);
    assert!(!out.status.success(), "space list should exit non-zero");
    assert!(out.stdout.is_empty(), "no rows printed on error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("herdr") && err.contains("error 4"),
        "error surfaces herdr-unavailable; got: {err}"
    );

    // --json path fails the same way (error before any JSON is written).
    let out = td.board(&["space", "list", "--json"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn session_list_without_herdr_surfaces_error() {
    // The test daemon runs the local spawner (no session registry), so
    // session.list yields the herdr-unavailable error (code 4); the CLI surfaces
    // it cleanly (non-zero exit + message, no rows printed).
    let td = TestDaemon::start(&[]);
    let out = td.board(&["session", "list"]);
    assert!(!out.status.success(), "session list should exit non-zero");
    assert!(out.stdout.is_empty(), "no rows printed on error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("herdr") && err.contains("error 4"),
        "error surfaces herdr-unavailable; got: {err}"
    );
}

#[test]
fn card_new_new_workspace_missing_cwd_is_validation_error() {
    // `new-workspace` requires both --space-ref and --space-cwd; omitting cwd
    // must surface the daemon's validation error (code 1).
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "needs cwd",
        "--harness",
        "fake",
        "--space-kind",
        "new-workspace",
        "--space-ref",
        "my-feature",
    ]);
    assert!(
        !out.status.success(),
        "missing space-cwd should exit non-zero"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error 1"),
        "error surfaces the validation code; got: {err}"
    );
}

#[test]
fn card_new_defaults_to_pi_and_claude_remains_explicit() {
    let td = TestDaemon::start(&[]);
    let pi = td.board(&["card", "new", "--title", "default", "--json"]);
    assert!(pi.status.success());
    let pi: serde_json::Value = serde_json::from_slice(&pi.stdout).unwrap();
    assert_eq!(pi["harness"], "pi");

    let claude = td.board(&[
        "card",
        "new",
        "--title",
        "explicit",
        "--harness",
        "claude",
        "--json",
    ]);
    assert!(claude.status.success());
    let claude: serde_json::Value = serde_json::from_slice(&claude.stdout).unwrap();
    assert_eq!(claude["harness"], "claude");
}

#[test]
fn card_new_rejects_pi_permission_mode() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad",
        "--permission",
        "acceptEdits",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("pi does not support permission modes"));
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
fn card_archive_and_restore_cli_roundtrip() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "archive me",
        "--harness",
        "fake",
        "--json",
    ]);
    assert!(out.status.success());
    let card: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = card["id"].as_i64().unwrap().to_string();

    let out = td.board(&["card", "archive", &id, "--json"]);
    assert!(out.status.success(), "archive failed: {:?}", out.stderr);
    let archived: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(archived["archived_at"].is_string());

    let out = td.board(&["card", "restore", &id, "--json"]);
    assert!(out.status.success(), "restore failed: {:?}", out.stderr);
    let restored: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(restored["archived_at"].is_null());
}

#[test]
fn card_new_with_session_persists_and_shows() {
    let td = TestDaemon::start(&[]);
    // Create a card with an explicit --session (into the manual Todo column, so
    // no dispatch / herdr is needed).
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "sessioned",
        "--harness",
        "fake",
        "--session",
        "my-sess",
        "--json",
    ]);
    assert!(out.status.success(), "card new --session should succeed");
    let card: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse Card json");
    assert_eq!(
        card["session"].as_str(),
        Some("my-sess"),
        "session persisted on the created card"
    );
    let id = card["id"].as_i64().expect("card id");

    // `card show` (human) surfaces the session.
    let out = td.board(&["card", "show", &id.to_string()]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("session: my-sess"),
        "card show renders the session; got:\n{text}"
    );
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
            space_kind: Some(board_core::protocol::SpaceKind::Workspace),
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

#[test]
fn cli_scopes_plain_cwds_and_preserves_global() {
    let td = TestDaemon::start(&[]);
    let one = td._dir.path().join("plain-one");
    let two = td._dir.path().join("plain-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();

    let created_one = td.board_in(&one, &["card", "new", "--title", "one", "--json"]);
    assert!(created_one.status.success(), "{:?}", created_one.stderr);
    let created_two = td.board_in(&two, &["card", "new", "--title", "two", "--json"]);
    assert!(created_two.status.success(), "{:?}", created_two.stderr);

    let listed_one = td.board_in(&one, &["card", "list", "--json"]);
    let cards_one: serde_json::Value = serde_json::from_slice(&listed_one.stdout).unwrap();
    assert_eq!(cards_one.as_array().unwrap().len(), 1);
    assert_eq!(cards_one[0]["title"], "one");
    let listed_two = td.board_in(&two, &["card", "list", "--json"]);
    let cards_two: serde_json::Value = serde_json::from_slice(&listed_two.stdout).unwrap();
    assert_eq!(cards_two.as_array().unwrap().len(), 1);
    assert_eq!(cards_two[0]["title"], "two");

    let mut client = td.client();
    assert!(client.board_get().unwrap().cards.is_empty());
    assert_eq!(client.board_list().unwrap().boards.len(), 3);
}

#[test]
fn cli_git_root_and_subdirectory_share_board() {
    let td = TestDaemon::start(&[]);
    let repo = td._dir.path().join("repo");
    let sub = repo.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());

    let created = td.board_in(&repo, &["card", "new", "--title", "shared", "--json"]);
    assert!(created.status.success(), "{:?}", created.stderr);
    let listed = td.board_in(&sub, &["card", "list", "--json"]);
    assert!(listed.status.success(), "{:?}", listed.stderr);
    let cards: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(cards.as_array().unwrap().len(), 1);
    assert_eq!(cards[0]["title"], "shared");
    assert_eq!(td.client().board_list().unwrap().boards.len(), 2);
}

#[test]
fn move_resolves_column_in_cards_board_not_current_cwd() {
    let td = TestDaemon::start(&[]);
    let alpha_path = td._dir.path().join("alpha");
    let beta_path = td._dir.path().join("beta");
    std::fs::create_dir_all(&alpha_path).unwrap();
    std::fs::create_dir_all(&beta_path).unwrap();
    let alpha_path = alpha_path.canonicalize().unwrap();
    let beta_path = beta_path.canonicalize().unwrap();

    let mut client = td.client();
    let alpha = client
        .board_open(alpha_path.to_str().unwrap())
        .unwrap()
        .board;
    let beta = client
        .board_open(beta_path.to_str().unwrap())
        .unwrap()
        .board;
    let alpha_done = client
        .column_create(&ColumnCreateParams {
            board_id: Some(alpha.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let beta_done = client
        .column_create(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let card = client
        .card_create(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "move me".into(),
            ..Default::default()
        })
        .unwrap();

    let moved = td.board_in(
        &beta_path,
        &["move", &card.id.to_string(), "Done", "--json"],
    );
    assert!(moved.status.success(), "{:?}", moved.stderr);
    let moved: serde_json::Value = serde_json::from_slice(&moved.stdout).unwrap();
    assert_eq!(moved["column_id"], alpha_done.id);
    assert_ne!(moved["column_id"], beta_done.id);
}
