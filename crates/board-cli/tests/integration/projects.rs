//! Project and cross-project board surface: create/select/list projects,
//! per-project board listing, board create/rename, and cross-project moves —
//! with the side-effect rule (only explicit open/create/select touch the
//! daemon's persisted selection).

use board_core::client::BoardClient;
use serde_json::Value;

use super::{json_error, json_output, TestDaemon};

fn canonical(dir: &std::path::Path) -> String {
    dir.canonicalize().unwrap().to_str().unwrap().to_string()
}

/// (a) `project create` on an existing directory creates the project with a
/// `main` board and persists the selection; `project.selected` echoes it and
/// `project.list` shows the folder name with Global last.
#[test]
fn project_create_selects_and_lists() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("alpha");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);

    let created = json_output(&td.board(&["project", "create", &scope, "--json"]));
    assert_eq!(created["project"]["scope_path"], scope);
    assert_eq!(created["project"]["name"], "alpha");
    assert_eq!(created["board"]["board"]["name"], "main");

    let selected = td.client().project_selected().unwrap();
    assert_eq!(
        selected.project.unwrap().scope_path.as_deref(),
        Some(scope.as_str())
    );
    assert_eq!(selected.board.unwrap().board.name, "main");

    let listed = json_output(&td.board(&["project", "list", "--json"]));
    let projects = listed["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2, "created project plus Global");
    assert_eq!(projects[0]["project"]["name"], "alpha");
    assert_eq!(projects[0]["project"]["scope_path"], scope);
    assert_eq!(projects[1]["project"]["name"], "Global");
    assert_eq!(projects[1]["project"]["scope_path"], Value::Null);
}

/// (b) Selecting a project that exists on disk but has no project row is an
/// RPC NotFound: exit code 2 and a JSON error pointing at the create command.
#[test]
fn project_select_of_uncreated_directory_is_not_found() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("never-created");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);

    let out = td.board(&["project", "select", &scope, "--json"]);
    let error = json_error(&out);
    assert_eq!(error["error"]["code"], 2, "RPC NotFound is protocol code 2");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("board project create"),
        "the error must point at the create command: {message}"
    );
}

/// (c) Creating a project for a directory that does not exist is a CLI-local
/// error (canonicalization fails before any RPC): exit 64.
#[test]
fn project_create_requires_an_existing_directory() {
    let td = TestDaemon::start(&[]);
    let missing = td._dir.path().join("does-not-exist");
    let out = td.board(&["project", "create", missing.to_str().unwrap(), "--json"]);
    let error = json_error(&out);
    assert_eq!(error["error"]["code"], 64, "CLI-local errors exit 64");
    assert_eq!(error["error"]["kind"], "cli");
}

/// (d) `board board create` targets the selected project and auto-selects the
/// new board; a case-insensitive duplicate name is a bad request (exit 1).
#[test]
fn board_create_auto_selects_and_rejects_duplicates() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("boards-project");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);
    json_output(&td.board(&["project", "create", &scope, "--json"]));

    let created = json_output(&td.board(&["board", "create", "backlog", "--json"]));
    let backlog_id = created["board"]["id"].as_i64().unwrap();
    assert_eq!(created["board"]["name"], "backlog");

    let listed = td.client().project_list().unwrap();
    let info = listed
        .projects
        .iter()
        .find(|info| info.project.scope_path.as_deref() == Some(scope.as_str()))
        .unwrap();
    assert_eq!(
        info.selected_board_id,
        Some(backlog_id),
        "creating a board auto-selects it"
    );

    // Case-insensitive duplicate within the same project.
    let dup = td.board(&["board", "create", "BACKLOG", "--json"]);
    assert_eq!(
        dup.status.code(),
        Some(1),
        "duplicate board is a bad request"
    );
}

/// (e) `board board list` shows only the selected project's boards;
/// `--all` shows every project's boards.
#[test]
fn board_list_is_per_project_by_default() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("list-project");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);
    json_output(&td.board(&["project", "create", &scope, "--json"]));
    json_output(&td.board(&["board", "create", "backlog", "--json"]));

    let per_project = json_output(&td.board(&["board", "list", "--json"]));
    let boards = per_project.as_array().unwrap();
    assert_eq!(boards.len(), 2, "only the selected project's boards");
    assert!(boards
        .iter()
        .all(|board| board["name"] == "main" || board["name"] == "backlog"));

    let all = json_output(&td.board(&["board", "list", "--all", "--json"]));
    let all_boards = all.as_array().unwrap();
    assert_eq!(
        all_boards.len(),
        3,
        "Global plus the created project's boards"
    );
}

/// (f) `project select PATH --board NAME` persists the project AND the named
/// board as the context.
#[test]
fn project_select_picks_a_named_board() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("select-project");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);
    json_output(&td.board(&["project", "create", &scope, "--json"]));
    let docs = json_output(&td.board(&["board", "create", "docs", "--json"]));
    let docs_id = docs["board"]["id"].as_i64().unwrap();

    let selected =
        json_output(&td.board(&["project", "select", &scope, "--board", "docs", "--json"]));
    assert_eq!(selected["project"]["name"], "select-project");
    assert_eq!(selected["board"]["board"]["name"], "docs");

    let context = td.client().project_selected().unwrap();
    assert_eq!(
        context.project.unwrap().scope_path.as_deref(),
        Some(scope.as_str())
    );
    assert_eq!(context.board.unwrap().board.id, docs_id);
}

/// (g) A cross-project `card move --to-project B --to-board main` lands the
/// card on B's main board and never touches the persisted selection: the
/// daemon still reports project A as selected afterwards.
#[test]
fn cross_project_move_keeps_the_selection() {
    let td = TestDaemon::start(&[]);
    let a = td._dir.path().join("move-a");
    let b = td._dir.path().join("move-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let a_scope = canonical(&a);
    let b_scope = canonical(&b);

    // Select project A and put a card on its main board.
    json_output(&td.board(&["project", "create", &a_scope, "--json"]));
    let card = json_output(&td.board(&["card", "create", "--title", "cross the border", "--json"]));
    let card_id = card["id"].as_i64().unwrap();
    let a_main = td
        .client()
        .project_get(&a_scope)
        .unwrap()
        .boards
        .into_iter()
        .find(|board| board.name == "main")
        .unwrap()
        .id;

    // Project B exists but is not selected.
    json_output(&td.board(&["project", "create", &b_scope, "--json"]));
    // Re-select A so the card create above stays the context... the create
    // command already selected A; B's create selected B, so go back to A.
    json_output(&td.board(&["project", "select", &a_scope, "--json"]));

    let moved = json_output(&td.board(&[
        "card",
        "move",
        &card_id.to_string(),
        "Todo",
        "--to-project",
        &b_scope,
        "--to-board",
        "main",
        "--json",
    ]));
    assert_eq!(moved["id"], card_id);
    let b_main = td
        .client()
        .project_get(&b_scope)
        .unwrap()
        .boards
        .into_iter()
        .find(|board| board.name == "main")
        .unwrap()
        .id;
    assert_ne!(b_main, a_main);
    assert_eq!(
        moved["board_id"], b_main,
        "the card lands on B's main board"
    );
    assert_eq!(
        td.client().card_get(card_id).unwrap().card.board_id,
        b_main,
        "the transfer must be persisted"
    );

    // The move is not an explicit selection: the context is still project A.
    let context = td.client().project_selected().unwrap();
    assert_eq!(
        context.project.unwrap().scope_path.as_deref(),
        Some(a_scope.as_str())
    );
}

/// (h) `board board rename` still works, and renaming onto a sibling name in
/// the same project is a bad request (exit 1).
#[test]
fn board_rename_works_and_rejects_sibling_collisions() {
    let td = TestDaemon::start(&[]);
    let dir = td._dir.path().join("rename-project");
    std::fs::create_dir_all(&dir).unwrap();
    let scope = canonical(&dir);
    json_output(&td.board(&["project", "create", &scope, "--json"]));
    let docs = json_output(&td.board(&["board", "create", "docs", "--json"]));
    let docs_id = docs["board"]["id"].as_i64().unwrap();

    let renamed = json_output(&td.board(&[
        "board",
        "rename",
        &docs_id.to_string(),
        "documentation",
        "--json",
    ]));
    assert_eq!(renamed["id"], docs_id);
    assert_eq!(renamed["name"], "documentation");

    // "MAIN" collides case-insensitively with the project's first board.
    let dup = td.board(&["board", "rename", &docs_id.to_string(), "MAIN", "--json"]);
    assert_eq!(
        dup.status.code(),
        Some(1),
        "sibling collision is a bad request"
    );
}
