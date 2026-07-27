//! Herdr pane-border title sync, through the daemon's `pane.set_title` RPC.
//!
//! The TUI owns no Herdr connection of its own (see `AGENTS.md`), so the only
//! observable effect of `Effect::SetPaneTitle` is that exact client call — and
//! it must stay a silent no-op everywhere the TUI is not the `herdr-board`
//! plugin pane.

use std::sync::{Arc, Mutex};

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::Event;
use board_tui::app::CardFilter;
use board_tui::testkit::{driver_with_origin, key};
use board_tui::view::pane_title;
use board_tui::{Driver, OriginContext};
use crossterm::event::KeyCode;
use serde_json::Value;

const EDITED: &str = "x";

/// Records every `pane.set_title` request and can make it fail, so a test can
/// prove the TUI treats a failed rename as harmless.
struct TitleClient {
    inner: FakeBoardClient,
    titles: Arc<Mutex<Vec<Value>>>,
    fail: bool,
}

impl TitleClient {
    fn new(fail: bool) -> (TitleClient, Arc<Mutex<Vec<Value>>>) {
        let titles = Arc::new(Mutex::new(Vec::new()));
        let client = TitleClient {
            inner: FakeBoardClient::new().unwrap(),
            titles: Arc::clone(&titles),
            fail,
        };
        (client, titles)
    }
}

impl BoardClient for TitleClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == "pane.set_title" {
            self.titles.lock().unwrap().push(params.clone());
            if self.fail {
                anyhow::bail!("herdr unavailable: pane.rename w1:p1: pane not found");
            }
        }
        self.inner.call(method, params)
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        self.inner.subscribe()
    }
}

/// The context a real `herdr-board` plugin pane is launched with.
fn plugin_origin() -> OriginContext {
    OriginContext {
        origin_socket: Some("/run/herdr/sessions/work/herdr.sock".into()),
        session: Some("work".into()),
        plugin_id: Some("herdr-board".into()),
        pane_id: Some("w1:p1".into()),
    }
}

fn titles(recorded: &Arc<Mutex<Vec<Value>>>) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The current toast text, if any (`Toast` itself is not `Debug`).
fn toast(d: &Driver) -> Option<String> {
    d.app.toast.as_ref().map(|t| t.text.clone())
}

#[test]
fn a_plugin_pane_sets_its_title_through_the_daemon_and_tracks_the_filter() {
    let (client, recorded) = TitleClient::new(false);
    let mut d = driver_with_origin(client, EDITED, plugin_origin());

    // Building the driver syncs the border once, before any input.
    let active = pane_title(&d.app.board.board, CardFilter::Active);
    assert_eq!(
        titles(&recorded),
        vec![serde_json::json!({
            "pane_id": "w1:p1",
            "title": active,
            "origin_socket": "/run/herdr/sessions/work/herdr.sock",
        })],
    );

    // `v` cycles the archive filter, and the border follows it.
    d.handle(key(KeyCode::Char('v')));
    let all = pane_title(&d.app.board.board, CardFilter::All);
    assert_ne!(all, active, "the filter must be visible in the title");
    let sent = titles(&recorded);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["title"], Value::String(all));
    assert_eq!(sent[1]["pane_id"], "w1:p1");
}

#[test]
fn a_failed_rename_is_swallowed_and_never_toasts_over_the_board() {
    let (client, recorded) = TitleClient::new(true);
    let mut d = driver_with_origin(client, EDITED, plugin_origin());

    assert_eq!(titles(&recorded).len(), 1);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));

    d.handle(key(KeyCode::Char('v')));
    assert_eq!(titles(&recorded).len(), 2);
    assert_eq!(d.app.card_filter, CardFilter::All);
    assert!(toast(&d).is_none(), "toast: {:?}", toast(&d));
}

#[test]
fn outside_a_herdr_board_plugin_pane_no_title_request_is_ever_sent() {
    // Every way the guard can fail: not the plugin at all, the plugin without a
    // pane id, and the plugin without an invoking Herdr socket to rename in.
    let cases = [
        ("default (standalone TUI, tests)", OriginContext::default()),
        (
            "another plugin",
            OriginContext {
                plugin_id: Some("herdr-file-viewer".into()),
                ..plugin_origin()
            },
        ),
        (
            "no pane id",
            OriginContext {
                pane_id: None,
                ..plugin_origin()
            },
        ),
        (
            "no origin socket",
            OriginContext {
                origin_socket: None,
                ..plugin_origin()
            },
        ),
    ];

    for (case, origin) in cases {
        let (client, recorded) = TitleClient::new(false);
        let mut d = driver_with_origin(client, EDITED, origin);
        d.handle(key(KeyCode::Char('v')));
        assert_eq!(d.app.card_filter, CardFilter::All, "{case}");
        assert!(
            titles(&recorded).is_empty(),
            "{case}: {:?}",
            titles(&recorded)
        );
    }
}
