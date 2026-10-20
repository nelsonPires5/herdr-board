//! board-ipc — the one cross-platform local-endpoint transport.
//!
//! Every board crate that talks to boardd or Herdr goes through this crate;
//! nothing else opens a socket or a pipe. An *endpoint* is always named by a
//! filesystem path (the `BOARD_SOCKET` / `HERDR_SOCKET_PATH` string):
//!
//! - **unix** — an AF_UNIX stream socket bound at that path. [`LocalStream`]
//!   and [`LocalListener`] are thin wrappers over `std::os::unix::net`, and
//!   the deadline connect is the libc implementation board-herdr has always
//!   used (CLOEXEC, nonblocking connect + `poll`, `ENAMETOOLONG` pre-check).
//! - **Windows** — a named pipe called `\\.\pipe\` + the path string verbatim
//!   (Herdr's own convention; see [`endpoint::pipe_name`]). Nothing is created
//!   on disk. Client handles are opened `FILE_FLAG_OVERLAPPED` so timeouts,
//!   nonblocking reads, and a concurrent read on one clone plus a write on
//!   another behave like the unix socket instead of serializing. Listeners
//!   create pipe instances with an owner-only DACL and reject remote clients;
//!   clients refuse a pipe that another user created (pipe names are one
//!   machine-wide namespace, so a name can be taken first).
//!
//! The blocking API ([`LocalStream`], [`LocalListener`]) mirrors the std
//! `UnixStream`/`UnixListener` surface the callers already use. Settings set
//! through one [`LocalStream::try_clone`] are shared by every clone, exactly
//! as `SO_RCVTIMEO`/`O_NONBLOCK` are shared by dup'd unix descriptors.
//!
//! With the `tokio` feature, [`AsyncLocalListener`]/[`AsyncLocalStream`]
//! provide the boardd server side.

pub mod endpoint;
mod listener;
#[cfg(windows)]
pub mod owner_only;
mod stream;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(feature = "tokio")]
mod async_io;

#[cfg(feature = "tokio")]
pub use async_io::{AsyncLocalListener, AsyncLocalStream, AsyncReadHalf, AsyncWriteHalf};
pub use listener::{Incoming, LocalListener};
pub use stream::LocalStream;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;
