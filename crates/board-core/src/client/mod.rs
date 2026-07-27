//! Blocking clients for boardd and typed client APIs.

#[cfg(feature = "fake-client")]
mod fake;
mod traits;
mod unix;

#[cfg(feature = "fake-client")]
pub use fake::{FakeBoardClient, FAKE_CLIENT_METHODS};
pub use traits::BoardClient;
pub use unix::{BoardRpcError, EventStream, RpcClientError, RpcError, UnixClient};
