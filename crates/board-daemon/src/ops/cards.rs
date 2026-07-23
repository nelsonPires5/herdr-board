use super::*;
use crate::dispatch::enqueue_run;
use board_core::db::BOARD_ID;
use board_core::engine::{
    decide_entry, merge_card_update, validate_card_archive, validate_card_edit,
    validate_card_settings, validate_card_values,
};
use board_core::harness::DEFAULT_HARNESS;
pub(super) fn card_create(d: &Arc<Daemon>, p: CardCreateParams) -> Result<Value> {
    validate_card_values(
        p.harness.as_deref().unwrap_or(DEFAULT_HARNESS),
        p.model.as_deref(),
        p.effort,
        p.permission_mode.as_deref(),
        p.space_kind.unwrap_or(SpaceKind::Workspace),
        p.space_ref.as_deref(),
        p.space_cwd.as_deref(),
        &d.config,
    )?;
    let card = d.store.lock().create_card(&p)?;
    let column = require_column(d, card.column_id)?;
    d.emit_changed(
        BoardChangedReason::CardCreated,
        Some(card.id),
        Some(card.column_id),
    );

    // Creating directly into an auto column dispatches immediately.
    if column.trigger == Trigger::Auto {
        let entry = decide_entry(&column, card.status, false);
        if entry.enqueue {
            d.sched.lock().unwrap().chain_hops.remove(&card.id);
            enqueue_run(d, card.id, card.column_id, false)?;
            d.wake_dispatch();
        }
    }
    Ok(json!(require_card(d, card.id)?))
}

pub(super) fn card_update(d: &Arc<Daemon>, p: CardUpdateParams) -> Result<Value> {
    let edits_locked = p.harness.is_some()
        || !p.model.is_unchanged()
        || !p.effort.is_unchanged()
        || !p.permission_mode.is_unchanged()
        || !p.session.is_unchanged()
        || p.space_kind.is_some()
        || !p.space_ref.is_unchanged()
        || !p.space_cwd.is_unchanged();
    let card = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        // The scheduler→store critical section serializes this validation and
        // update with an entire finalization transaction.
        validate_card_edit(card.status, edits_locked)?;
        if edits_locked && db.open_run_for_card(p.id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; cannot edit harness/space fields".into(),
            ));
        }
        let merged = merge_card_update(&card, &p);
        validate_card_settings(&merged, &d.config)?;
        db.update_card(&p)?
    };
    d.emit_changed(BoardChangedReason::CardUpdated, Some(card.id), None);
    Ok(json!(card))
}

pub(super) fn card_delete(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        db.get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        if db.open_run_for_card(p.id)?.is_some() {
            return Err(Error::InvalidState(
                "card has an open run; cancel it first".into(),
            ));
        }
        db.delete_card(p.id)?;
    }
    d.emit_changed(BoardChangedReason::CardDeleted, Some(p.id), None);
    Ok(json!(DeletedResult { deleted: true }))
}

pub(super) fn card_archive(d: &Arc<Daemon>, p: CardArchiveParams) -> Result<Value> {
    let card = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let card = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        if p.archived {
            validate_card_archive(card.status)?;
            if db.open_run_for_card(p.id)?.is_some() {
                return Err(Error::InvalidState(
                    "card has an open run; cancel it before archiving".into(),
                ));
            }
        }
        db.set_card_archived(p.id, p.archived)?
    };
    d.emit_changed(BoardChangedReason::CardArchived, Some(p.id), None);
    Ok(json!(card))
}

pub(super) fn card_move(d: &Arc<Daemon>, p: CardMoveParams) -> Result<Value> {
    let (card, target) = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let current = db
            .get_card(p.id)?
            .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
        if current.archived_at.is_some() {
            return Err(Error::InvalidState(
                "archived card must be restored before moving".into(),
            ));
        }
        let target = db
            .get_column(p.column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", p.column_id)))?;
        let card = db.move_card(p.id, p.column_id, p.position)?;
        (card, target)
    };
    d.emit_changed(
        BoardChangedReason::CardMoved,
        Some(card.id),
        Some(p.column_id),
    );

    let entry = decide_entry(&target, card.status, false);
    if entry.enqueue {
        // Human move resets the auto-chain counter.
        d.sched.lock().unwrap().chain_hops.remove(&card.id);
        enqueue_run(d, card.id, p.column_id, false)?;
        d.wake_dispatch();
    }
    Ok(json!(require_card(d, card.id)?))
}

pub(super) fn card_get(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    let db = d.store.lock();
    let card = db
        .get_card(p.id)?
        .ok_or_else(|| Error::NotFound(format!("card {}", p.id)))?;
    Ok(json!(CardDetail {
        comments: db.list_comments(p.id)?,
        runs: db.list_runs(p.id)?,
        card,
    }))
}

pub(super) fn card_list(d: &Arc<Daemon>, p: CardListParams) -> Result<Value> {
    let db = d.store.lock();
    let board_id = p.board_id.unwrap_or(BOARD_ID);
    let cards = match p.column_id {
        Some(c) => {
            let column = db
                .get_column(c)?
                .ok_or_else(|| Error::NotFound(format!("column {c}")))?;
            if column.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "column {c} belongs to board {}, expected {board_id}",
                    column.board_id
                )));
            }
            db.list_cards_in_column(c)?
        }
        None => db.list_cards(board_id)?,
    };
    Ok(json!(cards))
}

// -- comments / runs --------------------------------------------------------
