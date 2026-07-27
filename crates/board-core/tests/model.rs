//! Row-struct helpers that classify stored rows.

use board_core::model::Comment;

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
