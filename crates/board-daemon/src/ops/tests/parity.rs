//! Fake-client ↔ daemon method parity.
//!
//! `board_core::client::FakeBoardClient` re-implements part of boardd's RPC
//! surface and the **entire** board-tui test tier runs against it, so a method
//! the daemon routes but the fake does not is a hole a TUI suite cannot see: a
//! reducer path calling it passes its own tests and only fails live. Both lists
//! are generated from their own dispatch tables (`routes!` / `fake_methods!`),
//! so neither can drift from the code that actually answers requests.

use std::collections::BTreeSet;

use board_core::client::FAKE_CLIENT_METHODS;

use crate::ops::ROUTED_METHODS;

/// Routed methods the fake deliberately does not implement.
///
/// This allowlist is the load-bearing half of the guard: adding a method to the
/// daemon fails the parity test until someone either implements it in the fake
/// or consciously lands it here.
///
/// - `daemon.status` / `daemon.stop` — daemon lifecycle; the fake has no daemon.
/// - `harness.capabilities` / `harness.list` / `session.list` / `space.list` —
///   catalog RPCs answered from daemon config and a live Herdr. `board-tui`'s
///   `DemoClient` stubs these on top of the fake for the form suites.
/// - `run.cancel` / `run.retry` — reachable from the TUI (`Effect::RunCancel` /
///   `Effect::RunRetry`) but they mean "kill a pane" / "enqueue a run", which a
///   DB-only fake with no dispatcher cannot honestly model.
/// - `run.pane_exited` — the internal configured-harness wrapper callback; no
///   client, and therefore no fake, ever sends it.
const KNOWN_UNIMPLEMENTED: &[&str] = &[
    "daemon.status",
    "daemon.stop",
    "harness.capabilities",
    "harness.list",
    "run.cancel",
    "run.pane_exited",
    "run.retry",
    "session.list",
    "space.list",
];

fn set(methods: &[&str]) -> BTreeSet<String> {
    methods.iter().map(|m| (*m).to_string()).collect()
}

#[test]
fn routed_and_fake_method_lists_have_no_duplicates() {
    assert_eq!(
        set(ROUTED_METHODS).len(),
        ROUTED_METHODS.len(),
        "a method is routed twice: {ROUTED_METHODS:?}"
    );
    assert_eq!(
        set(FAKE_CLIENT_METHODS).len(),
        FAKE_CLIENT_METHODS.len(),
        "the fake implements a method twice: {FAKE_CLIENT_METHODS:?}"
    );
}

#[test]
fn the_fake_client_only_implements_methods_the_daemon_routes() {
    let extra: Vec<String> = set(FAKE_CLIENT_METHODS)
        .difference(&set(ROUTED_METHODS))
        .cloned()
        .collect();
    assert!(
        extra.is_empty(),
        "FakeBoardClient answers methods boardd does not route, so a TUI test \
         can pass against an RPC that does not exist: {extra:?}"
    );
}

#[test]
fn every_routed_method_is_implemented_by_the_fake_client_or_explicitly_allowlisted() {
    let missing: BTreeSet<String> = set(ROUTED_METHODS)
        .difference(&set(FAKE_CLIENT_METHODS))
        .cloned()
        .collect();
    assert_eq!(
        missing,
        set(KNOWN_UNIMPLEMENTED),
        "the set of routed-but-faked-nowhere methods changed. Implement the new \
         method in `board_core::client::fake` (preferred — the whole board-tui \
         test tier runs against it) or add it to KNOWN_UNIMPLEMENTED with a \
         reason. Routed: {ROUTED_METHODS:?}; faked: {FAKE_CLIENT_METHODS:?}"
    );
}

#[test]
fn the_allowlist_only_names_routed_methods() {
    let stale: Vec<String> = set(KNOWN_UNIMPLEMENTED)
        .difference(&set(ROUTED_METHODS))
        .cloned()
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_UNIMPLEMENTED names methods boardd no longer routes: {stale:?}"
    );
}
