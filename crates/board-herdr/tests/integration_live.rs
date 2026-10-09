//! Read-only integration tests against a *live* herdr socket.
//!
//! `#[ignore]` by default. Run with Herdr 0.9.0 / protocol 22 running:
//!   cargo test -p board-herdr -- --ignored
//! They self-skip (pass trivially) if no compatible socket is present, so the
//! ignored run is safe on machines without the supported Herdr.
//!
//! These tests assert the live-read contract only (calls succeed, ids are
//! non-empty, focused panes resolve). Deterministic wire-shape coverage lives
//! in the fake-socket suite (`tests/socket.rs`: `call_happy_path_ping_and_
//! workspace_list`, `tab_list_parses_live_payload`,
//! `pane_layout_parses_live_payload`, `error_response_maps_to_protocol_error`).

use board_herdr::{default_socket_path, HerdrClient, ReadSource, SUPPORTED_HERDR_PROTOCOL};

/// Connect to the live socket, or return `None` (skip the calling test).
///
/// Skip reasons are printed with a `SKIP` prefix so `-- --ignored --nocapture`
/// shows *why* the test did nothing; each reason is distinct:
/// - absent socket file → no herdr running;
/// - connect failure → socket present but unreachable;
/// - protocol mismatch → herdr running but on an unsupported contract
///   (a contract failure for version-gated runs, a skip here).
///
/// A successful gate prints a `LIVE` line with the negotiated version so the
/// prerequisite status is visible even when the test later skips on empty
/// session state.
fn client_or_skip() -> Option<HerdrClient> {
    let path = default_socket_path();
    if !path.exists() {
        eprintln!(
            "SKIP: no herdr socket at {}; herdr not running",
            path.display()
        );
        return None;
    }
    let mut client = match HerdrClient::connect(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "SKIP: socket present at {} but unreachable: {e}",
                path.display()
            );
            return None;
        }
    };
    match client.require_supported_protocol() {
        Ok(pong) => {
            eprintln!(
                "LIVE: herdr {} speaking protocol {} at {}",
                pong.version,
                pong.protocol,
                path.display()
            );
            Some(client)
        }
        Err(e) => {
            eprintln!(
                "SKIP: socket at {} does not speak protocol {SUPPORTED_HERDR_PROTOCOL}: {e}",
                path.display()
            );
            None
        }
    }
}

/// The one explicit live-read-contract probe: `pane.list` followed by
/// `pane.read` on the first pane.
///
/// Returns the pane id whose read succeeded, or `None` when the live session
/// has no panes (an absent prerequisite, not a failure). A failing
/// `pane.list` or `pane.read` is a contract failure and panics with context;
/// an empty pane list prints `SKIP` and returns `None` so callers can
/// distinguish "nothing to read" from "read is broken".
fn probe_live_pane_for_read(c: &mut HerdrClient) -> Option<String> {
    let panes = c
        .pane_list(None)
        .expect("live contract: pane.list must succeed");
    eprintln!("live prerequisites: {} panes", panes.len());
    let Some(first) = panes.first() else {
        eprintln!("SKIP: live session has no panes; nothing to pane.read");
        return None;
    };
    let read = c
        .pane_read(&first.pane_id, ReadSource::Recent, Some(5))
        .unwrap_or_else(|e| panic!("live contract: pane.read({}) failed: {e}", first.pane_id));
    assert_eq!(
        read.pane_id, first.pane_id,
        "live contract: pane.read must echo the requested pane id"
    );
    Some(first.pane_id.clone())
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
    // Contract only: must not error. Contents depend on the running session;
    // an empty list is a valid (absent-prerequisite) state, not a failure.
    // Wire shape is pinned in fake-socket tests.
    let workspaces = c
        .workspace_list()
        .expect("live contract: workspace.list must succeed");
    if workspaces.is_empty() {
        eprintln!("live workspaces: 0 (empty session; nothing further to check)");
    } else {
        eprintln!("live workspaces: {}", workspaces.len());
    }
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
    // Contract only: must not error; an empty list means no tabs exist
    // (absent prerequisite), not a broken contract. Wire shape is pinned in
    // fake-socket tests.
    let tabs = c
        .tab_list(None)
        .expect("live contract: tab.list must succeed");
    if tabs.is_empty() {
        eprintln!("live tabs: 0 (empty session; nothing further to check)");
        return;
    }
    eprintln!("live tabs: {}", tabs.len());
    for t in &tabs {
        assert!(
            !t.tab_id.is_empty(),
            "live contract: tab ids must be non-empty"
        );
    }
}

#[test]
#[ignore = "requires a live herdr socket"]
fn live_pane_layout() {
    let Some(mut c) = client_or_skip() else {
        return;
    };
    // `None` = focused tab's layout. The empty-session prerequisite is
    // checked BEFORE requesting layout, so a malformed response or
    // `internal_error` can never pass silently as a skip: with panes
    // present, any `pane.layout` failure is a contract failure and panics
    // with context.
    let panes = c
        .pane_list(None)
        .expect("live contract: pane.list must succeed");
    if panes.is_empty() {
        eprintln!("SKIP: no panes in live session; no layout to check");
        return;
    }
    let layout = c.pane_layout(None).unwrap_or_else(|e| {
        panic!(
            "live contract: pane.layout failed with {} panes present: {e}",
            panes.len()
        )
    });
    eprintln!(
        "live layout: {} panes, {} splits, focused={}",
        layout.panes.len(),
        layout.splits.len(),
        layout.focused_pane_id
    );
    if layout.panes.is_empty() {
        eprintln!("live layout: empty (no panes in focused tab; nothing further to check)");
        return;
    }
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
    // The explicit live-read-contract probe distinguishes absent
    // prerequisites (no panes → skip, printed inside the probe) from contract
    // failures (pane.list/pane.read errors → panic inside the probe).
    // Deterministic pane.read wire shape lives in fake-socket tests.
    let Some(pane_id) = probe_live_pane_for_read(&mut c) else {
        return;
    };
    eprintln!("live pane.read ok for {pane_id}");
}
