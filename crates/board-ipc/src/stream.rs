//! The blocking client/accepted stream.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::sys;

/// How long [`LocalStream::connect`] waits for a free pipe instance on
/// Windows (a live server between accepts reports `ERROR_PIPE_BUSY`). unix
/// `connect` is std's and never waits on this.
#[cfg(windows)]
const CONNECT_BUSY_WAIT: Duration = Duration::from_secs(5);

/// A connected, blocking, bidirectional local stream (an AF_UNIX socket on
/// unix, an overlapped named-pipe handle on Windows).
///
/// `Read`/`Write` are implemented for both `LocalStream` and `&LocalStream`.
/// Clones from [`try_clone`](Self::try_clone) refer to the same connection
/// and share its timeouts and nonblocking flag.
#[derive(Debug)]
pub struct LocalStream {
    pub(crate) inner: sys::Stream,
}

impl LocalStream {
    /// Connect to the endpoint at `path`.
    ///
    /// unix: `UnixStream::connect`. Windows: opens the pipe, waiting up to
    /// 5s for a free instance when the server is busy, then refuses (with
    /// `PermissionDenied`, never an "absent" error) a pipe whose server does
    /// not belong to the current user: pipe names are one machine-wide
    /// namespace, so another local user could have created the name first.
    pub fn connect(path: &Path) -> io::Result<LocalStream> {
        #[cfg(unix)]
        let inner = sys::Stream::connect(path)?;
        #[cfg(windows)]
        let inner = sys::Stream::connect_with_deadline(path, CONNECT_BUSY_WAIT)?;
        Ok(LocalStream { inner })
    }

    /// Connect to the endpoint at `path`, bounded by `timeout`.
    ///
    /// The path is checked with [`crate::endpoint::check`] first (unix:
    /// `ENAMETOOLONG`). Expiry of `timeout` fails with
    /// `io::ErrorKind::TimedOut`; no other failure uses that kind. On
    /// Windows the server-ownership check of [`connect`](Self::connect)
    /// applies too. The
    /// returned stream is in blocking mode with no read/write timeout.
    /// Descriptors are close-on-exec (unix); handles are not inheritable
    /// (Windows).
    pub fn connect_with_deadline(path: &Path, timeout: Duration) -> io::Result<LocalStream> {
        sys::Stream::connect_with_deadline(path, timeout).map(|inner| LocalStream { inner })
    }

    /// A second handle to the same connection, for a reader/writer split.
    pub fn try_clone(&self) -> io::Result<LocalStream> {
        self.inner.try_clone().map(|inner| LocalStream { inner })
    }

    /// Bound every subsequent blocking read; `None` waits forever. An
    /// expired read fails with `WouldBlock` (unix, std) or `TimedOut`
    /// (Windows) — treat both as a deadline. `Some(Duration::ZERO)` is
    /// rejected with `InvalidInput`, as in std.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(timeout)
    }

    /// Bound every subsequent blocking write; same semantics as
    /// [`set_read_timeout`](Self::set_read_timeout).
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(timeout)
    }

    /// In nonblocking mode a read with no data available fails with
    /// `WouldBlock`. Windows writes still complete (bounded by the write
    /// timeout) rather than failing with `WouldBlock`.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking)
    }

    /// Wait until a read would not block — data is buffered or the peer
    /// closed — without consuming anything or touching the read timeout.
    /// `Ok(true)` when readable, `Ok(false)` when `timeout` expired; `None`
    /// waits forever. A zero timeout still waits ~1ms (unix `poll` contract
    /// kept from board-herdr). unix restarts on `EINTR`; Windows waits on a
    /// zero-byte overlapped read, which completes on data or close without
    /// consuming anything.
    pub fn poll_read_ready(&self, timeout: Option<Duration>) -> io::Result<bool> {
        self.inner.poll_read_ready(timeout)
    }

    /// The process id of the pipe server this client is connected to
    /// (`GetNamedPipeServerProcessId`), for identity checks on the daemon.
    #[cfg(windows)]
    pub fn server_process_id(&self) -> io::Result<u32> {
        self.inner.server_process_id()
    }

    /// The underlying std socket.
    #[cfg(unix)]
    pub fn as_unix(&self) -> &std::os::unix::net::UnixStream {
        &self.inner.0
    }
}

#[cfg(unix)]
impl From<std::os::unix::net::UnixStream> for LocalStream {
    fn from(stream: std::os::unix::net::UnixStream) -> Self {
        LocalStream {
            inner: sys::Stream(stream),
        }
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for LocalStream {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.inner.0.as_fd()
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for LocalStream {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.inner.0.as_raw_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsHandle for LocalStream {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.inner.as_handle()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for LocalStream {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        std::os::windows::io::AsRawHandle::as_raw_handle(&self.inner.as_handle())
    }
}

impl Read for LocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Read for &LocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for LocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &LocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
