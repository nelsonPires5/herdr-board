use board_herdr::{Request, Response};
use serde_json::{json, Value};

#[test]
fn request_round_trips_to_line() {
    let req = Request {
        id: "7".into(),
        method: "workspace.list".into(),
        params: json!({}),
    };
    let line = req.to_line().unwrap();
    assert!(line.ends_with('\n'));
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["id"], "7");
    assert_eq!(v["method"], "workspace.list");
    assert_eq!(v["params"], json!({}));
}

#[test]
fn decodes_success_response() {
    let line = r#"{"id":"3","result":{"type":"ok"}}"#;
    let r = Response::from_line(line).unwrap();
    assert_eq!(r.id, "3");
    assert_eq!(r.result.unwrap()["type"], "ok");
    assert!(r.error.is_none());
}

#[test]
fn decodes_error_response() {
    let line = r#"{"id":"3","error":{"code":"invalid_request","message":"boom"}}"#;
    let r = Response::from_line(line).unwrap();
    let e = r.error.unwrap();
    assert_eq!(e.code, "invalid_request");
    assert_eq!(e.message, "boom");
    assert!(r.result.is_none());
}

#[test]
fn tolerates_unknown_fields() {
    let line = r#"{"id":"3","result":{"type":"ok"},"extra":42}"#;
    let r = Response::from_line(line).unwrap();
    assert_eq!(r.id, "3");
}
