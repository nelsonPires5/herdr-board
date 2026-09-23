//! board-herdr — a typed, blocking client for the herdr socket API.
//!
//! herdr speaks NDJSON over a unix socket (`~/.config/herdr/herdr.sock`, or
//! `$HERDR_SOCKET`). Each request is one line
//! `{"id","method","params"}` and each reply is one line
//! `{"id","result"}` or `{"id","error":{code,message}}`.
//!
//! Two connection types:
//! - [`HerdrClient`] — request/response (workspace/tab/agent/pane/
//!   notification/session calls). Blocking; wrap in `spawn_blocking` in async
//!   code.
//! - [`HerdrEvents`] — a separate persistent connection streaming
//!   [`HerdrEvent`]s via `events.subscribe`.
//!
//! Method and field names are verified against `herdr api schema --json`
//! (Herdr 0.9.0, protocol 22), captured in `docs/herdr-0.9.0-schema.json`
//! (legacy fixture at `tests/fixtures/schema.json` is protocol 20).

mod client;
mod envelope;
mod error;
mod events;
mod params;
mod transport;
mod types;

/// The Herdr release this client's schema fixture was captured from.
///
/// This is a reference/display version, not a minimum-version check: any
/// release speaking [`SUPPORTED_HERDR_PROTOCOL`] is accepted, with a warning
/// when it differs from this reference.
pub const SUPPORTED_HERDR_VERSION: &str = "0.9.0";
/// The only Herdr socket protocol supported by this client.
///
/// This is the hard compatibility gate; a different protocol is rejected
/// before any board operation touches the socket.
pub const SUPPORTED_HERDR_PROTOCOL: u32 = 22;

pub use client::HerdrClient;
pub use envelope::{ErrorBody, Request, Response};
pub use error::{HerdrError, Result};
pub use events::{
    parse_event_line, watch_subscriptions, Backoff, HerdrEvent, HerdrEvents, Subscription,
};
pub use params::{
    AgentPromptParams, AgentPromptWaitOptions, AgentStartParams, AgentWaitParams, PaneRenameParams,
    PaneSplitParams, TabCreateParams, TabRenameParams, WorkspaceCreateParams,
};
pub use transport::{default_socket_path, SocketDeadlines};
pub use types::{
    AgentInfo, AgentSession, AgentStarted, AgentStatus, Layout, LayoutPane, LayoutSplit,
    NotificationShown, NotificationSound, PaneInfo, PaneReadResult, Pong, ReadSource, Rect,
    SessionSnapshot, SplitDirection, TabCreated, TabInfo, WorkspaceCreated, WorkspaceInfo,
};
