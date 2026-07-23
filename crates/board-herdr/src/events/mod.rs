//! Event streaming over a dedicated herdr connection.
//!
//! `events.subscribe` must run on its own persistent socket (never the
//! request/response connection). This module opens that socket, subscribes,
//! and exposes a blocking [`Iterator`] of [`HerdrEvent`].
//!
//! ## Subscription quirk (verified live, protocol 17)
//! A `pane.agent_status_changed` subscription **requires a concrete `pane_id`**
//! — herdr validates the pane exists and rejects a wildcard/missing id with
//! `internal_error`. So the daemon must build one subscription per pane it
//! wants status for (see [`watch_subscriptions`]) and re-subscribe (or
//! reconnect) as it starts new agents. `pane.exited` / `pane.closed` are global
//! and take no `pane_id`.
//!
//! Emitted event lines use the `EventEnvelope` shape
//! `{"event":"<kind>","data":{"type":"<kind>",...}}` with **underscore** kind
//! names (`pane_agent_status_changed`, `pane_exited`), whereas *subscription*
//! entries use **dotted** names (`pane.agent_status_changed`). Both are handled
//! here.

mod backoff;
mod parse;
mod stream;

pub use backoff::Backoff;
pub use parse::{parse_event_line, HerdrEvent};
pub use stream::{watch_subscriptions, HerdrEvents, Subscription};
