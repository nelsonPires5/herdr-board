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

/// Convenience alias.
pub type Result<T> = std::result::Result<T, HerdrError>;
