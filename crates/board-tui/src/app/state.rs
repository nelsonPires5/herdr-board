//! The satellite state types `App` holds: the modals (`Picker`, `Confirm`,
//! `SwitcherState`, `CommentHistoryView`), the two mini-modes
//! (`MoveColumnState`, `DragState`), the archive filter, and the toast.
//!
//! Pure data plus the small pure helpers that build it. The reducer that
//! mutates it lives in `app` and its per-screen key handlers; nothing here
//! performs I/O or renders.

use board_core::model::{Column, CommentHistory};

use super::Screen;

/// Which level the Compact switcher sheet is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SwitcherLevel {
    /// The current board's columns, plus a trailing "switch board" row.
    Columns,
    /// The list of boards (reached via the trailing row).
    Boards,
}

/// State for the Compact-only two-level switcher sheet (`Screen::Switcher`).
pub struct SwitcherState {
    pub level: SwitcherLevel,
    pub sel: usize,
    /// The Columns-level selection to restore when backing out of `Boards`
    /// via `Esc`, rather than resetting to the top row. Only meaningful when
    /// `entered_at_boards` is `false`.
    pub columns_sel: usize,
    /// Whether this sheet was opened directly at `Boards` (`b`, which means
    /// "switch board") rather than drilled into from `Columns` (the header's
    /// center-button tap). Determines what `Esc` does at the `Boards` level:
    /// `true` closes the sheet outright, `false` steps back to `Columns` and
    /// restores `columns_sel`. Never used at the `Columns` level.
    pub entered_at_boards: bool,
    /// Populated on transition to `Boards`; `(label, board_id)`.
    pub boards: Vec<(String, i64)>,
    /// Where closing this sheet lands. See [`Screen`]'s `return_to` note.
    pub return_to: Screen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailScrollTarget {
    Comments,
    Runs,
}

/// State for the `Screen::CommentHistory` sheet: one comment's full audit
/// trail, oldest → newest, with its own scroll offset.
pub struct CommentHistoryView {
    pub comment_id: i64,
    pub entries: Vec<CommentHistory>,
    pub scroll: usize,
}

/// Which cards are visible on the board. Archiving never deletes history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardFilter {
    Active,
    All,
    Archived,
}

impl CardFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Archived,
            Self::Archived => Self::Active,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::All => "ALL",
            Self::Archived => "ARCHIVED",
        }
    }
}

/// A transient status message.
pub struct Toast {
    pub text: String,
    pub is_error: bool,
    /// Wall-clock second at which it was raised (for expiry in the run loop).
    pub at: i64,
}

/// A column picker (move card / choose where a deleted column's cards go).
pub struct Picker {
    pub title: String,
    pub options: Vec<(String, i64)>,
    pub sel: usize,
    pub purpose: PickerPurpose,
    /// Where dismissing this picker lands. See [`Screen`]'s `return_to` note.
    pub return_to: Screen,
}

#[derive(Clone, Copy)]
pub enum PickerPurpose {
    SwitchBoard,
    /// Cross-board move: choosing the destination board (stage 1).
    MoveCardPickBoard {
        card_id: i64,
    },
    /// Cross-board move: choosing a column of `board_id` (stage 2).
    MoveCardPickColumn {
        card_id: i64,
        board_id: i64,
    },
    DeleteColumnMoveTo {
        column_id: i64,
    },
}

/// Columns as `(label, id)` picker options, optionally without `exclude` — the
/// one place that shape is built, for the three column pickers (`D`'s
/// relocation target, `m`'s same-board move, and stage 2 of a cross-board
/// move, which excludes nothing because none of its columns is the current
/// one).
pub(crate) fn column_options(columns: &[Column], exclude: Option<i64>) -> Vec<(String, i64)> {
    columns
        .iter()
        .filter(|c| Some(c.id) != exclude)
        .map(|c| (c.name.clone(), c.id))
        .collect()
}

/// A yes/no confirmation.
pub struct Confirm {
    pub message: String,
    pub purpose: ConfirmPurpose,
    /// Where BOTH answers land. See [`Screen`]'s `return_to` note — the screen
    /// to go back to is a property of how the sheet was opened, never
    /// something to re-derive from `purpose`.
    pub return_to: Screen,
}

#[derive(Clone, Copy)]
pub enum ConfirmPurpose {
    DeleteCard(i64),
    /// Delete a column, optionally relocating its cards first (the
    /// destination the `D` picker collected).
    DeleteColumn {
        id: i64,
        move_cards_to: Option<i64>,
    },
    CancelRun(i64),
    /// Relaunch a real agent for this card — as destructive as it is
    /// expensive, hence the confirmation.
    RetryRun(i64),
    DeleteComment(i64),
}

/// In-progress "move column" mini-mode state (entered with `M`).
///
/// The staged reorder lives **here**, never in `App::board`: the snapshot is
/// the daemon's answer to `board.get` and a refresh tick can replace it at any
/// moment, which would silently discard an order staged inside it. Holding the
/// permutation separately means the staged order survives a mid-mode refresh
/// and is applied only at read time (see [`super::App::display_column`]).
pub struct MoveColumnState {
    pub column_id: i64,
    /// Where the column sat when `M` was pressed — where `Esc` puts it back.
    pub original_index: usize,
    /// Where ←/→ have currently staged it. `Enter` commits exactly this as the
    /// `column.reorder` position.
    pub staged_index: usize,
}

/// In-progress "reorder card" mini-mode state (entered with `O`).
///
/// Like [`MoveColumnState`], the staged position lives **here**, never in
/// `App::board`: `App::cards_of` applies it as a read-time permutation, so a
/// refresh tick landing mid-mode cannot silently discard the staged order.
pub struct ReorderCardState {
    pub card_id: i64,
    pub column_id: i64,
    /// Where the card sat when `O` was pressed — where `Esc` puts it back.
    pub original_index: usize,
    /// Where `j`/`k` have currently staged it. `Enter` commits exactly this as
    /// the `card.move` position within the same column.
    pub staged_index: usize,
}

/// Mouse drag in progress.
pub struct DragState {
    pub kind: DragKind,
    pub from_col: usize,
    pub hover_col: usize,
    /// For a card drag: the card's index within `from_col` when the drag
    /// began, so a drop back at the origin is a no-op. `None` when the card
    /// is no longer in that column (or for a column drag).
    pub from_card: Option<usize>,
    /// For a card drag: the card index currently hovered in the drag's
    /// column — the position a same-column drop would land at. `None` while
    /// hovering empty space or another column.
    pub hover_card: Option<usize>,
}

#[derive(Clone, Copy)]
pub enum DragKind {
    Card { card_id: i64 },
    Column { column_id: i64 },
}
