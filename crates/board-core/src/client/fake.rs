use serde_json::Value;

use crate::db::{ColumnTarget, ColumnWiring, Db, FinalizeEffects, FinalizeRun, BOARD_ID};

use crate::engine;
use crate::labels::card_labels;

use crate::protocol::{
    BoardGetParams, BoardListResult, BoardOpenParams, BoardRenameParams, BoardSnapshot,
    CardArchiveParams, CardCreateParams, CardDetail, CardListParams, CardMoveParams,
    CardUpdateParams, ColumnCreateParams, ColumnDeleteParams, ColumnReorderParams,
    ColumnUpdateParams, CommentAddParams, CommentDeleteParams, CommentGetParams,
    CommentHistoryParams, CommentUpdateParams, DeletedResult, Event, PaneSetTitleParams,
    PaneSetTitleResult, RunActionResult, RunDoneParams, RunFocusParams, RunFocusResult,
    TemplateApplyParams, Trigger,
};

use super::BoardClient;

// Mirrors `crates/board-daemon/src/ops/boards.rs` (`template_apply`) and
// `crates/board-daemon/src/template.rs`: same prompts, same five columns, same
// transitions. Duplicated here (rather than shared) because the daemon logic
// lives in board-daemon, which board-core cannot depend on; the column
// creation/wiring itself is shared via `Db::apply_template_columns_uow` so
// that part cannot drift.
const PLAN_PROMPT: &str =
    "You are in the PLAN stage. Use /quick-planner style planning: produce a written
implementation plan and save it under docs/plans/ (or .plans/). Do not write code.
When finished you MUST run:
  board comment $BOARD_CARD_ID \"Plan ready at <filepath>. <3-line summary>\"
  board done $BOARD_CARD_ID --outcome ok";

const EXECUTE_PROMPT: &str =
    "You are in the EXECUTE stage. Implement the plan referenced in the card comments.
Run tests. When finished:
  board comment $BOARD_CARD_ID \"<what changed, files touched, test results>\"
  board done $BOARD_CARD_ID --outcome ok    # or --outcome fail with reasons";

const REVIEW_PROMPT: &str =
    "You are in the REVIEW stage. Review the diff against the card description and the
plan/execution comments. Be adversarial. Then:
  board comment $BOARD_CARD_ID \"<verdict + findings>\"
  board done $BOARD_CARD_ID --outcome ok    # ok = ship to human; fail = back to Execute";

/// In-memory board state machine for TUI tests. Backed by an in-memory
/// SQLite db, so CRUD/move/positions/comments behave exactly like the real
/// store — but there is no dispatch: moving into an auto column just moves.
pub struct FakeBoardClient {
    db: Db,
    /// Harness config, so the fake answers the same resume-capability question
    /// the daemon answers (`run.focus`). Defaults mean built-ins only.
    config: crate::config::Config,
}

/// Validate an agent actor exactly as the daemon does. The fake harness may
/// invoke the board before its lifecycle row exists, so retain that narrow
/// compatibility path only for fake cards with no open durable run.
fn require_agent_run(db: &Db, actor_run_id: i64, card_id: i64, author: &str) -> anyhow::Result<()> {
    let expected_author = format!("agent:{actor_run_id}");
    if author != expected_author {
        anyhow::bail!("agent run {actor_run_id} may only act as {expected_author}");
    }

    match db.get_run(actor_run_id) {
        Ok(run) => {
            if run.card_id != card_id {
                anyhow::bail!("agent run {actor_run_id} does not belong to comment card {card_id}");
            }
            if run.ended_at.is_some() {
                anyhow::bail!("agent run {actor_run_id} is no longer open");
            }
            Ok(())
        }
        Err(crate::Error::NotFound(_)) => {
            let card = db
                .get_card(card_id)?
                .ok_or_else(|| anyhow::anyhow!("card {card_id} not found"))?;
            if card.harness == "fake" && db.open_run_for_card(card_id)?.is_none() {
                Ok(())
            } else {
                anyhow::bail!("run {actor_run_id} not found");
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn comment_for_mutation(
    db: &Db,
    id: i64,
    actor_run_id: Option<i64>,
) -> anyhow::Result<crate::model::CommentRecord> {
    let comment = db
        .get_comment(id)?
        .ok_or_else(|| anyhow::anyhow!("comment {id} not found"))?;
    if let Some(run_id) = actor_run_id {
        require_agent_run(db, run_id, comment.card_id, &comment.author)?;
    }
    Ok(comment)
}

impl FakeBoardClient {
    pub fn new() -> anyhow::Result<FakeBoardClient> {
        Ok(FakeBoardClient {
            db: Db::open_in_memory()?,
            config: crate::config::Config::default(),
        })
    }

    /// Declare config-defined harnesses (`[harness.NAME]`) so tests can exercise
    /// the resume opt-in through the fake.
    pub fn with_config(mut self, config: crate::config::Config) -> FakeBoardClient {
        self.config = config;
        self
    }

    /// Direct access to the underlying store (tests may seed runs/comments).
    pub fn db(&self) -> &Db {
        &self.db
    }
}

/// Declare the fake's method table exactly once.
///
/// The macro emits both the dispatch `match` and [`FAKE_CLIENT_METHODS`], so a
/// method this fake answers cannot be missing from the exported list, and a
/// listed method cannot be missing an implementation. The bindings the arms
/// read (`db`, `config`, `params`) are named at the invocation below so they
/// keep ordinary call-site scoping.
macro_rules! fake_methods {
    ($db:ident, $config:ident, $params:ident, { $($method:literal => $arm:expr),* $(,)? }) => {
        /// Every board method [`FakeBoardClient`] implements.
        ///
        /// The whole board-tui test tier runs against this fake, so its surface
        /// is compared against the daemon's routed surface by the parity guard
        /// in `board-daemon` — see `board_daemon::ROUTED_METHODS`.
        pub const FAKE_CLIENT_METHODS: &[&str] = &[$($method),*];

        impl BoardClient for FakeBoardClient {
            fn call(&mut self, method: &str, $params: Value) -> anyhow::Result<Value> {
                let $config = self.config.clone();
                let $db = &self.db;
                let v = match method {
                    $($method => $arm,)*
                    other => anyhow::bail!("FakeBoardClient: unsupported method {other}"),
                };
                Ok(v)
            }

            fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
                Ok(Box::new(std::iter::empty()))
            }
        }
    };
}

/// Stamp the daemon-owned display labels onto a card. The DB-only fake has no
/// herdr to resolve an unset session through, so `None` yields the
/// `default session` marker — exactly the daemon's fallback when herdr is
/// unreachable.
fn stamp(mut card: crate::model::Card) -> crate::model::Card {
    card.labels = card_labels(&card, None);
    card
}

fn stamp_all(cards: Vec<crate::model::Card>) -> Vec<crate::model::Card> {
    cards.into_iter().map(stamp).collect()
}

fake_methods!(db, config, params, {
    "board.get" => {
        let p: BoardGetParams = serde_json::from_value(params)?;
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        let snap = BoardSnapshot {
            board: db.get_board(board_id)?,
            columns: db.list_columns(board_id)?,
            cards: stamp_all(db.list_cards(board_id)?),
            active_runs: db.active_run_summaries(board_id)?,
        };
        serde_json::to_value(snap)?
    },
    "board.open" => {
        let p: BoardOpenParams = serde_json::from_value(params)?;
        let board = db.open_board(&p.scope_path)?;
        serde_json::to_value(BoardSnapshot {
            columns: db.list_columns(board.id)?,
            cards: stamp_all(db.list_cards(board.id)?),
            active_runs: db.active_run_summaries(board.id)?,
            board,
        })?
    },
    "board.list" => serde_json::to_value(BoardListResult {
        boards: db.list_boards()?,
    })?,
    "board.rename" => {
        let p: BoardRenameParams = serde_json::from_value(params)?;
        serde_json::to_value(db.rename_board(p.board_id, &p.name)?)?
    },
    "column.create" => {
        let p: ColumnCreateParams = serde_json::from_value(params)?;
        serde_json::to_value(db.create_column(&p)?)?
    },
    "column.update" => {
        let p: ColumnUpdateParams = serde_json::from_value(params)?;
        serde_json::to_value(db.update_column(&p)?)?
    },
    "column.reorder" => {
        let p: ColumnReorderParams = serde_json::from_value(params)?;
        serde_json::to_value(db.reorder_column(p.id, p.position)?)?
    },
    "column.delete" => {
        let p: ColumnDeleteParams = serde_json::from_value(params)?;
        let cards = db.list_cards_in_column(p.id)?;
        let has_open_run = db.column_has_open_run(p.id)?;
        engine::validate_column_delete(!cards.is_empty(), has_open_run, p.move_cards_to)?;
        db.delete_column(p.id, p.move_cards_to)?;
        serde_json::to_value(DeletedResult { deleted: true })?
    },
    "card.create" => {
        let p: CardCreateParams = serde_json::from_value(params)?;
        serde_json::to_value(stamp(db.create_card(&p)?))?
    },
    "card.duplicate" => {
        let id = params["id"].as_i64().unwrap_or_default();
        serde_json::to_value(stamp(db.duplicate_card(id)?))?
    },
    "card.update" => {
        let p: CardUpdateParams = serde_json::from_value(params)?;
        serde_json::to_value(stamp(db.update_card(&p)?))?
    },
    "card.delete" => {
        let id = params["id"].as_i64().unwrap_or_default();
        db.delete_card(id)?;
        serde_json::to_value(DeletedResult { deleted: true })?
    },
    "card.archive" => {
        let p: CardArchiveParams = serde_json::from_value(params)?;
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| anyhow::anyhow!("card {} not found", p.id))?;
        engine::validate_card_archive(card.status)?;
        serde_json::to_value(stamp(db.set_card_archived(p.id, p.archived)?))?
    },
    "card.move" => {
        let p: CardMoveParams = serde_json::from_value(params)?;
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| anyhow::anyhow!("card {} not found", p.id))?;
        if card.archived_at.is_some() {
            anyhow::bail!("archived card must be restored before moving");
        }
        let card = match p.board_id {
            // Cross-board transfer only when an explicit destination
            // board is named and differs from the current one.
            Some(bid) if bid != card.board_id => {
                db.transfer_card(p.id, bid, p.column_id, p.position)?
            }
            _ => db.move_card(p.id, p.column_id, p.position)?,
        };
        serde_json::to_value(stamp(card))?
    },
    "card.get" => {
        let id = params["id"].as_i64().unwrap_or_default();
        let card = db
            .get_card(id)?
            .ok_or_else(|| anyhow::anyhow!("card {id} not found"))?;
        let detail = CardDetail {
            card: stamp(card),
            comments: db.list_comments(id)?,
            runs: db.list_runs(id)?,
        };
        serde_json::to_value(detail)?
    },
    "card.list" => {
        let p: CardListParams = serde_json::from_value(params)?;
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        let visibility = p
            .visibility
            .unwrap_or(crate::protocol::CardVisibility::Active);
        let cards = match p.column_id {
            Some(c) => {
                let column = db
                    .get_column(c)?
                    .ok_or_else(|| anyhow::anyhow!("column {c} not found"))?;
                if column.board_id != board_id {
                    anyhow::bail!("column {c} belongs to another board");
                }
                db.list_cards_in_column_visible(c, visibility)?
            }
            None => db.list_cards_visible(board_id, visibility)?,
        };
        serde_json::to_value(stamp_all(cards))?
    },
    "run.done" => {
        let p: RunDoneParams = serde_json::from_value(params)?;
        let run = db
            .active_run_for_card(p.card_id)?
            .ok_or_else(|| anyhow::anyhow!("no active run for card {}", p.card_id))?;
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| anyhow::anyhow!("card {} not found", p.card_id))?;
        let column = db
            .get_column(run.column_id)?
            .ok_or_else(|| anyhow::anyhow!("column {} not found", run.column_id))?;
        let columns = db.list_columns(card.board_id)?;
        let decision = engine::decide_transition(&column, &columns, p.outcome, None);

        let FinalizeEffects {
            card,
            finished_run: run,
            next_run: _,
        } = db.finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: p.outcome,
            summary: p.summary.as_deref(),
            comments: &[("system", &decision.system_comment)],
            target_column_id: decision.target_column_id,
            final_status: decision.new_status,
            final_awaiting_reason: None,
            next: None,
        })?;
        serde_json::to_value(RunActionResult { run, card })?
    },
    "run.focus" => {
        let p: RunFocusParams = serde_json::from_value(params)?;
        // Ownership-validating lookup: a foreign run id is rejected
        // here exactly as the daemon rejects it.
        let run = db.run_for_card(p.card_id, p.run_id)?;
        // This fake is DB-only: it has no Herdr, so it cannot know
        // whether the recorded pane is still alive and must not pretend
        // to create one. What it *can* model honestly is the rescue
        // **decision**, which is entirely a function of the run row
        // plus config: an unsupported harness and a missing conversation
        // id are refused exactly as the daemon refuses them, and a
        // rescue-eligible run is reported as `Rescued` with no pane id
        // of its own — see `pane_id` below.
        let recorded_pane_id = run.herdr_pane_id.clone();
        let action = match &recorded_pane_id {
            Some(_) => crate::protocol::RunFocusAction::FocusedRecordedPane,
            None => {
                // Every precondition the daemon checks, in the same
                // order, so a fake-backed test cannot pass on input the
                // real daemon refuses.
                if run
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .is_none()
                {
                    anyhow::bail!(
                        "run {} of card {} recorded no harness conversation id, so \
                         there is nothing to resume",
                        run.id,
                        run.card_id
                    );
                }
                let support = crate::capability::resume_support_for(&run.harness, &config);
                if !support.is_supported() {
                    anyhow::bail!(
                        "run {} of card {} uses harness '{}', which does not support \
                         resuming a recorded conversation, so its closed pane cannot \
                         be reopened",
                        run.id,
                        run.card_id,
                        run.harness
                    );
                }
                // Pre-v11 rows persist no durable execution to resume.
                let Some(spec) = run.launch_spec.as_ref() else {
                    anyhow::bail!(
                        "run {} of card {} predates durable launch specs, so there is \
                         no recorded execution to resume",
                        run.id,
                        run.card_id
                    );
                };
                // The same refusals `resume_invocation` raises (legacy
                // all-in-one argv, in particular) must not be invisible
                // to fake-backed callers either.
                crate::harness::resume_invocation(
                    &run.harness,
                    support,
                    spec.execution(),
                    run.session_id.as_deref().unwrap_or_default().trim(),
                )?;
                if run.herdr_workspace_id.is_none() {
                    anyhow::bail!(
                        "run {} of card {} recorded no Herdr workspace, so a reopened \
                         pane would have nowhere to go",
                        run.id,
                        run.card_id
                    );
                }
                crate::protocol::RunFocusAction::Rescued
            }
        };
        serde_json::to_value(RunFocusResult {
            action,
            recorded_pane_id: recorded_pane_id.clone(),
            run_id: run.id,
            card_id: run.card_id,
            column_id: run.column_id,
            harness: run.harness,
            session: run.session,
            session_id: run.session_id,
            // A DB-only fake cannot mint a real Herdr pane. For a
            // would-be rescue it reports the sentinel below instead of
            // inventing an id that no pane answers to.
            pane_id: recorded_pane_id.unwrap_or_else(|| "(would-rescue)".to_string()),
        })?
    },
    "comment.add" => {
        let p: CommentAddParams = serde_json::from_value(params)?;
        let author = match p.actor_run_id {
            Some(run_id) => {
                let expected = format!("agent:{run_id}");
                if let Some(author) = p.author.as_deref() {
                    require_agent_run(db, run_id, p.card_id, author)?;
                } else {
                    require_agent_run(db, run_id, p.card_id, &expected)?;
                }
                expected
            }
            None => p.author.unwrap_or_else(|| "user".into()),
        };
        serde_json::to_value(db.add_comment(p.card_id, &author, &p.body)?)?
    },
    "comment.get" => {
        let p: CommentGetParams = serde_json::from_value(params)?;
        serde_json::to_value(
            db.get_comment(p.id)?
                .ok_or_else(|| anyhow::anyhow!("comment {} not found", p.id))?,
        )?
    },
    "comment.update" => {
        let p: CommentUpdateParams = serde_json::from_value(params)?;
        comment_for_mutation(db, p.id, p.actor_run_id)?;
        serde_json::to_value(db.update_comment(p.id, &p.body)?)?
    },
    "comment.delete" => {
        let p: CommentDeleteParams = serde_json::from_value(params)?;
        comment_for_mutation(db, p.id, p.actor_run_id)?;
        db.soft_delete_comment(p.id)?;
        serde_json::to_value(DeletedResult { deleted: true })?
    },
    "comment.history" => {
        let p: CommentHistoryParams = serde_json::from_value(params)?;
        serde_json::to_value(db.list_comment_history(p.id)?)?
    },
    "template.apply" => {
        let p: TemplateApplyParams = serde_json::from_value(params)?;
        if p.name != "pipeline" {
            return Err(
                crate::Error::BadRequest(format!("unknown template: {}", p.name)).into(),
            );
        }
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        db.get_board(board_id)?;
        let existing = db.list_columns(board_id)?;
        let cards = db.list_cards(board_id)?;
        if existing.len() != 1 || existing[0].name != "Todo" || !cards.is_empty() {
            return Err(crate::Error::InvalidState(
                "template.apply requires an empty board (only the seed Todo column, no cards)"
                    .into(),
            )
            .into());
        }
        let todo = existing[0].id;
        let specs = vec![
            ColumnCreateParams {
                name: "Plan".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(PLAN_PROMPT.into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Execute".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(EXECUTE_PROMPT.into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Review".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Auto),
                system_prompt: Some(REVIEW_PROMPT.into()),
                model_override: Some("opus".into()),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Human Review".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            },
            ColumnCreateParams {
                name: "Done".into(),
                board_id: Some(board_id),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            },
        ];
        let wiring = [
            ColumnWiring {
                column_index: 0,
                on_success: Some(ColumnTarget::Created(1)),
                on_fail: Some(ColumnTarget::Existing(todo)),
            },
            ColumnWiring {
                column_index: 1,
                on_success: Some(ColumnTarget::Created(2)),
                on_fail: None,
            },
            ColumnWiring {
                column_index: 2,
                on_success: Some(ColumnTarget::Created(3)),
                on_fail: Some(ColumnTarget::Created(1)),
            },
        ];
        serde_json::to_value(db.apply_template_columns_uow(board_id, &specs, &wiring)?)?
    },
    "pane.set_title" => {
        // This fake has no Herdr, so it renames nothing and answers with the
        // same bare acknowledgement the daemon returns on success. Decoding
        // the params first keeps a malformed request an error here too.
        let _: PaneSetTitleParams = serde_json::from_value(params)?;
        serde_json::to_value(PaneSetTitleResult { renamed: true })?
    },
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Column;
    use serde_json::json;

    fn columns(client: &mut FakeBoardClient) -> Vec<Column> {
        serde_json::from_value(
            client
                .call("template.apply", json!({"name": "pipeline"}))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn template_apply_creates_expected_pipeline() {
        let mut client = FakeBoardClient::new().unwrap();
        let cols = columns(&mut client);

        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Todo", "Plan", "Execute", "Review", "Human Review", "Done"]
        );

        let by_name = |name: &str| cols.iter().find(|c| c.name == name).unwrap();
        assert_eq!(by_name("Plan").trigger, Trigger::Auto);
        assert_eq!(by_name("Execute").trigger, Trigger::Auto);
        assert_eq!(by_name("Review").trigger, Trigger::Auto);
        assert_eq!(by_name("Human Review").trigger, Trigger::Manual);
        assert_eq!(by_name("Done").trigger, Trigger::Manual);

        // Transitions: Plan ok->Execute, fail->Todo; Execute ok->Review;
        // Review ok->Human Review, fail->Execute.
        let todo = by_name("Todo").id;
        let execute = by_name("Execute").id;
        let review = by_name("Review").id;
        let human = by_name("Human Review").id;

        assert_eq!(by_name("Plan").on_success_column_id, Some(execute));
        assert_eq!(by_name("Plan").on_fail_column_id, Some(todo));
        assert_eq!(by_name("Execute").on_success_column_id, Some(review));
        assert_eq!(by_name("Execute").on_fail_column_id, None);
        assert_eq!(by_name("Review").on_success_column_id, Some(human));
        assert_eq!(by_name("Review").on_fail_column_id, Some(execute));
    }

    #[test]
    fn template_apply_rejects_board_with_a_card() {
        let mut client = FakeBoardClient::new().unwrap();
        let todo_id = client.db().list_columns(BOARD_ID).unwrap()[0].id;
        client
            .call(
                "card.create",
                json!({"title": "existing card", "column_id": todo_id}),
            )
            .unwrap();

        let err = client
            .call("template.apply", json!({"name": "pipeline"}))
            .unwrap_err();
        assert!(err.to_string().contains(
            "template.apply requires an empty board (only the seed Todo column, no cards)"
        ));
    }

    #[test]
    fn template_apply_rejects_unknown_template_name() {
        let mut client = FakeBoardClient::new().unwrap();
        let err = client
            .call("template.apply", json!({"name": "not-a-real-template"}))
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("unknown template: not-a-real-template"));
    }
}
