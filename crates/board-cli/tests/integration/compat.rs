use super::runs::{run_done_retry_cancel_workflow, RunCliStyle};
use super::{json_output, old_card, TestDaemon};

#[test]
fn top_level_comment_and_move_aliases_remain_supported() {
    let td = TestDaemon::start(&[]);
    let card_id = old_card(&td, "legacy aliases");

    let comment =
        json_output(&td.board(&["comment", &card_id.to_string(), "legacy comment", "--json"]));
    assert_eq!(comment["card_id"], card_id);
    assert_eq!(comment["body"], "legacy comment");

    let moved = json_output(&td.board(&["move", &card_id.to_string(), "Todo", "--json"]));
    assert_eq!(moved["id"], card_id);
}

#[test]
fn top_level_done_cancel_and_retry_aliases_remain_supported() {
    // Comment/move alias coverage lives in
    // `top_level_comment_and_move_aliases_remain_supported` above; the
    // done/retry/cancel workflow itself is shared with the canonical
    // `card run` verbs in `runs.rs`.
    run_done_retry_cancel_workflow(RunCliStyle::Alias);
}
