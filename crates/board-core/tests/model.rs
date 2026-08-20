//! Row-struct helpers that classify stored rows.

use board_core::model::{Board, Comment, Project};

fn comment(author: &str) -> Comment {
    Comment {
        id: 1,
        card_id: 7,
        author: author.to_string(),
        body: "body".into(),
        created_at: "2026-07-14 11:58:00".into(),
    }
}

#[test]
fn is_system_only_matches_the_board_authored_author() {
    assert!(comment("system").is_system());
    assert!(!comment("user").is_system());
    assert!(!comment("agent:12").is_system());
    // Exact match: no prefix, suffix, or case folding.
    assert!(!comment("System").is_system());
    assert!(!comment("system:1").is_system());
    assert!(!comment("").is_system());
}

/// Pre-v15 wire payloads have no `archived_at`; parsing them must yield the
/// active state, and serialization must round-trip both states.
#[test]
fn project_and_board_archive_state_parses_pre_v15_payloads() {
    let project: Project =
        serde_json::from_str(r#"{"id":1,"name":"Global","scope_path":null}"#).unwrap();
    assert_eq!(project.archived_at, None);
    let board: Board =
        serde_json::from_str(r#"{"id":1,"project_id":1,"name":"main","scope_path":null}"#).unwrap();
    assert_eq!(board.archived_at, None);

    let archived: Project = serde_json::from_str(
        r#"{"id":2,"name":"x","scope_path":"/x","archived_at":"2026-08-20 10:00:00"}"#,
    )
    .unwrap();
    assert_eq!(archived.archived_at.as_deref(), Some("2026-08-20 10:00:00"));
    let round: Project = serde_json::from_str(&serde_json::to_string(&archived).unwrap()).unwrap();
    assert_eq!(round, archived);
    let board_archived: Board = serde_json::from_str(
        r#"{"id":3,"project_id":2,"name":"b","scope_path":"/x","archived_at":"2026-08-20 10:00:01"}"#,
    )
    .unwrap();
    assert_eq!(
        board_archived.archived_at.as_deref(),
        Some("2026-08-20 10:00:01")
    );
}
