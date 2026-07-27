//! Shared helpers for update integration tests.

use board_core::capability::{HarnessCapabilities, ModelInfo};
use board_core::client::BoardClient;
use board_core::protocol::{CardStatus, Effort, Event};
use board_tui::app::{App, Screen};
pub use board_tui::testkit::{
    choice_labels as opt_labels, demo_client, is_choice, key, mouse, rendered_rows, set_choice,
};
use board_tui::Driver;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// This suite's canned `$EDITOR` text — never asserted on here (see
/// `update/editor.rs`, which picks its own per test).
const EDITED: &str = "x";

pub fn demo_app() -> App {
    let mut c = demo_client().unwrap();
    App::new(c.board_get().unwrap())
}

pub fn driver_of<C: BoardClient + 'static>(client: C) -> Driver {
    board_tui::testkit::driver_with_editor(client, EDITED)
}

pub struct RecordingClient<C> {
    pub inner: C,
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl<C: BoardClient> BoardClient for RecordingClient<C> {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(method.to_string());
        self.inner.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

/// A two-model catalog where the models carry *different* effort sets, so tests
/// can observe the effort menu tracking the selected model.
pub fn split_effort_caps() -> HarnessCapabilities {
    HarnessCapabilities {
        harness: "claude".to_string(),
        models: vec![
            ModelInfo {
                id: "opus".to_string(),
                efforts: vec![Effort::Low, Effort::High],
            },
            ModelInfo {
                id: "haiku".to_string(),
                efforts: vec![Effort::Medium],
            },
        ],
        model_freeform: true,
        default_efforts: vec![Effort::Low, Effort::Medium, Effort::High],
        permission_modes: vec!["manual".to_string()],
        resume: Default::default(),
    }
}

/// Open the detail of the first card matching `status` in a fresh demo app.
pub fn demo_app_with_detail(status: CardStatus) -> App {
    let mut client = demo_client().unwrap();
    let board = client.board_get().unwrap();
    let card = board
        .cards
        .iter()
        .find(|c| c.status == status)
        .unwrap_or_else(|| panic!("no demo card with status {}", status.as_str()))
        .clone();
    let detail = client.card_get(card.id).unwrap();
    let mut app = App::new(board);
    app.screen = Screen::CardDetail;
    app.detail = Some(detail);
    app
}
