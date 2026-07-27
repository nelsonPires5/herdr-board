use super::*;
use crate::dispatch::prepare_enqueue_values;
use board_core::db::{Db, BOARD_ID};
use board_core::engine::{
    decide_entry, merge_card_update, validate_card_archive, validate_card_edit,
    validate_card_settings, validate_card_values, validate_effective_settings,
};
use board_core::harness::DEFAULT_HARNESS;
use board_core::model::Card;

fn pending_create_card(db: &Db, p: &CardCreateParams) -> Result<Card> {
    let board_id = p.board_id.unwrap_or(BOARD_ID);
    let column_id = p.column_id.unwrap_or(db.default_column_id(board_id)?);
    let column = db.require_column(column_id)?;
    if column.board_id != board_id {
        return Err(Error::InvalidState(format!(
            "column {column_id} belongs to board {}, expected {board_id}",
            column.board_id
        )));
    }
    Ok(Card {
        id: 0,
        board_id,
        column_id,
        position: 0,
        title: p.title.clone(),
        description: p.description.clone().unwrap_or_default(),
        harness: p
            .harness
            .clone()
            .unwrap_or_else(|| DEFAULT_HARNESS.to_string()),
        model: p.model.clone(),
        effort: p.effort,
        permission_mode: p.permission_mode.clone(),
        session: p.session.clone(),
        space_kind: p.space_kind.unwrap_or(SpaceKind::Workspace),
        space_ref: p.space_ref.clone(),
        space_cwd: p.space_cwd.clone(),
        status: CardStatus::Idle,
        awaiting_reason: None,
        session_id: None,
        created_at: String::new(),
        updated_at: String::new(),
        archived_at: None,
    })
}

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

    let (card, enqueue) = {
        // Scheduler state and card creation/enqueue share one critical
        // section. The DB UoW below contains no Herdr or process I/O.
        let mut _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let pending = pending_create_card(&db, &p)?;
        let column = db.require_column(pending.column_id)?;
        let entry = decide_entry(&column, pending.status, false);
        if entry.enqueue {
            let prepared = prepare_enqueue_values(d, &db, &pending, pending.column_id, false)?;
            let (card, _run) = db.create_card_and_enqueue_uow(&p, &prepared.borrowed())?;
            _sched.chain_hops.remove(&card.id);
            (card, true)
        } else {
            (db.create_card(&p)?, false)
        }
    };

    d.emit_changed(
        BoardChangedReason::CardCreated,
        Some(card.id),
        Some(card.column_id),
    );
    if enqueue {
        d.wake_dispatch();
    }
    Ok(json!(card))
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
        let card = db.require_card(p.id)?;
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
        db.require_card(p.id)?;
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
        let card = db.require_card(p.id)?;
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
    let (card, target, source_board_id, source_column_id, enqueue) = {
        let mut _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let current = db.require_card(p.id)?;
        if current.archived_at.is_some() {
            return Err(Error::InvalidState(
                "archived card must be restored before moving".into(),
            ));
        }
        let target = db.require_column(p.column_id)?;
        let cross = p.board_id.is_some_and(|bid| bid != current.board_id);
        if cross {
            // The destination board must actually exist; the declared board
            // must match the target column's board.
            let declared = p.board_id.ok_or_else(|| {
                Error::InvalidState("cross-board move has no destination board".into())
            })?;
            db.get_board(declared)?;
            if target.board_id != declared {
                return Err(Error::InvalidState(format!(
                    "column {} belongs to board {}, declared destination board is {}",
                    p.column_id, target.board_id, declared
                )));
            }
            // Blocking sanity check, scoped to the cross-board transfer:
            // validate the merged effective harness/model/effort/permission
            // for the target column (reused from enqueue), confirm the card's
            // herdr session resolves, and — only when the destination is an
            // auto column that would run — confirm the card's workspace is
            // resolvable (read-only preflight). An incompatible setting or an
            // unresolvable session/workspace aborts the move; nothing is
            // written.
            validate_effective_settings(&current, &target, &d.config)?;
            if let Some(reg) = &d.session_registry {
                let socket = match reg.resolve(current.session.as_deref()) {
                    Ok(r) => r.socket,
                    Err(e) => {
                        return Err(Error::InvalidState(format!(
                            "cannot move: session does not resolve: {e:#}"
                        )));
                    }
                };
                if decide_entry(&target, current.status, false).enqueue {
                    if let Err(e) = (|| -> anyhow::Result<()> {
                        let mut client = board_herdr::HerdrClient::connect(&socket)
                            .map_err(|e| anyhow::anyhow!("herdr unavailable: {e}"))?;
                        crate::dispatch::validate_space_resolvable(
                            &mut client,
                            current.space_kind,
                            current.space_ref.as_deref(),
                            current.space_cwd.as_deref(),
                        )
                    })() {
                        return Err(Error::InvalidState(format!(
                            "cannot move: workspace does not resolve: {e:#}"
                        )));
                    }
                }
            }
        }

        let entry = decide_entry(&target, current.status, false);
        let card = if entry.enqueue {
            let prepared = prepare_enqueue_values(d, &db, &current, p.column_id, false)?;
            let (card, _run) = if cross {
                db.transfer_card_and_enqueue_uow(
                    p.id,
                    p.board_id.ok_or_else(|| {
                        Error::InvalidState("cross-board move has no destination board".into())
                    })?,
                    p.column_id,
                    p.position,
                    &prepared.borrowed(),
                )?
            } else {
                db.move_card_and_enqueue_uow(p.id, p.column_id, p.position, &prepared.borrowed())?
            };
            // This scheduler-only mutation follows the DB commit and is not
            // observable when the durable move/enqueue UoW fails.
            _sched.chain_hops.remove(&card.id);
            card
        } else if cross {
            db.transfer_card(
                p.id,
                p.board_id.ok_or_else(|| {
                    Error::InvalidState("cross-board move has no destination board".into())
                })?,
                p.column_id,
                p.position,
            )?
        } else {
            db.move_card(p.id, p.column_id, p.position)?
        };
        (
            card,
            target,
            current.board_id,
            current.column_id,
            entry.enqueue,
        )
    };

    // One precise CardMoved per affected board (the event now carries
    // board_id), replacing the old double coarse emit. Events are published
    // only after both the move and any initial enqueue have committed.
    if source_board_id != target.board_id {
        d.emit_changed_board(
            BoardChangedReason::CardMoved,
            source_board_id,
            Some(card.id),
            Some(source_column_id),
        );
    }
    d.emit_changed_board(
        BoardChangedReason::CardMoved,
        target.board_id,
        Some(card.id),
        Some(p.column_id),
    );
    if enqueue {
        d.wake_dispatch();
    }
    Ok(json!(card))
}

pub(super) fn card_get(d: &Arc<Daemon>, p: CardIdParams) -> Result<Value> {
    let db = d.store.lock();
    let card = db.require_card(p.id)?;
    Ok(json!(CardDetail {
        comments: db.list_comments(p.id)?,
        runs: db.list_runs(p.id)?,
        card,
    }))
}

pub(super) fn card_list(d: &Arc<Daemon>, p: CardListParams) -> Result<Value> {
    let db = d.store.lock();
    let board_id = p.board_id.unwrap_or(BOARD_ID);
    let visibility = p.visibility.unwrap_or(CardVisibility::Active);
    let cards = match p.column_id {
        Some(c) => {
            let column = db.require_column(c)?;
            if column.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "column {c} belongs to board {}, expected {board_id}",
                    column.board_id
                )));
            }
            db.list_cards_in_column_visible(c, visibility)?
        }
        None => db.list_cards_visible(board_id, visibility)?,
    };
    Ok(json!(cards))
}

// -- comments / runs --------------------------------------------------------
