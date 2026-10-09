//! Project/board selection contract: persistent selection, per-project board
//! selection, recency capped at three, and the open/create/select side-effect
//! rules (queries and moves never touch recency).

use board_core::db::{Db, EnqueueRun, FinalizeRun};
use board_core::protocol::{CardCreateParams, CardStatus, RunOutcome, Visibility};

fn mem() -> Db {
    Db::open_in_memory().expect("in-memory db")
}

#[test]
fn fresh_db_has_only_the_global_project_with_a_main_board() {
    let db = mem();
    let projects = db.list_projects().expect("projects");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Global");
    assert_eq!(projects[0].scope_path, None);
    // No selection yet: bootstrap state.
    assert_eq!(db.selected_project_id().expect("selection"), None);
    assert_eq!(
        db.recent_project_ids_excluding(None).expect("recents"),
        Vec::<i64>::new()
    );

    let board = db
        .project_context_board(projects[0].id)
        .expect("context board");
    assert_eq!(board.name, "main");
    assert_eq!(board.id, 1);
}

#[test]
fn project_creation_selects_project_and_main_board() {
    let db = mem();
    let (project, board) = db
        .create_project_context("/tmp/alpha/project")
        .expect("create project");
    assert_eq!(project.name, "project");
    assert_eq!(project.scope_path.as_deref(), Some("/tmp/alpha/project"));
    assert_eq!(board.name, "main");
    assert_eq!(board.project_id, project.id);
    assert_eq!(db.list_columns(board.id).expect("columns").len(), 1);

    // Creating selects both and updates recency.
    assert_eq!(
        db.selected_project_id().expect("selected"),
        Some(project.id)
    );
    assert_eq!(
        db.selected_board_id_for(project.id).expect("selected"),
        Some(board.id)
    );
    assert_eq!(
        db.recent_project_ids_excluding(Some(project.id))
            .expect("recents"),
        Vec::<i64>::new(),
        "the current project is excluded from its own recents"
    );
    // Duplicate creation is a bad request.
    let dup = db
        .create_project_context("/tmp/alpha/project")
        .expect_err("duplicate");
    assert_eq!(dup.code(), 1);
}

#[test]
fn recency_is_capped_at_three_and_most_recent_first() {
    let db = mem();
    let mut projects = Vec::new();
    for path in ["/r/a", "/r/b", "/r/c", "/r/d", "/r/e"] {
        let (project, _) = db.open_project_context(path).expect("open");
        projects.push(project);
    }
    // Five opens: only the three most recent remain, most recent first.
    let recents = db.recent_project_ids_excluding(None).expect("recents");
    assert_eq!(
        recents,
        vec![projects[4].id, projects[3].id, projects[2].id]
    );

    // Touching an old project moves it to the front; the touched project is
    // then excluded from the picker's recents section.
    db.select_project_by_scope("/r/a", None).expect("re-select");
    let recents = db
        .recent_project_ids_excluding(Some(projects[0].id))
        .expect("recents");
    assert_eq!(recents, vec![projects[4].id, projects[3].id]);
    let recents = db.recent_project_ids_excluding(None).expect("recents");
    assert_eq!(
        recents,
        vec![projects[0].id, projects[4].id, projects[3].id]
    );
}

#[test]
fn per_project_board_recency_and_selection_are_isolated() {
    let db = mem();
    let (pa, main_a) = db.open_project_context("/r/a").expect("a");
    let (pb, main_b) = db.open_project_context("/r/b").expect("b");

    let b1 = db.create_board(pa.id, "Backlog").expect("backlog");
    let b2 = db.create_board(pa.id, "Archive").expect("archive");
    let b3 = db
        .create_board(pb.id, "Backlog")
        .expect("other project backlog");

    // Same board name in another project is fine.
    assert_eq!(b3.name, "Backlog");
    // Recency per project, capped at 3, most recent first, current excluded.
    assert_eq!(
        db.recent_board_ids_excluding(pa.id, None).expect("recents"),
        vec![b2.id, b1.id, main_a.id]
    );
    assert_eq!(
        db.recent_board_ids_excluding(pb.id, None).expect("recents"),
        vec![b3.id, main_b.id]
    );

    // Selecting a board in project B moves the context there.
    db.select_board(b3.id).expect("select board");
    assert_eq!(db.selected_project_id().expect("selected"), Some(pb.id));
    assert_eq!(
        db.selected_board_id_for(pb.id).expect("selected"),
        Some(b3.id)
    );
    // Project A's board selection is untouched by B's activity.
    assert_eq!(
        db.selected_board_id_for(pa.id).expect("selected"),
        Some(b2.id)
    );
}

#[test]
fn selection_and_recency_survive_a_file_reopen() {
    // Selection and recency are durable (SQLite): a daemon restart reopens
    // the same file and must see the same context.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("selection.db");
    let (alpha_id, beta_id, beta_board_id);
    {
        let db = Db::open(&path).expect("open");
        let (alpha, _) = db
            .create_project_context("/tmp/restart-alpha")
            .expect("alpha");
        let (beta, beta_board) = db
            .create_project_context("/tmp/restart-beta")
            .expect("beta");
        alpha_id = alpha.id;
        beta_id = beta.id;
        beta_board_id = beta_board.id;
        // Newest creation selects beta; alpha is the recency entry.
        assert_eq!(db.selected_project_id().expect("selected"), Some(beta.id));
        assert_eq!(
            db.recent_project_ids_excluding(Some(beta.id))
                .expect("recents"),
            vec![alpha.id]
        );
    }
    let db = Db::open(&path).expect("reopen");
    assert_eq!(
        db.selected_project_id().expect("selected"),
        Some(beta_id),
        "selection must survive a daemon restart"
    );
    assert_eq!(
        db.selected_board_id_for(beta_id).expect("board"),
        Some(beta_board_id)
    );
    assert_eq!(
        db.recent_project_ids_excluding(Some(beta_id))
            .expect("recents"),
        vec![alpha_id],
        "recency must survive a daemon restart"
    );
}

#[test]
fn selecting_a_missing_project_fails_with_the_create_hint() {
    let db = mem();
    let err = db
        .require_project_by_scope("/never/created")
        .expect_err("missing");
    assert_eq!(err.code(), 2);
    assert!(
        err.to_string().contains("board project create"),
        "error must point at the create command: {err}"
    );
}

#[test]
fn board_create_is_auto_selected_and_cannot_duplicate_names_case_insensitively() {
    let db = mem();
    let (project, _) = db.open_project_context("/r/a").expect("project");
    let board = db.create_board(project.id, "Backlog").expect("create");
    assert_eq!(
        db.selected_board_id_for(project.id).expect("selected"),
        Some(board.id)
    );

    let dup = db
        .create_board(project.id, "backlog")
        .expect_err("case-insensitive dup");
    assert_eq!(dup.code(), 1);
    // Same name in the Global project is legal.
    let global = db.create_board(1, "Backlog").expect("global backlog");
    assert_eq!(global.name, "Backlog");
    assert_ne!(global.id, board.id);
}

#[test]
fn open_board_resolution_never_touches_selection_or_recency() {
    let db = mem();
    db.open_project_context("/r/a").expect("context a");
    let before_selected = db.selected_project_id().expect("selected");
    let before_recents = db.recent_project_ids_excluding(None).expect("recents");

    // board.open is the query/move resolution primitive: no side effects.
    let board = db.open_board("/r/b").expect("open b");
    assert_eq!(db.selected_project_id().expect("selected"), before_selected);
    assert_eq!(
        db.recent_project_ids_excluding(None).expect("recents"),
        before_recents
    );

    // But the resolution itself is a real get-or-create with a context board.
    let project = db.get_project(board.project_id).expect("project");
    assert_eq!(project.scope_path.as_deref(), Some("/r/b"));
    assert_eq!(board.name, "main");
}

#[test]
fn project_list_result_is_deterministic_and_picker_ready() {
    let db = mem();
    db.open_project_context("/r/zeta").expect("zeta");
    db.open_project_context("/r/alpha").expect("alpha");
    db.select_project_by_scope("/r/alpha", None)
        .expect("select alpha");

    let result = db.project_list_result().expect("list");
    // Folder-name order, Global last.
    let names: Vec<&str> = result
        .projects
        .iter()
        .map(|p| p.project.name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "zeta", "Global"]);
    assert_eq!(
        result.selected_project_id,
        result.projects[0].project.id.into()
    );
    // Each project serves its own boards plus recency.
    let alpha = &result.projects[0];
    assert!(alpha.boards.iter().any(|b| b.name == "main"));
    assert_eq!(alpha.selected_board_id, alpha.boards[0].id.into());
    // Global is the special project with one board.
    let global = &result.projects[2];
    assert_eq!(global.project.scope_path, None);
    assert_eq!(global.boards.len(), 1);
    assert_eq!(global.boards[0].name, "main");
}

// --- board/project archive wiring (e2e/38 hermetic keepers) ---
//
// The live scenario keeps one open-run refusal trip; everything below is the
// wiring that the removed live matrices used to prove.

#[test]
fn board_archive_visibility_active_all_archived() {
    let db = mem();
    let (project, _) = db.open_project_context("/arch/alpha").expect("project");
    let board = db.create_board(project.id, "ArchiveMe").expect("board");
    db.set_board_archived(board.id, true).expect("archive");

    let active = db
        .list_boards_for_project_filtered(project.id, Some(Visibility::Active))
        .expect("active");
    assert!(
        active.iter().all(|b| b.archived_at.is_none()),
        "active must hide archived boards: {active:?}"
    );
    assert!(
        !active.iter().any(|b| b.id == board.id),
        "archived board leaked into active"
    );
    let archived = db
        .list_boards_for_project_filtered(project.id, Some(Visibility::Archived))
        .expect("archived");
    assert!(
        archived
            .iter()
            .any(|b| b.id == board.id && b.archived_at.is_some()),
        "archived visibility must include the board with archived_at: {archived:?}"
    );
    let all = db
        .list_boards_for_project_filtered(project.id, Some(Visibility::All))
        .expect("all");
    assert!(all.iter().any(|b| b.id == board.id), "all must include it");
    assert!(
        all.iter().any(|b| b.name == "main"),
        "all must still include the active main board"
    );
}

#[test]
fn project_archive_visibility_active_all_archived() {
    let db = mem();
    let (alpha, main) = db.create_project_context("/arch/vis-alpha").expect("alpha");
    assert_eq!(alpha.name, "vis-alpha");
    // A project archives only after every board is archived.
    db.set_board_archived(main.id, true).expect("archive main");
    db.set_project_archived("/arch/vis-alpha", true)
        .expect("archive project");

    let active = db
        .list_projects_filtered(Some(Visibility::Active))
        .expect("active");
    assert!(
        !active.iter().any(|p| p.id == alpha.id),
        "active must hide the archived project"
    );
    let archived = db
        .list_projects_filtered(Some(Visibility::Archived))
        .expect("archived");
    assert!(
        archived
            .iter()
            .any(|p| p.id == alpha.id && p.archived_at.is_some()),
        "archived must include the project with archived_at"
    );
    let all = db
        .list_projects_filtered(Some(Visibility::All))
        .expect("all");
    assert!(
        all.iter().any(|p| p.id == alpha.id),
        "all must include the archived project"
    );
}

#[test]
fn archived_board_name_stays_reserved_nocase_and_project_path_reserved() {
    let db = mem();
    let (project, _) = db.open_project_context("/arch/names").expect("project");
    let board = db.create_board(project.id, "ArchiveMe").expect("board");
    db.set_board_archived(board.id, true).expect("archive");

    // The UNIQUE (project_id, name COLLATE NOCASE) index has no archived
    // exemption: the archived name stays reserved, case-insensitively.
    let dup = db.create_board(project.id, "archiveme").expect_err("dup");
    assert_eq!(dup.code(), 1);
    assert!(
        dup.to_string().contains("already exists"),
        "duplicate message must say already exists: {dup}"
    );
    // Restore never requires a rename: the same name is still taken, and an
    // explicit reselect of the restored board works without renaming.
    let restored = db.set_board_archived(board.id, false).expect("restore");
    assert!(restored.archived_at.is_none());
    let still_dup = db.create_board(project.id, "ArchiveMe").expect_err("dup");
    assert_eq!(still_dup.code(), 1);

    // Project scope paths stay reserved while archived: the partial UNIQUE
    // index is on scope_path IS NOT NULL, and archiving keeps the path.
    let (beta, beta_main) = db.create_project_context("/arch/names-beta").expect("beta");
    db.set_board_archived(beta_main.id, true)
        .expect("archive main");
    db.set_project_archived("/arch/names-beta", true)
        .expect("archive beta");
    let dup_project = db
        .create_project_context("/arch/names-beta")
        .expect_err("project dup");
    assert_eq!(dup_project.code(), 1);
    assert!(beta.scope_path.is_some());
}

#[test]
fn board_archive_selection_falls_back_and_restore_does_not_autoselect() {
    let db = mem();
    let (project, _) = db.open_project_context("/arch/sel").expect("project");
    let first = db.create_board(project.id, "First").expect("first");
    let second = db.create_board(project.id, "Second").expect("second");
    // Creating selects the newest board.
    assert_eq!(
        db.selected_board_id_for(project.id).expect("selected"),
        Some(second.id)
    );
    // Select First explicitly, then archive it: selection must fall back to
    // an active board and never point at the archived one.
    db.select_board(first.id).expect("select first");
    db.set_board_archived(first.id, true).expect("archive");
    let selected = db.selected_board_id_for(project.id).expect("selected");
    assert_ne!(
        selected,
        Some(first.id),
        "selection must leave the archived board"
    );
    let selected_board = selected.expect("a fallback board must be selected");
    assert!(
        db.get_board(selected_board)
            .expect("board")
            .archived_at
            .is_none(),
        "fallback selection must be an active board"
    );
    // Restore never auto-selects: the restored board stays unselected until
    // an explicit select.
    db.set_board_archived(first.id, false).expect("restore");
    assert_eq!(
        db.selected_board_id_for(project.id).expect("selected"),
        Some(selected_board),
        "restore must not reselect the restored board"
    );
    db.select_board(first.id)
        .expect("explicit select after restore");
    assert_eq!(
        db.selected_board_id_for(project.id).expect("selected"),
        Some(first.id)
    );
    assert_eq!(second.project_id, project.id);
}

#[test]
fn project_archive_requires_all_boards_archived_refuses_open_run_and_falls_back() {
    let db = mem();
    let (alpha, main) = db
        .create_project_context("/arch/rule-alpha")
        .expect("alpha");
    let (beta, _) = db.create_project_context("/arch/rule-beta").expect("beta");
    // Re-select alpha: the newest creation (beta) would otherwise be selected.
    db.select_project_by_scope("/arch/rule-alpha", None)
        .expect("select alpha");
    assert_eq!(db.selected_project_id().expect("selected"), Some(alpha.id));
    // Archiving the project while its main board is still active is refused.
    let err = db
        .set_project_archived("/arch/rule-alpha", true)
        .expect_err("rule");
    assert_eq!(err.code(), 3);
    assert!(
        err.to_string().contains("active board"),
        "refusal must name the active-board rule: {err}"
    );
    assert!(
        db.get_project(alpha.id)
            .expect("project")
            .archived_at
            .is_none(),
        "refused archive must stay atomic (still active)"
    );
    // A board with an open run cannot be archived (ended_at IS NULL includes
    // queued runs): the wiring refuses before any write.
    let todo = db.default_column_id(main.id).expect("todo");
    let card = db
        .create_card(&CardCreateParams {
            title: "open-run guard".into(),
            board_id: Some(main.id),
            column_id: Some(todo),
            ..Default::default()
        })
        .expect("card");
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: todo,
            harness: "fake",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .expect("enqueue");
    let err = db.set_board_archived(main.id, true).expect_err("open run");
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("open run"), "got: {err}");
    assert!(
        db.get_board(main.id).expect("board").archived_at.is_none(),
        "refused board archive must stay atomic"
    );
    // Finish the run: archiving the board, then the project, succeeds, and
    // selection falls back away from the archived project.
    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Idle,
        final_awaiting_reason: None,
        next: None,
    })
    .expect("finalize");
    db.set_board_archived(main.id, true).expect("archive main");
    db.set_project_archived("/arch/rule-alpha", true)
        .expect("archive project");
    // Deterministic fallback: the only other active project is beta.
    assert_eq!(
        db.selected_project_id().expect("selected"),
        Some(beta.id),
        "selection must fall back to the remaining active project (beta)"
    );
    assert!(
        db.get_project(beta.id)
            .expect("project")
            .archived_at
            .is_none(),
        "fallback selection must be an active project"
    );
    // Restoring a project restores neither its boards nor its selection:
    // boards stay archived, selection stays on the fallback, and an explicit
    // select lands back on the project context afterward.
    db.set_project_archived("/arch/rule-alpha", false)
        .expect("restore project");
    assert!(
        db.get_project(alpha.id)
            .expect("project")
            .archived_at
            .is_none(),
        "restored project must be active again"
    );
    assert!(
        db.get_board(main.id).expect("board").archived_at.is_some(),
        "restoring a project must NOT restore its boards"
    );
    assert_eq!(
        db.selected_project_id().expect("selected"),
        Some(beta.id),
        "restoring a project must NOT auto-select it"
    );
    // Explicit selection works afterward (once its board is restored the
    // context board exists again).
    db.set_board_archived(main.id, false)
        .expect("restore board");
    db.select_project_by_scope("/arch/rule-alpha", None)
        .expect("explicit select after project restore");
    assert_eq!(db.selected_project_id().expect("selected"), Some(alpha.id));
    assert_eq!(
        db.selected_board_id_for(alpha.id).expect("selected"),
        Some(main.id)
    );
}

#[test]
fn board_archive_roundtrip_preserves_columns_cards_runs_and_comments() {
    let db = mem();
    let (project, _) = db.open_project_context("/arch/roundtrip").expect("project");
    let board = db.create_board(project.id, "ArchiveMe").expect("board");
    let todo = db.default_column_id(board.id).expect("todo");
    let card = db
        .create_card(&CardCreateParams {
            title: "open-run guard".into(),
            board_id: Some(board.id),
            column_id: Some(todo),
            ..Default::default()
        })
        .expect("card");
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: todo,
            harness: "fake",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .expect("enqueue");
    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Idle,
        final_awaiting_reason: None,
        next: None,
    })
    .expect("finalize");
    db.add_comment(card.id, "user", "keep me").expect("comment");

    db.set_board_archived(board.id, true).expect("archive");
    db.set_board_archived(board.id, false).expect("restore");

    let restored = db.get_board(board.id).expect("board");
    assert!(restored.archived_at.is_none());
    assert!(!db.list_columns(board.id).expect("columns").is_empty());
    let cards = db.list_cards(board.id).expect("cards");
    assert!(
        cards.iter().any(|c| c.title == "open-run guard"),
        "cards must survive the archive round-trip: {cards:?}"
    );
    assert_eq!(
        db.list_runs(card.id).expect("runs").len(),
        1,
        "run history must survive"
    );
    assert_eq!(
        db.list_comments(card.id).expect("comments").len(),
        1,
        "comments must survive"
    );
}

#[test]
fn archive_state_and_selection_survive_file_reopen() {
    // Archiving is durable SQLite state: a daemon restart reopens the same
    // file and must see the same archived_at plus the post-fallback selection.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("archive.db");
    let (project_id, board_id, beta_id);
    {
        let db = Db::open(&path).expect("open");
        let (project, main) = db.open_project_context("/arch/durable").expect("project");
        let board = db.create_board(project.id, "ArchiveMe").expect("board");
        let (beta, _) = db
            .create_project_context("/arch/durable-beta")
            .expect("beta");
        project_id = project.id;
        board_id = board.id;
        beta_id = beta.id;
        // Land back on durable/ArchiveMe so the archive trips its fallback.
        db.select_board(board.id).expect("select");
        db.set_board_archived(board.id, true).expect("archive");
        assert_ne!(
            db.selected_board_id_for(project.id).expect("selected"),
            Some(board.id)
        );
        // Archive the whole project too: every board first, then the project.
        // Selection must deterministically fall back to beta.
        db.set_board_archived(main.id, true).expect("archive main");
        db.set_project_archived("/arch/durable", true)
            .expect("archive project");
        assert!(
            db.get_project(project.id)
                .expect("project")
                .archived_at
                .is_some(),
            "project must be archived before reopen"
        );
        assert_eq!(
            db.selected_project_id().expect("selected"),
            Some(beta.id),
            "selection must fall back to beta"
        );
    }
    let db = Db::open(&path).expect("reopen");
    assert!(
        db.get_board(board_id).expect("board").archived_at.is_some(),
        "archived state must survive a daemon restart"
    );
    assert!(
        db.get_project(project_id)
            .expect("project")
            .archived_at
            .is_some(),
        "project archived state must survive a daemon restart"
    );
    assert_eq!(
        db.selected_project_id().expect("selected"),
        Some(beta_id),
        "project fallback selection must survive a daemon restart"
    );
    let selected = db.selected_board_id_for(project_id).expect("selected");
    assert_ne!(
        selected,
        Some(board_id),
        "selection must still avoid the archived board"
    );
    if let Some(sid) = selected {
        assert!(
            db.get_board(sid).expect("board").archived_at.is_none(),
            "fallback selection must still be active after reopen"
        );
    }
}
