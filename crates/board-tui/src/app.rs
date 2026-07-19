//! Application state machine: `App` state, `Screen`, synthetic `Msg`s, and the
//! pure `update(&mut App, Msg) -> Vec<Effect>` reducer. Rendering lives in `view`;
//! I/O (client calls, `$EDITOR`) lives in `lib` via the returned [`Effect`]s.
//!
//! Keeping `update` free of I/O is what lets tests drive synthetic key/mouse
//! events and assert on state (navigation, form cycling, drag transitions) and on
//! rendered snapshots deterministically.

use board_core::protocol::{BoardSnapshot, CardDetail, CardMoveParams, CardStatus, RunOutcome};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::forms::{FieldId, FieldKind, Form, Submit};

/// Which modal/screen is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Board,
    CardDetail,
    CardForm,
    ColumnForm,
    Picker,
    Confirm,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailScrollTarget {
    Comments,
    Runs,
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

/// A synthetic event fed to [`update`].
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A `board_changed` (or fallback) notification: refetch the board.
    Refresh,
}

/// A side effect for the driver to perform (client I/O, editor, quit).
pub enum Effect {
    Refetch,
    LoadBoards,
    SwitchBoard(i64),
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
    TemplateApply(String),
    RunCancel(i64),
    RunRetry(i64),
    RunDone(i64, RunOutcome),
    FocusRun(i64),
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
}

#[derive(Clone, Copy)]
pub enum PickerPurpose {
    SwitchBoard,
    MoveCard { card_id: i64 },
    DeleteColumnMoveTo { column_id: i64 },
}

/// A yes/no confirmation.
pub struct Confirm {
    pub message: String,
    pub purpose: ConfirmPurpose,
}

#[derive(Clone, Copy)]
pub enum ConfirmPurpose {
    DeleteCard(i64),
    DeleteColumn(i64),
    CancelRun(i64),
}

/// Mouse drag in progress.
pub struct DragState {
    pub kind: DragKind,
    pub from_col: usize,
    pub hover_col: usize,
}

#[derive(Clone, Copy)]
pub enum DragKind {
    Card { card_id: i64 },
    Column { column_id: i64 },
}

/// The whole TUI state.
pub struct App {
    pub board: BoardSnapshot,
    pub screen: Screen,
    pub sel_col: usize,
    pub sel_card: usize,
    pub card_filter: CardFilter,
    pub detail: Option<CardDetail>,
    /// Card detail opens as a contextual popup; users can expand it in place.
    pub detail_fullscreen: bool,
    pub detail_scroll_target: DetailScrollTarget,
    pub detail_comments_scroll: usize,
    pub detail_runs_scroll: usize,
    pub form: Option<Form>,
    /// Forms opened from card detail return there on save/cancel.
    pub form_from_detail: bool,
    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
    pub drag: Option<DragState>,
    pub toast: Option<Toast>,
    pub should_quit: bool,
    /// Injected clock (epoch seconds) for deterministic timer rendering.
    pub now: i64,
    /// Injected millisecond clock for double-click detection (0 in tests).
    pub now_ms: u128,
    /// Last full draw area, for mouse hit-testing.
    pub last_area: Rect,
    last_click: Option<(u16, u16, u128)>,
}

impl App {
    pub fn new(board: BoardSnapshot) -> App {
        App {
            board,
            screen: Screen::Board,
            sel_col: 0,
            sel_card: 0,
            card_filter: CardFilter::Active,
            detail: None,
            detail_fullscreen: false,
            detail_scroll_target: DetailScrollTarget::Comments,
            detail_comments_scroll: 0,
            detail_runs_scroll: 0,
            form: None,
            form_from_detail: false,
            picker: None,
            confirm: None,
            drag: None,
            toast: None,
            should_quit: false,
            now: 0,
            now_ms: 0,
            last_area: Rect::new(0, 0, 80, 24),
            last_click: None,
        }
    }

    pub fn replace_board(&mut self, board: BoardSnapshot) {
        self.board = board;
        self.screen = Screen::Board;
        self.sel_col = 0;
        self.sel_card = 0;
        self.detail = None;
        self.detail_fullscreen = false;
        self.detail_comments_scroll = 0;
        self.detail_runs_scroll = 0;
        self.form = None;
        self.form_from_detail = false;
        self.picker = None;
        self.confirm = None;
        self.drag = None;
    }

    // -- board queries -------------------------------------------------------

    pub fn col_id_at(&self, idx: usize) -> Option<i64> {
        self.board.columns.get(idx).map(|c| c.id)
    }

    /// Cards of a column, in board order.
    pub fn cards_of(&self, col_id: i64) -> Vec<&board_core::model::Card> {
        self.board
            .cards
            .iter()
            .filter(|c| c.column_id == col_id)
            .filter(|c| match self.card_filter {
                CardFilter::Active => c.archived_at.is_none(),
                CardFilter::All => true,
                CardFilter::Archived => c.archived_at.is_some(),
            })
            .collect()
    }

    pub fn selected_card_id(&self) -> Option<i64> {
        let col_id = self.col_id_at(self.sel_col)?;
        self.cards_of(col_id).get(self.sel_card).map(|c| c.id)
    }

    pub fn selected_card(&self) -> Option<&board_core::model::Card> {
        let col_id = self.col_id_at(self.sel_col)?;
        self.cards_of(col_id).get(self.sel_card).copied()
    }

    pub fn selected_card_status(&self) -> Option<CardStatus> {
        self.selected_card().map(|c| c.status)
    }

    /// A pristine board that a template could be applied onto.
    pub fn is_empty_board(&self) -> bool {
        self.board.cards.is_empty() && self.board.columns.len() == 1
    }

    pub fn set_toast(&mut self, text: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            is_error,
            at: self.now,
        });
    }

    /// Keep chronological order (oldest → newest) and open both histories at
    /// their bottom so the most recent item is always the last visible row.
    pub fn scroll_detail_to_latest(&mut self) {
        let Some(detail) = &self.detail else { return };
        let comments_total = detail.comments.len();
        let runs_total = detail.runs.len();
        let layout = crate::view::detail_layout(self, self.last_area);
        let comments_visible = layout.comments.height.saturating_sub(1) as usize;
        let runs_visible = layout.runs.height.saturating_sub(1) as usize;
        self.detail_comments_scroll = comments_total.saturating_sub(comments_visible.max(1));
        self.detail_runs_scroll = runs_total.saturating_sub(runs_visible.max(1));
    }

    fn toggle_detail_fullscreen(&mut self) {
        self.detail_fullscreen = !self.detail_fullscreen;
        self.scroll_detail_to_latest();
    }

    // -- navigation ----------------------------------------------------------

    fn clamp_card(&mut self) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            self.sel_card = 0;
        } else if self.sel_card >= len {
            self.sel_card = len - 1;
        }
    }

    fn move_col(&mut self, delta: isize) {
        let n = self.board.columns.len();
        if n == 0 {
            return;
        }
        self.sel_col = (self.sel_col as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_card();
    }

    fn move_card(&mut self, delta: isize) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        self.sel_card = (self.sel_card as isize + delta).rem_euclid(len as isize) as usize;
    }

    // -- drag helpers (also exercised directly by unit tests) ----------------

    pub fn begin_card_drag(&mut self, card_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Card { card_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn begin_column_drag(&mut self, column_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Column { column_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn drag_hover(&mut self, col: usize) {
        if let Some(d) = &mut self.drag {
            d.hover_col = col;
        }
    }

    /// Complete a drag, producing a move/reorder effect when it landed elsewhere.
    pub fn finish_drag(&mut self) -> Vec<Effect> {
        let Some(d) = self.drag.take() else {
            return vec![];
        };
        if d.hover_col == d.from_col {
            return vec![];
        }
        match d.kind {
            DragKind::Card { card_id } => match self.col_id_at(d.hover_col) {
                Some(column_id) => vec![Effect::CardMove(CardMoveParams {
                    id: card_id,
                    column_id,
                    position: None,
                })],
                None => vec![],
            },
            DragKind::Column { column_id } => vec![Effect::ColumnReorder {
                id: column_id,
                position: d.hover_col as i64,
            }],
        }
    }
}

/// The pure reducer. Mutates `app` and returns effects for the driver.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Refresh => vec![Effect::Refetch],
        Msg::Key(k) => on_key(app, k),
        Msg::Mouse(m) => on_mouse(app, m),
    }
}

fn on_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    match app.screen {
        Screen::Board => board_key(app, k),
        Screen::CardDetail => detail_key(app, k),
        Screen::CardForm | Screen::ColumnForm => form_key(app, k),
        Screen::Picker => picker_key(app, k),
        Screen::Confirm => confirm_key(app, k),
        Screen::Help => {
            app.screen = Screen::Board;
            vec![]
        }
    }
}

fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    match k.code {
        KeyCode::Left | KeyCode::Char('h') => app.move_col(-1),
        KeyCode::Right | KeyCode::Char('l') => app.move_col(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_card(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_card(1),
        KeyCode::Char('b') => return vec![Effect::LoadBoards],
        KeyCode::Char('n') => {
            if let Some(col_id) = app.col_id_at(app.sel_col) {
                app.form_from_detail = false;
                app.form = Some(Form::card_create(col_id));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('N') => {
            app.form_from_detail = false;
            app.form = Some(Form::column_create(&app.board.columns));
            app.screen = Screen::ColumnForm;
        }
        KeyCode::Char('e') => {
            if let Some(card) = app.selected_card().cloned() {
                app.form_from_detail = false;
                app.form = Some(Form::card_edit(&card));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('E') => {
            if let Some(col) = app.board.columns.get(app.sel_col).cloned() {
                app.form_from_detail = false;
                app.form = Some(Form::column_edit(&col, &app.board.columns));
                app.screen = Screen::ColumnForm;
            }
        }
        KeyCode::Char('a') => return archive_selected_card(app),
        KeyCode::Char('v') => {
            app.card_filter = app.card_filter.next();
            app.sel_card = 0;
            app.clamp_card();
            return vec![Effect::SetPaneTitle(app.card_filter)];
        }
        KeyCode::Char('d') => {
            if let Some(id) = app.selected_card_id() {
                app.confirm = Some(Confirm {
                    message: "Delete this card?".into(),
                    purpose: ConfirmPurpose::DeleteCard(id),
                });
                app.screen = Screen::Confirm;
            }
        }
        KeyCode::Char('D') => return delete_column(app),
        KeyCode::Char('m') => return open_move_picker(app),
        KeyCode::Char('H') => return shove_card(app, -1),
        KeyCode::Char('L') => return shove_card(app, 1),
        KeyCode::Enter => {
            if let Some(id) = app.selected_card_id() {
                app.detail_fullscreen = false;
                app.detail_scroll_target = DetailScrollTarget::Comments;
                app.detail_comments_scroll = 0;
                app.detail_runs_scroll = 0;
                app.screen = Screen::CardDetail;
                return vec![Effect::LoadDetail(id)];
            }
        }
        KeyCode::Char('T') => {
            if app.is_empty_board() {
                return vec![Effect::TemplateApply("pipeline".into())];
            }
            app.set_toast("template only applies to an empty board", true);
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.set_toast("refreshed", false);
            return vec![Effect::Refetch];
        }
        KeyCode::Char('?') => app.screen = Screen::Help,
        KeyCode::Char('q') | KeyCode::Esc => return vec![Effect::Quit],
        _ => {}
    }
    vec![]
}

fn archive_selected_card(app: &mut App) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_none()
        && matches!(
            card.status,
            CardStatus::Queued | CardStatus::Running | CardStatus::Blocked
        )
    {
        app.set_toast("card has an active run; cancel it before archiving", true);
        return vec![];
    }
    vec![Effect::CardArchive {
        id: card.id,
        archived: card.archived_at.is_none(),
    }]
}

fn delete_column(app: &mut App) -> Vec<Effect> {
    let Some(col_id) = app.col_id_at(app.sel_col) else {
        return vec![];
    };
    // Column deletion must account for cards hidden by the current archive
    // filter; the daemon still needs a destination for every persisted card.
    let has_cards = app.board.cards.iter().any(|card| card.column_id == col_id);
    if has_cards {
        // Ask where to move them (daemon still refuses if a card is running).
        let options: Vec<(String, i64)> = app
            .board
            .columns
            .iter()
            .filter(|c| c.id != col_id)
            .map(|c| (c.name.clone(), c.id))
            .collect();
        if options.is_empty() {
            app.set_toast("no other column to move cards to", true);
            return vec![];
        }
        app.picker = Some(Picker {
            title: "Move cards to which column?".into(),
            options,
            sel: 0,
            purpose: PickerPurpose::DeleteColumnMoveTo { column_id: col_id },
        });
        app.screen = Screen::Picker;
    } else {
        app.confirm = Some(Confirm {
            message: "Delete this column?".into(),
            purpose: ConfirmPurpose::DeleteColumn(col_id),
        });
        app.screen = Screen::Confirm;
    }
    vec![]
}

fn open_move_picker(app: &mut App) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_some() {
        app.set_toast("restore archived card before moving", true);
        return vec![];
    }
    let card_id = card.id;
    let cur = app.col_id_at(app.sel_col);
    let options: Vec<(String, i64)> = app
        .board
        .columns
        .iter()
        .filter(|c| Some(c.id) != cur)
        .map(|c| (c.name.clone(), c.id))
        .collect();
    if options.is_empty() {
        return vec![];
    }
    app.picker = Some(Picker {
        title: "Move card to which column?".into(),
        options,
        sel: 0,
        purpose: PickerPurpose::MoveCard { card_id },
    });
    app.screen = Screen::Picker;
    vec![]
}

fn shove_card(app: &mut App, delta: isize) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_some() {
        app.set_toast("restore archived card before moving", true);
        return vec![];
    }
    let card_id = card.id;
    let n = app.board.columns.len() as isize;
    if n == 0 {
        return vec![];
    }
    let target = (app.sel_col as isize + delta).rem_euclid(n) as usize;
    if target == app.sel_col {
        return vec![];
    }
    let Some(column_id) = app.col_id_at(target) else {
        return vec![];
    };
    app.sel_col = target;
    app.sel_card = 0;
    vec![Effect::CardMove(CardMoveParams {
        id: card_id,
        column_id,
        position: None,
    })]
}

fn detail_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let card_id = app.detail.as_ref().map(|d| d.card.id);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.screen = Screen::Board;
            app.detail = None;
            app.detail_fullscreen = false;
            app.detail_comments_scroll = 0;
            app.detail_runs_scroll = 0;
        }
        KeyCode::Char('f') => app.toggle_detail_fullscreen(),
        KeyCode::Tab => {
            app.detail_scroll_target = match app.detail_scroll_target {
                DetailScrollTarget::Comments => DetailScrollTarget::Runs,
                DetailScrollTarget::Runs => DetailScrollTarget::Comments,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => scroll_detail(app, -1),
        KeyCode::Down | KeyCode::Char('j') => scroll_detail(app, 1),
        KeyCode::Char('e') => {
            if let Some(card) = app.detail.as_ref().map(|d| d.card.clone()) {
                app.form_from_detail = true;
                app.form = Some(Form::card_edit(&card));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('a') => {
            let Some(card) = app.detail.as_ref().map(|detail| &detail.card) else {
                return vec![];
            };
            if card.archived_at.is_none()
                && matches!(
                    card.status,
                    CardStatus::Queued | CardStatus::Running | CardStatus::Blocked
                )
            {
                app.set_toast("card has an active run; cancel it before archiving", true);
            } else {
                return vec![Effect::CardArchive {
                    id: card.id,
                    archived: card.archived_at.is_none(),
                }];
            }
        }
        KeyCode::Char('c') => {
            if let Some(id) = card_id {
                app.form_from_detail = true;
                app.form = Some(Form::comment(id));
                app.screen = Screen::CardForm;
            }
        }
        KeyCode::Char('o') => {
            if let Some(id) = card_id {
                return vec![Effect::FocusRun(id)];
            }
        }
        KeyCode::Char('x') => {
            if let Some(id) = card_id {
                app.confirm = Some(Confirm {
                    message: "Cancel the running run?".into(),
                    purpose: ConfirmPurpose::CancelRun(id),
                });
                app.screen = Screen::Confirm;
            }
        }
        KeyCode::Char('r') => {
            if let Some(id) = card_id {
                return vec![Effect::RunRetry(id)];
            }
        }
        KeyCode::Char('?') => app.screen = Screen::Help,
        _ => {}
    }
    vec![]
}

fn scroll_detail(app: &mut App, delta: isize) {
    let Some(detail) = &app.detail else { return };
    let layout = crate::view::detail_layout(app, app.last_area);
    let (offset, total, visible) = match app.detail_scroll_target {
        DetailScrollTarget::Comments => (
            &mut app.detail_comments_scroll,
            detail.comments.len(),
            layout.comments.height.saturating_sub(1) as usize,
        ),
        DetailScrollTarget::Runs => (
            &mut app.detail_runs_scroll,
            detail.runs.len(),
            layout.runs.height.saturating_sub(1) as usize,
        ),
    };
    let max = total.saturating_sub(visible.max(1));
    *offset = (*offset as isize + delta).clamp(0, max as isize) as usize;
}

fn form_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if app.form.is_none() {
        app.screen = Screen::Board;
        return vec![];
    }

    // Ctrl+E: hand a multiline text field to $EDITOR.
    if k.code == KeyCode::Char('e') && k.modifiers.contains(KeyModifiers::CONTROL) {
        let multiline = app
            .form
            .as_ref()
            .map(|f| f.focused_is_multiline())
            .unwrap_or(false);
        return if multiline {
            vec![Effect::EditFocusedTextArea]
        } else {
            vec![]
        };
    }

    match k.code {
        KeyCode::Esc => close_form(app, false),
        KeyCode::Enter => return submit_form(app),
        KeyCode::Tab => app.form.as_mut().unwrap().focus_step(1),
        KeyCode::BackTab => app.form.as_mut().unwrap().focus_step(-1),
        _ => {
            let form = app.form.as_mut().unwrap();
            if form.focused_is_choice() {
                let delta = match k.code {
                    KeyCode::Left | KeyCode::Up => Some(-1),
                    KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => Some(1),
                    _ => None,
                };
                if let Some(delta) = delta {
                    let fid = form.focused().id;
                    form.focused_mut().cycle(delta);
                    // A changed harness needs fresh capabilities; a changed
                    // session needs its own workspace list; a changed column
                    // harness-override needs its own capabilities; model/space-
                    // kind changes reshape the dependent selectors in place.
                    match fid {
                        FieldId::Harness | FieldId::Session | FieldId::HarnessOverride => {
                            return vec![Effect::LoadFormOptions]
                        }
                        FieldId::Model => form.on_model_changed(),
                        FieldId::SpaceKind => form.on_space_kind_changed(),
                        _ => {}
                    }
                }
            } else if let FieldKind::Text(ta) = &mut form.focused_mut().kind {
                // Enter/Tab/Esc are handled above; everything else is editing.
                ta.input(k);
            }
        }
    }
    vec![]
}

fn submit_form(app: &mut App) -> Vec<Effect> {
    let Some(form) = app.form.as_ref() else {
        return vec![];
    };
    match form.submit() {
        Ok(submit) => {
            let board_id = app.board.board.id;
            let effects = match submit {
                Submit::CardCreate(mut p) => {
                    p.board_id = Some(board_id);
                    vec![Effect::CardCreate(p)]
                }
                Submit::CardUpdate(p) => vec![Effect::CardUpdate(p)],
                Submit::ColumnCreate(mut p) => {
                    p.board_id = Some(board_id);
                    vec![Effect::ColumnCreate(p)]
                }
                Submit::ColumnUpdate(p) => vec![Effect::ColumnUpdate(p)],
                Submit::Comment { card_id, body } => vec![Effect::CommentAdd { card_id, body }],
            };
            close_form(app, true);
            effects
        }
        Err(msg) => {
            app.set_toast(msg, true);
            vec![]
        }
    }
}

fn close_form(app: &mut App, _submitted: bool) {
    let back_to_detail = app.form_from_detail;
    app.form = None;
    app.form_from_detail = false;
    app.screen = if back_to_detail {
        Screen::CardDetail
    } else {
        Screen::Board
    };
}

fn picker_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(picker) = app.picker.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if picker.sel > 0 {
                picker.sel -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if picker.sel + 1 < picker.options.len() {
                picker.sel += 1;
            }
        }
        KeyCode::Enter => {
            let (_, target) = picker.options[picker.sel];
            let purpose = picker.purpose;
            app.picker = None;
            app.screen = Screen::Board;
            return match purpose {
                PickerPurpose::SwitchBoard => vec![Effect::SwitchBoard(target)],
                PickerPurpose::MoveCard { card_id } => vec![Effect::CardMove(CardMoveParams {
                    id: card_id,
                    column_id: target,
                    position: None,
                })],
                PickerPurpose::DeleteColumnMoveTo { column_id } => vec![Effect::ColumnDelete {
                    id: column_id,
                    move_cards_to: Some(target),
                }],
            };
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.picker = None;
            app.screen = Screen::Board;
        }
        _ => {}
    }
    vec![]
}

fn confirm_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(confirm) = app.confirm.as_ref() else {
        app.screen = Screen::Board;
        return vec![];
    };
    match k.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let purpose = confirm.purpose;
            app.confirm = None;
            match purpose {
                ConfirmPurpose::DeleteCard(id) => {
                    app.screen = Screen::Board;
                    vec![Effect::CardDelete(id)]
                }
                ConfirmPurpose::DeleteColumn(id) => {
                    app.screen = Screen::Board;
                    vec![Effect::ColumnDelete {
                        id,
                        move_cards_to: None,
                    }]
                }
                ConfirmPurpose::CancelRun(id) => {
                    app.screen = Screen::CardDetail;
                    vec![Effect::RunCancel(id)]
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            let back_detail = matches!(confirm.purpose, ConfirmPurpose::CancelRun(_));
            app.confirm = None;
            app.screen = if back_detail {
                Screen::CardDetail
            } else {
                Screen::Board
            };
            vec![]
        }
        _ => vec![],
    }
}

// -- mouse -------------------------------------------------------------------

fn on_mouse(app: &mut App, m: MouseEvent) -> Vec<Effect> {
    if app.screen == Screen::CardDetail {
        let detail_layout = crate::view::detail_layout(app, app.last_area);
        let in_rect = |rect: Rect| {
            m.column >= rect.x
                && m.column < rect.x.saturating_add(rect.width)
                && m.row >= rect.y
                && m.row < rect.y.saturating_add(rect.height)
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(crate::view::detail_toggle_rect(app, app.last_area)) {
                    app.toggle_detail_fullscreen();
                } else if in_rect(detail_layout.comments) {
                    app.detail_scroll_target = DetailScrollTarget::Comments;
                } else if in_rect(detail_layout.runs) {
                    app.detail_scroll_target = DetailScrollTarget::Runs;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if in_rect(detail_layout.comments) {
                    app.detail_scroll_target = DetailScrollTarget::Comments;
                } else if in_rect(detail_layout.runs) {
                    app.detail_scroll_target = DetailScrollTarget::Runs;
                } else {
                    return vec![];
                }
                scroll_detail(
                    app,
                    if matches!(m.kind, MouseEventKind::ScrollDown) {
                        1
                    } else {
                        -1
                    },
                );
            }
            _ => {}
        }
        return vec![];
    }
    if app.screen != Screen::Board {
        return vec![];
    }
    let layout = crate::view::board_layout(app, app.last_area);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((col_idx, card_idx)) = layout.hit_card(m.column, m.row) {
                app.sel_col = col_idx;
                app.sel_card = card_idx;
                // double-click → open detail
                let dbl = app
                    .last_click
                    .map(|(x, y, t)| {
                        x == m.column && y == m.row && app.now_ms.saturating_sub(t) < 400
                    })
                    .unwrap_or(false);
                app.last_click = Some((m.column, m.row, app.now_ms));
                if dbl {
                    if let Some(id) = app.selected_card_id() {
                        app.detail_fullscreen = false;
                        app.detail_scroll_target = DetailScrollTarget::Comments;
                        app.detail_comments_scroll = 0;
                        app.detail_runs_scroll = 0;
                        app.screen = Screen::CardDetail;
                        return vec![Effect::LoadDetail(id)];
                    }
                }
                if let Some(card) = app.selected_card() {
                    if card.archived_at.is_some() {
                        app.set_toast("restore archived card before moving", true);
                    } else {
                        app.begin_card_drag(card.id, col_idx);
                    }
                }
            } else if let Some(col_idx) = layout.hit_header(m.column, m.row) {
                app.sel_col = col_idx;
                app.clamp_card();
                if let Some(id) = app.col_id_at(col_idx) {
                    app.begin_column_drag(id, col_idx);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(col_idx) = layout.hit_any_column(m.column) {
                app.drag_hover(col_idx);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(col_idx) = layout.hit_any_column(m.column) {
                app.drag_hover(col_idx);
            }
            return app.finish_drag();
        }
        MouseEventKind::ScrollDown => app.move_card(1),
        MouseEventKind::ScrollUp => app.move_card(-1),
        _ => {}
    }
    vec![]
}

/// Post-mutation helper: after the board is refetched the selection may point
/// past the end of a shrunk column; clamp it. Also used by the driver.
pub fn clamp_selection(app: &mut App) {
    if app.sel_col >= app.board.columns.len() {
        app.sel_col = app.board.columns.len().saturating_sub(1);
    }
    app.clamp_card();
}
