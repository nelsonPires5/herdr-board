//! `pane.set_title` — the RPC the TUI plugin pane uses instead of shelling out
//! to `herdr pane rename` itself.

use super::*;

/// A fake Herdr that answers `pane.rename` with the renamed pane, so the test
/// can read back the exact params the daemon sent.
fn renaming_herdr() -> FakeHerdr {
    testkit::herdr_server()
        .on("pane.rename", |req| {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            let mut pane = testkit::pane_info(pane_id);
            pane["label"] = req["params"]["label"].clone();
            testkit::reply(req, json!({"type": "pane_info", "pane": pane}))
        })
        .serve()
}

#[test]
fn pane_set_title_renames_the_caller_pane_through_herdr() {
    let herdr = renaming_herdr();
    // No session registry on purpose: the pane belongs to the *caller's* Herdr,
    // named by `origin_socket`, so this must not depend on session enumeration.
    let d = test_daemon(Config::default());

    let v = handle_request(
        &d,
        "pane.set_title",
        json!({
            "pane_id": "w1:p9",
            "title": "Board [herdr-board · ACTIVE]",
            "origin_socket": herdr.socket,
        }),
    )
    .unwrap();

    assert_eq!(v, json!({"renamed": true}));
    // The protocol gate runs first, then exactly one rename — nothing else.
    assert_eq!(herdr.methods(), vec!["ping", "pane.rename"]);
    let sent = herdr.requests_for("pane.rename");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["params"]["pane_id"], "w1:p9");
    assert_eq!(sent[0]["params"]["label"], "Board [herdr-board · ACTIVE]");
}

#[test]
fn pane_set_title_rejects_a_socket_with_the_wrong_protocol() {
    let herdr = testkit::herdr_server()
        .protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1)
        .serve();
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:p9", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(
        msg.contains(&format!(
            "Herdr {} with protocol {} is required",
            board_herdr::SUPPORTED_HERDR_VERSION,
            board_herdr::SUPPORTED_HERDR_PROTOCOL
        )),
        "message: {msg}"
    );
    // The gate is the first and only request: no rename reaches a socket the
    // daemon has not verified.
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn pane_set_title_reports_an_unreachable_socket_as_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:p9", "title": "Board", "origin_socket": "/tmp/no-such-herdr.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(
        err.to_string().contains("origin Herdr socket"),
        "message: {err}"
    );
}

#[test]
fn pane_set_title_rejects_an_empty_pane_id_before_touching_herdr() {
    let herdr = renaming_herdr();
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "  ", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(herdr.methods().is_empty(), "herdr was contacted");
}

#[test]
fn pane_set_title_surfaces_a_herdr_refusal_rather_than_claiming_success() {
    let herdr = testkit::herdr_server()
        .on("pane.rename", |req| {
            testkit::error(req, "pane_not_found", "pane not found")
        })
        .serve();
    let d = test_daemon(Config::default());

    let err = handle_request(
        &d,
        "pane.set_title",
        json!({"pane_id": "w1:gone", "title": "Board", "origin_socket": herdr.socket}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(msg.contains("pane.rename w1:gone"), "message: {msg}");
}
