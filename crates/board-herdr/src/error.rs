//! Error type for the herdr client.

use thiserror::Error;

/// Errors returned by [`HerdrClient`](crate::HerdrClient) and
/// [`HerdrEvents`](crate::HerdrEvents).
#[derive(Debug, Error)]
pub enum HerdrError {
    /// Socket / transport level failure (connect, read, write).
    #[error("herdr io error: {0}")]
    Io(#[from] std::io::Error),

    /// The daemon replied with an `error` envelope. `code` is a herdr error
    /// code string (e.g. `invalid_request`, `internal_error`), not a number.
    #[error("herdr protocol error [{code}]: {message}")]
    Protocol { code: String, message: String },

    /// A response or event line could not be decoded into the expected shape.
    #[error("herdr decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// A bounded socket operation did not complete in time.
    #[error("herdr {operation} deadline exceeded")]
    Deadline { operation: &'static str },

    /// The connection was closed by the peer, or EOF was hit mid-call.
    #[error("herdr connection closed")]
    Disconnected,
}

/// Return only protocol codes whose spelling is part of the supported Herdr
/// contract. Error envelopes are remote input, so unknown code strings must
/// not become diagnostic fields.
pub(crate) fn diagnostic_protocol_code(code: &str) -> &'static str {
    match code {
        "agent_name_taken" => "agent_name_taken",
        "agent_pane_busy" => "agent_pane_busy",
        "incompatible_protocol" => "incompatible_protocol",
        "internal_error" => "internal_error",
        "invalid_request" => "invalid_request",
        "pane_not_found" => "pane_not_found",
        "unsupported_agent_kind" => "unsupported_agent_kind",
        "workspace_not_found" => "workspace_not_found",
        _ => "unknown_protocol",
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, HerdrError>;
