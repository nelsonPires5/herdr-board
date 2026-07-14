//! board-core — the shared heart of herdr-board (OWNED BY PHASE A).
//!
//! Pure, synchronous building blocks used by every other crate:
//! - [`protocol`]: serde types for the boardd socket protocol (source of truth).
//! - [`model`]: SQLite-row structs (Board/Column/Card/Comment/Run).
//! - [`db`]: rusqlite store, migrations, CRUD, position management, queries.
//! - [`engine`]: pure column-engine transition/entry/validation decisions.
//! - [`prompt`]: prompt assembly and effective-settings resolution.
//! - [`harness`]: argv/env builders for the builtin `claude` and config harnesses.
//! - [`config`]: `~/.config/herdr-board/config.toml` loader.
//! - [`paths`]: db/socket/log/config path resolution.
//! - [`client`]: blocking NDJSON `BoardClient` (+ `FakeBoardClient` behind a feature).
//! - [`spawn`]: `Spawner` trait + request/handle types (implemented by Phase D).

pub mod client;
pub mod config;
pub mod db;
pub mod engine;
pub mod harness;
pub mod model;
pub mod paths;
pub mod prompt;
pub mod protocol;
pub mod spawn;

pub use engine::ValidationError;

/// Crate-wide error type. `anyhow` is used at the process edges (CLI/daemon);
/// this `thiserror` enum carries the structured cases the daemon maps onto the
/// protocol's numeric error codes.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("config: {0}")]
    Config(String),

    /// Bad request / unknown method (protocol code 1).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Entity not found (protocol code 2).
    #[error("not found: {0}")]
    NotFound(String),

    /// Invalid state, e.g. delete a column with a running card (protocol code 3).
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// herdr unavailable (protocol code 4).
    #[error("herdr unavailable: {0}")]
    HerdrUnavailable(String),

    #[error(transparent)]
    Validation(#[from] ValidationError),
}

impl Error {
    /// Map onto the protocol's numeric error codes (see `docs/protocol.md`).
    pub fn code(&self) -> i32 {
        match self {
            Error::BadRequest(_) => 1,
            Error::NotFound(_) => 2,
            Error::InvalidState(_) => 3,
            Error::Validation(v) => v.code(),
            Error::HerdrUnavailable(_) => 4,
            Error::Sqlite(_) | Error::Json(_) | Error::Io(_) | Error::Config(_) => 5,
        }
    }
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;
