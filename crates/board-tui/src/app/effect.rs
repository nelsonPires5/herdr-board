//! The reducer's output alphabet.
//!
//! `app::update` never performs I/O; it returns [`Effect`]s and
//! `driver::dispatch` is the only thing that executes them. Keeping the enum
//! in its own module makes that contract greppable: anything that constructs
//! an `Effect` is pure, anything that matches one is the driver.

use board_core::protocol::{CardMoveParams, RunOutcome};

use super::CardFilter;

/// A side effect for the driver to perform (client I/O, editor, quit).
pub enum Effect {
    Refetch,
    LoadBoards,
    /// Compact-only switcher, level 2: fetch the board list into `app.switcher`
    /// instead of the Regular/Wide `Picker`.
    LoadBoardsForSwitcher,
    SwitchBoard(i64),
    /// Cross-board move, stage 1: open the destination-board picker.
    LoadBoardsForMove {
        card_id: i64,
    },
    /// Cross-board move, stage 2: load the selected destination board's columns
    /// into the picker.
    LoadColumnsForMove {
        card_id: i64,
        board_id: i64,
    },
    LoadDetail(i64),
    CardCreate(board_core::protocol::CardCreateParams),
    CardUpdate(board_core::protocol::CardUpdateParams),
    CardDelete(i64),
    CardArchive {
        id: i64,
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
    /// Keep the Herdr pane border title in sync with the archive filter.
    SetPaneTitle(CardFilter),
    Quit,
}
