//! Archive/restore decisions for boards and projects.
//!
//! Pure, synchronous, no I/O. The daemon gathers facts from the DB
//! (`has_open_run`, `unarchived_board_count`, `is_global`) and asks these
//! helpers whether the target transition is allowed.

use thiserror::Error;

/// Rejection for `board.archive`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BoardArchiveRejection {
    #[error(
        "board {board_id} has an open run and cannot be archived; finish or cancel the run first"
    )]
    OpenRun { board_id: i64 },
}

/// Rejection for `project.archive`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectArchiveRejection {
    #[error("the Global project cannot be archived")]
    GlobalProject,
    #[error(
        "project has {count} active board(s); archive every board first with `board board archive <id>`"
    )]
    ActiveBoards { count: usize },
    #[error("project has an open run and cannot be archived; finish or cancel the run first")]
    OpenRun,
}

/// Why an archived destination cannot accept new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("archived {kind} must be restored first: `{restore_hint}`")]
pub struct ArchivedDestination {
    pub kind: &'static str,
    pub restore_hint: &'static str,
}

/// Decide whether a board may be moved to `target_archived`.
///
/// Idempotent: archiving an already-archived board (or restoring an active
/// one) is a no-op and never rejected. Finished runs do not block.
pub fn decide_board_archive(
    board_id: i64,
    already_archived: bool,
    has_open_run: bool,
    target_archived: bool,
) -> Result<(), BoardArchiveRejection> {
    if already_archived == target_archived {
        return Ok(());
    }
    if target_archived && has_open_run {
        return Err(BoardArchiveRejection::OpenRun { board_id });
    }
    Ok(())
}

/// Decide whether a project may be moved to `target_archived`.
///
/// Restoring (`target_archived=false`) is always allowed.
pub fn decide_project_archive(
    is_global: bool,
    already_archived: bool,
    unarchived_board_count: usize,
    has_open_run: bool,
    target_archived: bool,
) -> Result<(), ProjectArchiveRejection> {
    if already_archived == target_archived {
        return Ok(());
    }
    if !target_archived {
        return Ok(());
    }
    if is_global {
        return Err(ProjectArchiveRejection::GlobalProject);
    }
    if unarchived_board_count > 0 {
        return Err(ProjectArchiveRejection::ActiveBoards {
            count: unarchived_board_count,
        });
    }
    if has_open_run {
        return Err(ProjectArchiveRejection::OpenRun);
    }
    Ok(())
}

/// Whether `board_id` / `project` as a destination may accept new work.
///
/// Archived boards or projects refuse card creation, moves, dispatch, retry,
/// resume, and template application until restored.
pub fn decide_new_work_on_board(
    board_archived: bool,
    board_id: i64,
) -> Result<(), ArchivedDestination> {
    if board_archived {
        return Err(ArchivedDestination {
            kind: "board",
            restore_hint: "board board restore <id|name>",
        });
    }
    let _ = board_id;
    Ok(())
}

pub fn decide_new_work_on_project(project_archived: bool) -> Result<(), ArchivedDestination> {
    if project_archived {
        return Err(ArchivedDestination {
            kind: "project",
            restore_hint: "board project restore <path>",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_archive_allows_finished_runs_and_idempotent() {
        assert!(decide_board_archive(1, false, false, true).is_ok());
        assert!(decide_board_archive(1, true, false, true).is_ok());
        assert!(decide_board_archive(1, true, true, true).is_ok()); // idempotent even with open run
        assert!(decide_board_archive(1, false, false, false).is_ok());
    }

    #[test]
    fn board_archive_refuses_open_run() {
        let err = decide_board_archive(7, false, true, true).unwrap_err();
        assert_eq!(err, BoardArchiveRejection::OpenRun { board_id: 7 });
    }

    #[test]
    fn project_archive_rules() {
        assert!(decide_project_archive(true, false, 0, false, true).is_err()); // global
        assert!(matches!(
            decide_project_archive(false, false, 2, false, true),
            Err(ProjectArchiveRejection::ActiveBoards { count: 2 })
        ));
        assert!(matches!(
            decide_project_archive(false, false, 0, true, true),
            Err(ProjectArchiveRejection::OpenRun)
        ));
        assert!(decide_project_archive(false, false, 0, false, true).is_ok());
        assert!(decide_project_archive(false, true, 0, false, false).is_ok()); // restore
        assert!(decide_project_archive(false, true, 0, true, true).is_ok()); // idempotent
    }
}
