//! RED contract tests for board identity and comment lifecycle semantics.
//!
//! These are intentionally written against the public `Db` surface.  The
//! daemon/client layers must preserve the same rules when they expose the
//! corresponding typed protocol methods.

use board_core::db::{Db, BOARD_ID};
use board_core::protocol::{CardCreateParams, CardVisibility};

fn mem() -> Db {
    Db::open_in_memory().expect("in-memory board database")
}

fn card(db: &Db, title: &str) -> board_core::model::Card {
    db.create_card(&CardCreateParams {
        title: title.into(),
        ..Default::default()
    })
    .expect("card")
}

#[test]
fn board_rename_preserves_identity_scope_columns_and_cards() {
    let db = mem();
    let board = db.open_board("/repo/project").expect("scoped board");
    let card = db
        .create_card(&CardCreateParams {
            board_id: Some(board.id),
            title: "kept card".into(),
            ..Default::default()
        })
        .expect("card");

    let renamed = db.rename_board(board.id, "Project board").expect("rename");

    assert_eq!(renamed.id, board.id);
    assert_eq!(renamed.name, "Project board");
    assert_eq!(renamed.scope_path.as_deref(), Some("/repo/project"));
    assert_eq!(db.get_board(board.id).expect("board").name, "Project board");
    assert_eq!(
        db.open_board("/repo/project").expect("same scope").id,
        board.id
    );
    assert_eq!(db.list_columns(board.id).expect("columns").len(), 1);
    assert_eq!(
        db.get_card(card.id)
            .expect("card")
            .expect("present")
            .board_id,
        board.id
    );
}

/// Board names are unique case-insensitively within one project only: a
/// rename onto a sibling's name in the same project is refused, while the
/// same name in a different project is legal.
#[test]
fn board_rename_keeps_names_unique_within_a_project() {
    let db = mem();
    let one = db.open_board("/one").expect("board one");
    let two = db.open_board("/two").expect("board two");
    let sibling = db.create_board(one.project_id, "Backlog").expect("sibling");

    // Same project: renaming onto a sibling's name (case-insensitively) fails.
    assert!(db.rename_board(one.id, "BACKLOG").is_err());
    assert!(db.rename_board(one.id, "Backlog").is_err());
    assert_eq!(db.get_board(one.id).expect("board").name, one.name);
    // Different project: the same name is legal.
    assert!(db.rename_board(one.id, two.name.as_str()).is_ok());
    // Project-scoped rename keeps the board in its project.
    assert_eq!(db.get_board(sibling.id).expect("board").name, "Backlog");
}

#[test]
fn comments_support_get_update_soft_delete_and_history() {
    let db = mem();
    let card = card(&db, "comment lifecycle");
    let comment = db
        .add_comment(card.id, "user", "original body")
        .expect("comment");

    assert_eq!(
        db.get_comment(comment.id)
            .expect("get comment")
            .expect("comment exists")
            .body,
        "original body"
    );

    let edited = db
        .update_comment(comment.id, "edited body")
        .expect("edit comment");
    assert_eq!(edited.body, "edited body");
    assert_eq!(edited.author, "user");

    let history = db
        .list_comment_history(comment.id)
        .expect("comment history");
    assert_eq!(
        history
            .iter()
            .map(|entry| entry.body.as_str())
            .collect::<Vec<_>>(),
        vec!["original body", "edited body"]
    );

    let deleted = db
        .soft_delete_comment(comment.id)
        .expect("soft delete comment");
    assert!(deleted.deleted_at.is_some());
    assert!(db
        .get_comment(comment.id)
        .expect("get deleted comment")
        .expect("audit record remains")
        .deleted_at
        .is_some());
    assert!(db
        .list_comments(card.id)
        .expect("ordinary comments")
        .is_empty());

    let history_after_delete = db
        .list_comment_history(comment.id)
        .expect("history after delete");
    assert_eq!(
        history_after_delete
            .iter()
            .map(|entry| entry.body.as_str())
            .collect::<Vec<_>>(),
        vec!["original body", "edited body"]
    );
    assert!(history_after_delete
        .last()
        .expect("history row")
        .deleted_at
        .is_some());
}

#[test]
fn card_detail_hides_deleted_comments_but_shows_current_edits() {
    let db = mem();
    let card = card(&db, "card detail");
    let edited = db
        .add_comment(card.id, "user", "before edit")
        .expect("editable comment");
    let deleted = db
        .add_comment(card.id, "user", "remove from detail")
        .expect("deletable comment");

    db.update_comment(edited.id, "after edit").expect("edit");
    db.soft_delete_comment(deleted.id).expect("delete");

    let detail = db.get_card_detail(card.id).expect("card detail");
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].id, edited.id);
    assert_eq!(detail.comments[0].body, "after edit");
    assert!(!detail
        .comments
        .iter()
        .any(|comment| comment.id == deleted.id));

    let deleted_audit = db
        .list_comment_history(deleted.id)
        .expect("deleted comment audit");
    assert!(deleted_audit
        .iter()
        .any(|entry| entry.body == "remove from detail" && entry.deleted_at.is_some()));
}

#[test]
fn system_comments_are_immutable_at_the_store_boundary() {
    let db = mem();
    let card = card(&db, "system comment");
    let system = db
        .add_comment(card.id, "system", "run transitioned")
        .expect("system comment");

    assert!(db.update_comment(system.id, "rewritten").is_err());
    assert!(db.soft_delete_comment(system.id).is_err());
    let current = db
        .get_comment(system.id)
        .expect("get system comment")
        .expect("system comment remains");
    assert_eq!(current.author, "system");
    assert_eq!(current.body, "run transitioned");
}

#[test]
fn agent_comment_ownership_survives_edits_and_history() {
    let db = mem();
    let card = card(&db, "agent comment");
    let agent = db
        .add_comment(card.id, "agent:42", "agent output")
        .expect("agent comment");

    // The update API accepts a body, not a replacement author.  An edit must
    // remain owned by the run that created the comment in both projections.
    let edited = db
        .update_comment(agent.id, "revised agent output")
        .expect("edit");
    assert_eq!(edited.author, "agent:42");
    let history = db.list_comment_history(agent.id).expect("agent history");
    assert!(history.iter().all(|entry| entry.author == "agent:42"));
}

#[test]
fn active_all_and_archived_visibility_has_one_shared_contract() {
    let db = mem();
    let active = card(&db, "active");
    let archived = card(&db, "archived");
    db.set_card_archived(archived.id, true).expect("archive");

    assert_eq!(
        serde_json::to_string(&CardVisibility::Active).expect("visibility wire"),
        "\"active\""
    );
    assert_eq!(
        serde_json::to_string(&CardVisibility::All).expect("visibility wire"),
        "\"all\""
    );
    assert_eq!(
        serde_json::to_string(&CardVisibility::Archived).expect("visibility wire"),
        "\"archived\""
    );

    let snapshot_cards = db.list_cards(BOARD_ID).expect("board snapshot cards");
    assert_eq!(
        snapshot_cards
            .iter()
            .map(|card| (card.id, card.title.as_str()))
            .collect::<Vec<_>>(),
        vec![(active.id, "active"), (archived.id, "archived")]
    );

    let titles = |visibility| {
        db.list_cards_visible(BOARD_ID, visibility)
            .expect("visible cards")
            .into_iter()
            .map(|card| card.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(CardVisibility::Active), vec!["active"]);
    assert_eq!(titles(CardVisibility::All), vec!["active", "archived"]);
    assert_eq!(titles(CardVisibility::Archived), vec!["archived"]);
}
