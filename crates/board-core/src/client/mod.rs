//! Blocking clients for boardd and typed client APIs.

#[cfg(feature = "fake-client")]
mod fake;
mod local;
mod traits;

#[cfg(feature = "fake-client")]
pub use fake::{FakeBoardClient, FAKE_CLIENT_METHODS};
pub use local::{
    is_daemon_absent, BoardRpcError, EventStream, LocalClient, RpcClientError, RpcError, UnixClient,
};
pub use traits::BoardClient;
