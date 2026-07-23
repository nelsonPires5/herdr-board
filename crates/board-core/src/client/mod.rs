//! Blocking clients for boardd and typed client APIs.

#[cfg(feature = "fake-client")]
mod fake;
mod traits;
mod unix;

#[cfg(feature = "fake-client")]
pub use fake::FakeBoardClient;
pub use traits::BoardClient;
pub use unix::{EventStream, UnixClient};
