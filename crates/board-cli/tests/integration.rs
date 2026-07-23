//! Daemon integration tests exercising the real `board` binary and boardd with
//! the `LocalSpawner` and the fake harness (no herdr, no Claude cost). Each test
//! gets its own temp DB, socket, config, and daemon process.

#[path = "integration/support.rs"]
mod support;
pub(crate) use support::*;

#[path = "integration/events.rs"]
mod events;
#[path = "integration/harness.rs"]
mod harness;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
#[path = "integration/scope.rs"]
mod scope;
#[path = "integration/stop.rs"]
mod stop;
