//! The effect executor: owns the client + `$EDITOR` launcher and applies the
//! [`Effect`](crate::app::Effect)s `app::update` produces.
//!
//! Kept separate from [`crate::runtime`] (the terminal loop) so tests can drive
//! it against a `FakeBoardClient` and a fake editor with no real terminal.
//!
//! - [`dispatch`] holds the `Effect` match and its mutate/refresh policy.
//! - [`load`] holds every read path: refetch, board/column option lists,
//!   card detail, and form catalog metadata.

mod dispatch;
mod load;

use anyhow::Result;
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{BoardSnapshot, Event};
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};

use crate::app::{update, App, CardFilter, Msg};
use crate::editor::{EditorLauncher, RealEditor};
use crate::OriginContext;

/// Owns the client + editor and applies [`Effect`](crate::app::Effect)s
/// produced by `update`.
pub struct Driver {
    pub app: App,
    client: Box<dyn BoardClient>,
    editor: Box<dyn EditorLauncher>,
    /// The invoking Herdr/plugin context: which session socket to name when an
    /// effect needs one (`run.focus`, `pane.set_title`) and whether this
    /// process is actually the `herdr-board` plugin pane.
    pub(crate) origin: OriginContext,
    /// Bug A: set when the terminal contents were clobbered outside
    /// ratatui's diff (an `$EDITOR` round-trip) or the size changed; the next
    /// `event_loop` iteration calls `terminal.clear()` before drawing so every
    /// cell is repainted, then resets this.
    needs_full_redraw: bool,
}

impl Driver {
    /// Build a driver, fetching the initial board.
    pub fn new(client: Box<dyn BoardClient>) -> Result<Driver> {
        Driver::with_editor_and_origin(
            client,
            Box::new(RealEditor),
            OriginContext::from_environment(),
        )
    }

    pub fn with_editor(
        client: Box<dyn BoardClient>,
        editor: Box<dyn EditorLauncher>,
    ) -> Result<Driver> {
        Driver::with_editor_and_origin(client, editor, OriginContext::default())
    }

    pub fn with_editor_and_origin(
        mut client: Box<dyn BoardClient>,
        editor: Box<dyn EditorLauncher>,
        origin: OriginContext,
    ) -> Result<Driver> {
        let board = client.board_get()?;
        Driver::with_editor_and_board_and_origin(client, editor, board, origin)
    }

    pub fn with_editor_and_board(
        client: Box<dyn BoardClient>,
        editor: Box<dyn EditorLauncher>,
        board: BoardSnapshot,
    ) -> Result<Driver> {
        Driver::with_editor_and_board_and_origin(client, editor, board, OriginContext::default())
    }

    pub fn with_editor_and_board_and_origin(
        client: Box<dyn BoardClient>,
        editor: Box<dyn EditorLauncher>,
        board: BoardSnapshot,
        origin: OriginContext,
    ) -> Result<Driver> {
        let mut driver = Driver {
            app: App::with_origin_context(board, origin.clone()),
            client,
            editor,
            origin,
            needs_full_redraw: false,
        };
        driver.set_pane_title(CardFilter::Active);
        Ok(driver)
    }

    /// Override the invoking Herdr socket (deterministic tests/embedders).
    pub fn set_origin_socket(&mut self, socket: Option<String>) {
        self.origin.origin_socket = socket.clone();
        self.origin.session = board_core::paths::session_name_from_socket(socket.as_deref());
        self.app.origin_context = self.origin.clone();
    }

    /// Feed one synthetic message: run the reducer, then apply its effects.
    pub fn handle(&mut self, msg: Msg) {
        for eff in update(&mut self.app, msg) {
            self.dispatch(eff);
        }
    }

    /// Whether the next draw needs a full `terminal.clear()` first (Bug A: an
    /// `$EDITOR` round-trip or a terminal-size change). Exposed read-only so
    /// tests/embedders can observe it without consuming it.
    pub fn needs_full_redraw(&self) -> bool {
        self.needs_full_redraw
    }

    /// Consume the full-redraw flag, resetting it — the same
    /// check-then-clear-then-reset step `event_loop` performs before every
    /// draw. Exposed so tests can simulate that step without a real terminal.
    pub fn take_needs_full_redraw(&mut self) -> bool {
        std::mem::take(&mut self.needs_full_redraw)
    }

    /// Sync the driver to the frame area the next draw will use, setting the
    /// full-redraw flag when it differs from the last one (Bug A: shrinking
    /// then growing a terminal can leave stale cells behind). Called by
    /// `event_loop` every iteration; exposed so tests can simulate a resize
    /// without a real terminal.
    pub fn sync_frame_area(&mut self, area: Rect) {
        if area != self.app.last_area {
            self.needs_full_redraw = true;
        }
        self.app.last_area = area;
    }

    /// Subscribe to the daemon's board events, for the runtime's redraw-ping
    /// thread. Empty/unsupported transports (e.g. `FakeBoardClient`) simply
    /// error and the loop falls back to action-driven refetch.
    pub(crate) fn subscribe(&mut self) -> Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.client.subscribe()
    }

    /// The local transport endpoint that can replace this client's stale
    /// request connection after the daemon restarts.
    pub(crate) fn reconnect_path(&self) -> Option<PathBuf> {
        self.client.reconnect_path()
    }

    /// Replace the stale request connection before the runtime asks for a
    /// full snapshot. Keeping this on the driver ensures later user actions
    /// also use the replacement daemon, not only the first recovery refetch.
    pub(crate) fn reconnect(&mut self, path: &Path) -> bool {
        match UnixClient::connect(path) {
            Ok(client) => {
                self.client = Box::new(client);
                true
            }
            Err(error) => {
                self.app.set_toast(error.to_string(), true);
                false
            }
        }
    }

    /// Turn a client error into a toast and swallow it: no client failure is
    /// fatal to the TUI. Every effect that calls out goes through here.
    fn guard<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.app.set_toast(e.to_string(), true);
                None
            }
        }
    }

    fn edit_focused(&mut self) {
        let Some(form) = self.app.form.as_ref() else {
            return;
        };
        let initial = form.focused().get_text();
        match self.editor.edit(&initial) {
            Ok(result) => {
                if let Some(form) = self.app.form.as_mut() {
                    form.focused_mut().set_text(&result.text);
                }
                if result.needs_full_redraw {
                    self.needs_full_redraw = true;
                }
            }
            Err(e) => self.app.set_toast(e.to_string(), true),
        }
    }

    pub(crate) fn expire_toast(&mut self) {
        if let Some(t) = &self.app.toast {
            if self.app.now - t.at > 4 {
                self.app.toast = None;
            }
        }
    }
}
