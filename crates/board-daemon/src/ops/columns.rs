use super::*;
use board_core::engine::{
    merge_column_update, validate_column_delete, validate_column_update, validate_column_values,
    PermissionContext,
};
pub(super) fn column_create(d: &Arc<Daemon>, p: ColumnCreateParams) -> Result<Value> {
    validate_column_values(
        p.harness_override.as_deref(),
        p.model_override.as_deref(),
        p.effort_override.as_deref(),
        p.permission_override.as_deref(),
        &d.config,
        PermissionContext::ColumnOverride,
    )?;
    let col = d.store.lock().create_column(&p)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, Some(col.id));
    Ok(json!(col))
}

pub(super) fn column_update(d: &Arc<Daemon>, p: ColumnUpdateParams) -> Result<Value> {
    let col = {
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let current = db
            .get_column(p.id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", p.id)))?;
        let merged = merge_column_update(&current, &p);
        validate_column_update(&current, &merged, &p, &d.config)?;
        db.update_column(&p)?
    };
    d.emit_changed(BoardChangedReason::ColumnChanged, None, Some(col.id));
    Ok(json!(col))
}

pub(super) fn column_reorder(d: &Arc<Daemon>, p: ColumnReorderParams) -> Result<Value> {
    let cols = d.store.lock().reorder_column(p.id, p.position)?;
    d.emit_changed(BoardChangedReason::ColumnChanged, None, None);
    Ok(json!(cols))
}

pub(super) fn column_delete(d: &Arc<Daemon>, p: ColumnDeleteParams) -> Result<Value> {
    {
        // Match finalization's scheduler→store order so the delete and its
        // open-run check cannot interleave with the final transaction.
        let _sched = d.sched.lock().unwrap();
        let db = d.store.lock();
        let cards = db.list_cards_in_column(p.id)?;
        let has_open_run = db.column_has_open_run(p.id)?;
        validate_column_delete(!cards.is_empty(), has_open_run, p.move_cards_to)?;
        db.delete_column(p.id, p.move_cards_to)?;
    }
    d.emit_changed(BoardChangedReason::ColumnChanged, None, None);
    Ok(json!(DeletedResult { deleted: true }))
}

// -- cards ------------------------------------------------------------------
