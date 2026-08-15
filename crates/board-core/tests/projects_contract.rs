//! Project/board selection contract: persistent selection, per-project board
//! selection, recency capped at three, and the open/create/select side-effect
//! rules (queries and moves never touch recency).

use board_core::db::Db;

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
