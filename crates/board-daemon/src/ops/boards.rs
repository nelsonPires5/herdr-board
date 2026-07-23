use super::*;
use board_core::db::BOARD_ID;
pub(super) fn daemon_status(d: &Arc<Daemon>) -> Result<Value> {
    let (active_runs, queued_runs) = {
        let db = d.store.lock();
        (db.count_active_runs()?, db.count_queued_runs()?)
    };
    let herdr_connected = match &d.herdr {
        Some(h) => {
            let mut c = h.clone();
            c.is_live()
        }
        None => false,
    };
    Ok(json!(DaemonStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        db_path: d.db_path.to_string_lossy().into_owned(),
        herdr_connected,
        active_runs,
        queued_runs,
    }))
}

fn board_snapshot(d: &Arc<Daemon>, board_id: i64) -> Result<Value> {
    let db = d.store.lock();
    Ok(json!(BoardSnapshot {
        board: db.get_board(board_id)?,
        columns: db.list_columns(board_id)?,
        cards: db.list_cards(board_id)?,
        active_runs: db.active_run_summaries(board_id)?,
    }))
}

pub(super) fn board_open(d: &Arc<Daemon>, p: BoardOpenParams) -> Result<Value> {
    let board = d.store.lock().open_board(&p.scope_path)?;
    board_snapshot(d, board.id)
}

pub(super) fn board_list(d: &Arc<Daemon>) -> Result<Value> {
    Ok(json!(BoardListResult {
        boards: d.store.lock().list_boards()?,
    }))
}

pub(super) fn board_get(d: &Arc<Daemon>, p: BoardGetParams) -> Result<Value> {
    board_snapshot(d, p.board_id.unwrap_or(BOARD_ID))
}

// -- columns ----------------------------------------------------------------
