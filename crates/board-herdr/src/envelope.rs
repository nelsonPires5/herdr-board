//! NDJSON request/response envelope for the herdr socket protocol.
//!
//! Wire format (one JSON object per line):
//! - request:  `{"id":"<str>","method":"<name>","params":{...}}`
//! - success:  `{"id":"<str>","result":{...}}`
//! - error:    `{"id":"<str>","error":{"code":"<str>","message":"<str>"}}`

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An outbound request envelope.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// Encode as a single NDJSON line (trailing `\n` included).
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

/// The `error` body carried by an error response / event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// An inbound response envelope. Exactly one of `result` / `error` is set for a
/// well-formed reply; both are optional here so decoding is tolerant.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<ErrorBody>,
}

impl Response {
    /// Parse one NDJSON line into a [`Response`].
    pub fn from_line(line: &str) -> Result<Response, serde_json::Error> {
        serde_json::from_str(line.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
