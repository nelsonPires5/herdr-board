use super::*;
use board_core::model::CommentRecord;

/// Check an explicitly supplied agent identity before it can mutate a
/// comment.  The actor is intentionally optional for compatibility with the
/// original `comment.add` API, which identified agent comments only through
/// their `author` string.
fn require_agent_run(
    db: &board_core::db::Db,
    actor_run_id: i64,
    card_id: i64,
    author: &str,
) -> Result<()> {
    let expected_author = format!("agent:{actor_run_id}");
    if author != expected_author {
        return Err(Error::InvalidState(format!(
            "agent run {actor_run_id} may only act as {expected_author}"
        )));
    }

    let run = match db.get_run(actor_run_id) {
        Ok(run) => run,
        Err(Error::NotFound(_)) => {
            // The provider-free fake harness historically gets an injected
            // BOARD_RUN_ID without first persisting a lifecycle row. Keep
            // that compatibility path, but do not weaken identity checks for
            // normal harnesses or for a durable run that belongs elsewhere.
            let card = db
                .get_card(card_id)?
                .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
            if card.harness == "fake" && db.open_run_for_card(card_id)?.is_none() {
                return Ok(());
            }
            return Err(Error::NotFound(format!("run {actor_run_id}")));
        }
        Err(error) => return Err(error),
    };
    if run.card_id != card_id {
        return Err(Error::InvalidState(format!(
            "agent run {actor_run_id} does not belong to comment card {card_id}"
        )));
    }
    if run.ended_at.is_some() {
        return Err(Error::InvalidState(format!(
            "agent run {actor_run_id} is no longer open"
        )));
    }
    Ok(())
}

fn comment_for_mutation(
    db: &board_core::db::Db,
    id: i64,
    actor_run_id: Option<i64>,
) -> Result<CommentRecord> {
    let comment = db
        .get_comment(id)?
        .ok_or_else(|| Error::NotFound(format!("comment {id}")))?;
    if let Some(run_id) = actor_run_id {
        require_agent_run(db, run_id, comment.card_id, &comment.author)?;
    }
    Ok(comment)
}

pub(super) fn comment_add(d: &Arc<Daemon>, p: CommentAddParams) -> Result<Value> {
    let (comment, card) = {
        let db = d.store.lock();
        let card = db
            .get_card(p.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.card_id)))?;
        let author = match p.actor_run_id {
            Some(run_id) => {
                let expected = format!("agent:{run_id}");
                // An explicitly supplied author is checked, while omitting it
                // gets the identity implied by the actor run. This keeps the
                // old author-only comment.add call compatible with fake
                // agents, but prevents a new actor-aware caller from forging
                // another run's ownership.
                if let Some(author) = p.author.as_deref() {
                    require_agent_run(&db, run_id, p.card_id, author)?;
                } else {
                    require_agent_run(&db, run_id, p.card_id, &expected)?;
                }
                expected
            }
            None => p.author.unwrap_or_else(|| "user".into()),
        };
        let comment = db.add_comment(card.id, &author, &p.body)?;
        (comment, card)
    };
    d.emit_changed_board(
        BoardChangedReason::CommentAdded,
        card.board_id,
        Some(card.id),
        None,
    );
    Ok(json!(comment))
}

pub(super) fn comment_get(d: &Arc<Daemon>, p: CommentGetParams) -> Result<Value> {
    let comment = d
        .store
        .lock()
        .get_comment(p.id)?
        .ok_or_else(|| Error::NotFound(format!("comment {}", p.id)))?;
    Ok(json!(comment))
}

pub(super) fn comment_update(d: &Arc<Daemon>, p: CommentUpdateParams) -> Result<Value> {
    let (updated, card) = {
        let db = d.store.lock();
        let comment = comment_for_mutation(&db, p.id, p.actor_run_id)?;
        let card = db
            .get_card(comment.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", comment.card_id)))?;
        let updated = db.update_comment(comment.id, &p.body)?;
        (updated, card)
    };
    d.emit_changed_board(
        BoardChangedReason::CommentAdded,
        card.board_id,
        Some(card.id),
        None,
    );
    Ok(json!(updated))
}

pub(super) fn comment_delete(d: &Arc<Daemon>, p: CommentDeleteParams) -> Result<Value> {
    let card = {
        let db = d.store.lock();
        let comment = comment_for_mutation(&db, p.id, p.actor_run_id)?;
        let card = db
            .get_card(comment.card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", comment.card_id)))?;
        db.soft_delete_comment(comment.id)?;
        card
    };
    d.emit_changed_board(
        BoardChangedReason::CommentAdded,
        card.board_id,
        Some(card.id),
        None,
    );
    Ok(json!(DeletedResult { deleted: true }))
}

pub(super) fn comment_history(d: &Arc<Daemon>, p: CommentHistoryParams) -> Result<Value> {
    let db = d.store.lock();
    // list_comment_history intentionally returns an empty list for an unknown
    // id at the DB layer; the RPC resource endpoint should distinguish that
    // from a real comment with no snapshots.
    db.get_comment(p.id)?
        .ok_or_else(|| Error::NotFound(format!("comment {}", p.id)))?;
    Ok(json!(db.list_comment_history(p.id)?))
}
