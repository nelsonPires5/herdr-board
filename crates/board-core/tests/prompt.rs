//! Prompt assembly + effective-settings resolution tests.

use board_core::engine::ValidationError;
use board_core::model::{Card, Column, Comment};
use board_core::prompt::{assemble_prompt, effective_settings, PROMPT_CLOSEOUT};
use board_core::protocol::{CardStatus, Effort, SpaceKind, Trigger};

fn comment(id: i64, author: &str, ts: &str, body: &str) -> Comment {
    Comment {
        id,
        card_id: 1,
        author: author.to_string(),
        body: body.to_string(),
        created_at: ts.to_string(),
    }
}

#[test]
fn prompt_without_comments_is_description_plus_closeout() {
    let out = assemble_prompt("do the thing", &[]);
    assert_eq!(out, format!("do the thing\n\n{PROMPT_CLOSEOUT}"));
}

#[test]
fn prompt_with_comments_appends_section() {
    let comments = vec![
        comment(1, "user", "2026-07-14 10:00", "context one"),
        comment(2, "agent:5", "2026-07-14 10:05", "did stuff"),
    ];
    let out = assemble_prompt("base", &comments);
    assert_eq!(
        out,
        format!(
            "base\n\n## Card comments\nuser (2026-07-14 10:00): context one\nagent:5 (2026-07-14 10:05): did stuff\n\n{PROMPT_CLOSEOUT}"
        )
    );
}

#[test]
fn prompt_truncates_to_last_20_in_order() {
    let comments: Vec<Comment> = (1..=25)
        .map(|i| comment(i, "user", "t", &format!("c{i}")))
        .collect();
    let out = assemble_prompt("base", &comments);
    // Oldest kept is c6 (25 - 20 + 1); c5 and earlier dropped; newest c25 present.
    assert!(out.contains("c6"));
    assert!(!out.contains("c5:") && !out.contains(" c5\n") && !out.contains("): c5"));
    assert!(out.contains("): c25"));
    // Exactly 20 comment lines.
    let lines = out.lines().filter(|l| l.starts_with("user (")).count();
    assert_eq!(lines, 20);
    // Order preserved: c6 appears before c7.
    let i6 = out.find("): c6").unwrap();
    let i7 = out.find("): c7").unwrap();
    assert!(i6 < i7);
}

fn base_card() -> Card {
    Card {
        id: 1,
        board_id: 1,
        column_id: 2,
        position: 0,
        title: "t".into(),
        description: "d".into(),
        harness: "claude".into(),
        model: Some("sonnet".into()),
        effort: Some(Effort::Medium),
        permission_mode: Some("acceptEdits".into()),
        space_kind: SpaceKind::Workspace,
        space_ref: Some("w4".into()),
        worktree_base: None,
        status: CardStatus::Idle,
        session_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    }
}

fn base_column() -> Column {
    Column {
        id: 2,
        board_id: 1,
        name: "Review".into(),
        position: 2,
        system_prompt: Some("be adversarial".into()),
        trigger: Trigger::Auto,
        on_success_column_id: None,
        on_fail_column_id: None,
        fresh_session: true,
        harness_override: None,
        model_override: None,
        effort_override: None,
        permission_override: None,
        timeout_minutes: Some(30),
    }
}

#[test]
fn effective_settings_use_card_values_by_default() {
    let s = effective_settings(&base_card(), &base_column()).unwrap();
    assert_eq!(s.harness, "claude");
    assert_eq!(s.model.as_deref(), Some("sonnet"));
    assert_eq!(s.effort, Some(Effort::Medium));
    assert_eq!(s.permission_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(s.system_prompt.as_deref(), Some("be adversarial"));
    assert!(s.fresh_session);
    assert_eq!(s.timeout_minutes, Some(30));
}

#[test]
fn column_overrides_win() {
    let mut col = base_column();
    col.model_override = Some("opus".into());
    col.effort_override = Some("high".into());
    col.harness_override = Some("fake".into());
    let s = effective_settings(&base_card(), &col).unwrap();
    assert_eq!(s.harness, "fake");
    assert_eq!(s.model.as_deref(), Some("opus"));
    assert_eq!(s.effort, Some(Effort::High));
}

#[test]
fn bypass_column_override_is_refused() {
    let mut col = base_column();
    col.permission_override = Some("bypassPermissions".into());
    assert_eq!(
        effective_settings(&base_card(), &col).unwrap_err(),
        ValidationError::BypassNotAllowed
    );
}
