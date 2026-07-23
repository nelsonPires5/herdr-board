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
//! (protocol 17), captured in `tests/fixtures/schema.json`.

mod client;
mod envelope;
mod error;
mod events;
mod params;
mod transport;
mod types;

pub use client::HerdrClient;
pub use envelope::{ErrorBody, Request, Response};
pub use error::{HerdrError, Result};
pub use events::{
    parse_event_line, watch_subscriptions, Backoff, HerdrEvent, HerdrEvents, Subscription,
};
pub use params::{
    AgentPromptParams, AgentPromptWaitOptions, AgentStartParams, AgentWaitParams, PaneRenameParams,
    PaneSplitParams, TabCreateParams, WorkspaceCreateParams,
};
pub use transport::{default_socket_path, SocketDeadlines};
pub use types::{
    AgentInfo, AgentStarted, AgentStatus, Layout, LayoutPane, LayoutSplit, NotificationShown,
    NotificationSound, PaneInfo, PaneReadResult, Pong, ReadSource, Rect, SessionSnapshot,
    SplitDirection, TabCreated, TabInfo, WorkspaceCreated, WorkspaceInfo,
};
