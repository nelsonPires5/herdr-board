//! The reducer's output alphabet.
//!
//! `app::update` never performs I/O; it returns [`Effect`]s and
//! `driver::dispatch` is the only thing that executes them. Keeping the enum
//! in its own module makes that contract greppable: anything that constructs
//! an `Effect` is pure, anything that matches one is the driver.

use board_core::protocol::{BoardCreateParams, CardMoveParams, ProjectCreateParams, RunOutcome};

use super::CardFilter;

/// A side effect for the driver to perform (client I/O, editor, quit).
pub enum Effect {
    Refetch,
    /// Refresh the `app.projects` cache and `app.project` from `project.list`.
    LoadProjects,
    /// Fetch `project.list` and open `Screen::ProjectPicker`.
    LoadProjectPicker,
    /// Fetch `project.list` and open `Screen::BoardPicker` for the given
    /// project (`None` = the current project).
    LoadBoardPicker {
        project_id: Option<i64>,
    },
    /// `project.select`: switch the context to `project_id`'s `board_id`.
    SelectProject {
        project_id: i64,
        board_id: i64,
    },
    /// `board.select`: switch the context to `board_id`.
    SelectBoard(i64),
    /// `project.create`: create the project and land on its `main` board.
    ProjectCreate(ProjectCreateParams),
    /// `board.create`: create a named board in a project and land on it.
    BoardCreate(BoardCreateParams),
    /// Fetch `board.get` for the move form's destination board and populate
    /// its Column field.
    LoadMoveColumns {
        board_id: i64,
    },
    LoadDetail(i64),
    CardCreate(board_core::protocol::CardCreateParams),
    CardUpdate(board_core::protocol::CardUpdateParams),
    CardDelete(i64),
    /// Duplicate a card: the daemon creates an idle copy directly below the
    /// original, never dispatching a run.
    CardDuplicate(i64),
    CardArchive {
        id: i64,
        archived: bool,
    },
    BoardArchive {
        board_id: i64,
        archived: bool,
    },
    ProjectArchive {
        project_id: i64,
        archived: bool,
    },
    CardMove(CardMoveParams),
    ColumnCreate(board_core::protocol::ColumnCreateParams),
    ColumnUpdate(board_core::protocol::ColumnUpdateParams),
    ColumnReorder {
        id: i64,
        position: i64,
    },
    ColumnDelete {
        id: i64,
        move_cards_to: Option<i64>,
    },
    CommentAdd {
        card_id: i64,
        body: String,
    },
    CommentUpdate {
        id: i64,
        body: String,
    },
    CommentDelete {
        id: i64,
    },
    /// Fetch a comment's full audit trail (`comment.history`) and open
    /// `Screen::CommentHistory` with it.
    LoadCommentHistory {
        id: i64,
    },
    TemplateApply(String),
    RunCancel(i64),
    RunRetry(i64),
    RunDone(i64, RunOutcome),
    /// Focus one exact run's pane: `(card_id, run_id)`. The run is chosen by
    /// the TUI (`run.focus` never picks one implicitly).
    FocusRun(i64, i64),
    /// Hand the focused multiline text field to `$EDITOR`.
    EditFocusedTextArea,
    /// Fetch `harness.capabilities` + `session.list` + `space.list` for the open
    /// card form and populate its guided selectors. Emitted on form open and on
    /// harness/session change (the latter re-scopes the workspace list).
    LoadFormOptions,
    /// Keep the Herdr pane border title in sync with the archive filters.
    SetPaneTitle(CardFilter),
    /// Reload the project and board pickers with the given visibilities.
    ReloadPickers,
    Quit,
}
