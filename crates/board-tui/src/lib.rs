//! board-tui — ratatui kanban app (OWNED BY PHASE C).
//!
//! The full kanban TUI over a [`BoardClient`]. Phase D's `board tui` calls
//! [`run`]; tests drive the same [`Driver`] with synthetic events and a
//! `TestBackend`.
//!
//! Design: a pure state machine (`app::update`) + a pure renderer (`view::view`),
//! with all I/O (client calls, `$EDITOR`, terminal) confined to the [`Driver`]
//! and [`run`]. See `docs/design.md` §4.

pub mod app;
pub mod editor;
pub mod forms;
#[cfg(feature = "fake-client")]
pub mod testkit;
pub mod view;

use std::io::Stdout;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use board_core::capability::HarnessCapabilities;
use board_core::client::BoardClient;
use board_core::protocol::{Event, SessionInfo, SessionListResult, SpaceInfo, SpaceListResult};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::{clamp_selection, update, App, Effect, Msg};
use crate::editor::{EditorLauncher, RealEditor};
use crate::view::view;

/// Owns the client + editor and applies [`Effect`]s produced by `update`.
///
/// Kept separate from [`run`] (the terminal loop) so tests can drive it against
/// a `FakeBoardClient` and a fake editor with no real terminal.
pub struct Driver {
    pub app: App,
    client: Box<dyn BoardClient>,
    editor: Box<dyn EditorLauncher>,
}

impl Driver {
    /// Build a driver, fetching the initial board.
    pub fn new(client: Box<dyn BoardClient>) -> Result<Driver> {
        Driver::with_editor(client, Box::new(RealEditor))
    }

    pub fn with_editor(
        mut client: Box<dyn BoardClient>,
        editor: Box<dyn EditorLauncher>,
    ) -> Result<Driver> {
        let board = client.board_get()?;
        Ok(Driver {
            app: App::new(board),
            client,
            editor,
        })
    }

    /// Feed one synthetic message: run the reducer, then apply its effects.
    pub fn handle(&mut self, msg: Msg) {
        for eff in update(&mut self.app, msg) {
            self.dispatch(eff);
        }
    }

    fn guard<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.app.set_toast(e.to_string(), true);
                None
            }
        }
    }

    fn refetch(&mut self) {
        let r = self.client.board_get();
        if let Some(snap) = self.guard(r) {
            self.app.board = snap;
            clamp_selection(&mut self.app);
        }
    }

    fn load_detail(&mut self, id: i64) {
        let r = self.client.card_get(id);
        if let Some(detail) = self.guard(r) {
            self.app.detail = Some(detail);
        }
    }

    fn edit_focused(&mut self) {
        let Some(form) = self.app.form.as_ref() else {
            return;
        };
        let initial = form.focused().get_text();
        match self.editor.edit(&initial) {
            Ok(edited) => {
                if let Some(form) = self.app.form.as_mut() {
                    form.focused_mut().set_text(&edited);
                }
            }
            Err(e) => self.app.set_toast(e.to_string(), true),
        }
    }

    fn dispatch(&mut self, eff: Effect) {
        match eff {
            Effect::Refetch => self.refetch(),
            Effect::LoadDetail(id) => self.load_detail(id),
            Effect::CardCreate(p) => {
                let r = self.client.card_create(&p);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::CardUpdate(p) => {
                let r = self.client.card_update(&p);
                if self.guard(r).is_some() {
                    self.refetch();
                    self.reload_open_detail();
                }
            }
            Effect::CardDelete(id) => {
                let r = self.client.card_delete(id);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::CardMove(p) => {
                let r = self.client.card_move(&p);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::ColumnCreate(p) => {
                let r = self.client.column_create(&p);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::ColumnUpdate(p) => {
                let r = self.client.column_update(&p);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::ColumnReorder { id, position } => {
                let r = self.client.column_reorder(id, position);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::ColumnDelete { id, move_cards_to } => {
                let r = self.client.column_delete(id, move_cards_to);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::CommentAdd { card_id, body } => {
                let r = self.client.comment_add(card_id, &body, None);
                if self.guard(r).is_some() {
                    self.reload_open_detail();
                    self.refetch();
                }
            }
            Effect::TemplateApply(name) => {
                let r = self.client.template_apply(&name);
                if self.guard(r).is_some() {
                    self.refetch();
                }
            }
            Effect::RunCancel(id) => {
                let r = self.client.run_cancel(id);
                if self.guard(r).is_some() {
                    self.load_detail(id);
                    self.refetch();
                }
            }
            Effect::RunRetry(id) => {
                let r = self.client.run_retry(id);
                if self.guard(r).is_some() {
                    self.load_detail(id);
                    self.refetch();
                }
            }
            Effect::RunDone(id, outcome) => {
                let r = self.client.run_done(id, outcome, None);
                if self.guard(r).is_some() {
                    self.load_detail(id);
                    self.refetch();
                }
            }
            Effect::EditFocusedTextArea => self.edit_focused(),
            Effect::LoadFormOptions => self.load_form_options(),
            Effect::Quit => self.app.should_quit = true,
        }
    }

    /// Fetch the capability catalog + workspace list for the open card form and
    /// hand them to the form. A failed fetch is non-fatal: the affected
    /// selectors fall back to free-text and the user gets a status-line warning.
    fn load_form_options(&mut self) {
        let Some(form) = self.app.form.as_ref() else {
            return;
        };
        let harness = form.current_harness();
        // The workspace list is scoped to the currently selected session.
        let session = form.current_session();
        let caps = fetch_capabilities(self.client.as_mut(), &harness);
        let sessions = fetch_sessions(self.client.as_mut());
        let spaces = fetch_spaces(self.client.as_mut(), session.as_deref());

        let mut warning: Option<String> = None;
        let caps_opt = match caps {
            Ok(c) => Some(c),
            Err(e) => {
                warning = Some(format!("capabilities unavailable ({e}); free-text"));
                None
            }
        };
        let spaces_opt = match spaces {
            Ok(s) => Some(s),
            Err(e) => {
                if warning.is_none() {
                    warning = Some(format!("spaces unavailable ({e}); free-text"));
                }
                None
            }
        };
        // Sessions failing is non-fatal: keep `(default)` + any preselection.
        let sessions_opt = sessions.ok();
        if let Some(form) = self.app.form.as_mut() {
            form.apply_options(caps_opt, spaces_opt, sessions_opt);
        }
        if let Some(w) = warning {
            self.app.set_toast(w, true);
        }
    }

    fn reload_open_detail(&mut self) {
        if let Some(id) = self.app.detail.as_ref().map(|d| d.card.id) {
            self.load_detail(id);
        }
    }

    fn expire_toast(&mut self) {
        if let Some(t) = &self.app.toast {
            if self.app.now - t.at > 4 {
                self.app.toast = None;
            }
        }
    }
}

/// Fetch `harness.capabilities` for `harness` via the client's generic `call`
/// (works over the real socket; the fake testkit client stubs it).
fn fetch_capabilities(client: &mut dyn BoardClient, harness: &str) -> Result<HarnessCapabilities> {
    let v = client.call(
        "harness.capabilities",
        serde_json::json!({ "harness": harness }),
    )?;
    Ok(serde_json::from_value(v)?)
}

/// Fetch `space.list` (scoped to `session`, `None` = default) via the client's
/// generic `call`.
fn fetch_spaces(client: &mut dyn BoardClient, session: Option<&str>) -> Result<Vec<SpaceInfo>> {
    let v = client.call("space.list", serde_json::json!({ "session": session }))?;
    let r: SpaceListResult = serde_json::from_value(v)?;
    Ok(r.spaces)
}

/// Fetch `session.list` via the client's generic `call`.
fn fetch_sessions(client: &mut dyn BoardClient) -> Result<Vec<SessionInfo>> {
    let v = client.call("session.list", serde_json::json!({}))?;
    let r: SessionListResult = serde_json::from_value(v)?;
    Ok(r.sessions)
}

fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The `board tui` entry point: set up the terminal, spawn an event-subscription
/// thread, and run the draw/input loop until quit.
pub fn run(client: Box<dyn BoardClient>) -> Result<()> {
    let mut driver = Driver::new(client)?;

    // Live updates: a background thread turns board events into redraw pings.
    // Falls back silently to action-driven refetch when subscribe is empty /
    // unsupported (e.g. FakeBoardClient).
    let (tx, rx) = mpsc::channel::<()>();
    if let Ok(stream) = driver.client.subscribe() {
        std::thread::spawn(move || {
            let stream: Box<dyn Iterator<Item = Event> + Send> = stream;
            for _ev in stream {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut driver, &mut terminal, &rx);

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    driver: &mut Driver,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: &mpsc::Receiver<()>,
) -> Result<()> {
    loop {
        driver.app.now = epoch_secs();
        driver.app.now_ms = epoch_millis();
        driver.expire_toast();

        let size = terminal.size()?;
        driver.app.last_area = Rect::new(0, 0, size.width, size.height);
        terminal.draw(|f| view(&driver.app, f))?;

        if crossterm::event::poll(Duration::from_millis(200))? {
            match crossterm::event::read()? {
                CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
                    driver.handle(Msg::Key(k));
                }
                CtEvent::Mouse(m) => driver.handle(Msg::Mouse(m)),
                _ => {}
            }
        } else {
            // Drain any pending redraw pings.
            let mut refreshed = false;
            while rx.try_recv().is_ok() {
                refreshed = true;
            }
            if refreshed {
                driver.handle(Msg::Refresh);
            }
        }

        if driver.app.should_quit {
            return Ok(());
        }
    }
}
