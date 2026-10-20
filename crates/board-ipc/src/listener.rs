//! The blocking listener (test fakes and other synchronous servers).

use std::io;
use std::path::Path;

use crate::{sys, LocalStream};

/// A blocking listener for [`LocalStream`] connections.
///
/// unix: a `UnixListener` bound at the path (the socket file is left behind
/// on drop, as std does). Windows: named-pipe server instances with an
/// owner-only DACL that reject remote clients; the next instance is created
/// before a connected one is handed off, so clients see `ERROR_PIPE_BUSY`
/// (never "absent") between accepts.
#[derive(Debug)]
pub struct LocalListener {
    inner: sys::Listener,
}

impl LocalListener {
    /// Start listening at `path`. A path another listener already owns
    /// fails with `AddrInUse` on both platforms.
    pub fn bind(path: &Path) -> io::Result<LocalListener> {
        sys::Listener::bind(path).map(|inner| LocalListener { inner })
    }

    /// Wait for the next client. In nonblocking mode, fails with
    /// `WouldBlock` when none is pending. The accepted stream is blocking.
    pub fn accept(&self) -> io::Result<LocalStream> {
        self.inner.accept().map(|inner| LocalStream { inner })
    }

    /// An endless iterator over [`accept`](Self::accept) results.
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming { listener: self }
    }

    /// Make [`accept`](Self::accept) return `WouldBlock` instead of waiting.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }
}

/// Iterator returned by [`LocalListener::incoming`]; never yields `None`.
#[derive(Debug)]
pub struct Incoming<'a> {
    listener: &'a LocalListener,
}

impl Iterator for Incoming<'_> {
    type Item = io::Result<LocalStream>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.listener.accept())
    }
}
