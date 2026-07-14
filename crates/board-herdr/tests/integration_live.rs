//! Read-only integration tests against a *live* herdr socket.
//!
//! `#[ignore]` by default. Run with a real herdr running:
//!   cargo test -p board-herdr -- --ignored
//! They also self-skip (pass trivially) if no socket is present, so the
//! ignored run is safe on machines without herdr.

use board_herdr::{default_socket_path, HerdrClient, ReadSource};

fn client_or_skip() -> Option<HerdrClient> {
    let path = default_socket_path();
    if !path.exists() {
        eprintln!("no herdr socket at {}; skipping", path.display());
        return None;
    }
    match HerdrClient::connect(&path) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("could not connect to herdr: {e}; skipping");
            None
        }
    }
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_ping() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    let pong = c.ping().expect("ping");
    assert!(!pong.version.is_empty());
    assert!(pong.protocol > 0);
    assert!(c.is_live());
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_workspace_list() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    // Must not error; contents depend on the running session.
    let workspaces = c.workspace_list().expect("workspace.list");
    eprintln!("live workspaces: {}", workspaces.len());
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_session_snapshot() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    let snap = c.session_snapshot().expect("session.snapshot");
    assert!(!snap.version.is_empty());
    assert!(snap.protocol > 0);
    eprintln!(
        "live snapshot: {} workspaces, {} panes, {} agents",
        snap.workspaces.len(),
        snap.panes.len(),
        snap.agents.len()
    );
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_tab_list() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    let tabs = c.tab_list(None).expect("tab.list");
    eprintln!("live tabs: {}", tabs.len());
    for t in &tabs {
        assert!(!t.tab_id.is_empty());
    }
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_pane_layout() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    // `None` = focused tab's layout.
    let layout = c.pane_layout(None).expect("pane.layout");
    eprintln!(
        "live layout: {} panes, {} splits, focused={}",
        layout.panes.len(),
        layout.splits.len(),
        layout.focused_pane_id
    );
    // The focused pane id, when present, should appear among the panes.
    if !layout.focused_pane_id.is_empty() {
        assert!(layout
            .panes
            .iter()
            .any(|p| p.pane_id == layout.focused_pane_id));
    }
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_pane_list_and_read() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    let panes = c.pane_list(None).expect("pane.list");
    if let Some(p) = panes.first() {
        let read = c
            .pane_read(&p.pane_id, ReadSource::Recent, Some(5))
            .expect("pane.read");
        assert_eq!(read.pane_id, p.pane_id);
    }
}
