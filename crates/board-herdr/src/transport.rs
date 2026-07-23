//! Socket-path resolution, deadline configuration, and platform-aware
//! AF_UNIX transport helpers.
//!
//! All `unsafe` blocks live in this module. The safe wrappers handle:
//! - AF_UNIX path-length validation
//! - Non-blocking connect with a deadline
//! - SOCK_CLOEXEC / SOCK_NONBLOCK atomically on Linux, portable fallback
//!   elsewhere
//! - Clearing the read timeout so blocking iterators wait forever — on
//!   platforms where `set_read_timeout(None)` returns `EINVAL` (macOS) we
//!   fall back to a huge sentinel duration.

use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{HerdrError, Result};

// -- socket-path resolution ---------------------------------------------------

/// Default socket path: `$HERDR_SOCKET_PATH` (herdr's canonical variable,
/// injected into panes/plugins so named sessions resolve to their own socket),
/// else `$HERDR_SOCKET` (this crate's override), else the default session's
/// `~/.config/herdr/herdr.sock`.
pub fn default_socket_path() -> PathBuf {
    for var in ["HERDR_SOCKET_PATH", "HERDR_SOCKET"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".config/herdr/herdr.sock")
}

// -- deadline configuration ---------------------------------------------------

/// Bounds for blocking socket operations. Long-running Herdr methods extend
/// `request` by their wire `timeout_ms` plus `method_grace`.
#[derive(Debug, Clone, Copy)]
pub struct SocketDeadlines {
    pub connect: Duration,
    pub read: Duration,
    pub write: Duration,
    pub handshake: Duration,
    pub request: Duration,
    pub method_grace: Duration,
}

impl Default for SocketDeadlines {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            read: Duration::from_secs(30),
            write: Duration::from_secs(5),
            handshake: Duration::from_secs(5),
            request: Duration::from_secs(30),
            method_grace: Duration::from_secs(5),
        }
    }
}

// -- connect with deadline ----------------------------------------------------

/// Open a blocking AF_UNIX stream to `path`, bounded by `timeout`.
///
/// The returned stream is in **blocking** mode (O_NONBLOCK cleared) so
/// callers can use std `read`/`write` with optional `set_read_timeout`.
pub(crate) fn connect_with_deadline(path: &Path, timeout: Duration) -> Result<UnixStream> {
    let path_bytes = path.as_os_str().as_bytes();
    let max_path = std::mem::size_of::<libc::sockaddr_un>()
        - std::mem::offset_of!(libc::sockaddr_un, sun_path);
    if path_bytes.len() >= max_path {
        return Err(HerdrError::Io(std::io::Error::from_raw_os_error(
            libc::ENAMETOOLONG,
        )));
    }

    // SAFETY: all libc pointers below refer to initialized local storage for
    // the duration of each call. `fd` has one owner and is closed on every
    // error path or transferred exactly once to `UnixStream`.
    unsafe {
        let socket_kind = libc::SOCK_STREAM;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let socket_kind = socket_kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
        let fd = libc::socket(libc::AF_UNIX, socket_kind, 0);
        if fd < 0 {
            return Err(HerdrError::Io(std::io::Error::last_os_error()));
        }
        let close_error = |error| {
            libc::close(fd);
            error
        };
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let descriptor_flags = libc::fcntl(fd, libc::F_GETFD);
            if descriptor_flags < 0
                || libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) < 0
            {
                return Err(close_error(HerdrError::Io(std::io::Error::last_os_error())));
            }
            let status_flags = libc::fcntl(fd, libc::F_GETFL);
            if status_flags < 0
                || libc::fcntl(fd, libc::F_SETFL, status_flags | libc::O_NONBLOCK) < 0
            {
                return Err(close_error(HerdrError::Io(std::io::Error::last_os_error())));
            }
        }
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr().cast(),
            address.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
        let address_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
            as libc::socklen_t;
        if libc::connect(
            fd,
            (&raw const address).cast::<libc::sockaddr>(),
            address_len,
        ) < 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(close_error(HerdrError::Io(error)));
            }
            let millis = timeout.as_millis().min(i32::MAX as u128).max(1) as i32;
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = libc::poll(&raw mut poll_fd, 1, millis);
            if ready == 0 {
                return Err(close_error(HerdrError::Deadline {
                    operation: "connect",
                }));
            }
            if ready < 0 {
                return Err(close_error(HerdrError::Io(std::io::Error::last_os_error())));
            }
            let mut socket_error = 0;
            let mut error_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            if libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut socket_error).cast(),
                &raw mut error_len,
            ) < 0
            {
                return Err(close_error(HerdrError::Io(std::io::Error::last_os_error())));
            }
            if socket_error != 0 {
                return Err(close_error(HerdrError::Io(
                    std::io::Error::from_raw_os_error(socket_error),
                )));
            }
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) < 0 {
            return Err(close_error(HerdrError::Io(std::io::Error::last_os_error())));
        }
        Ok(UnixStream::from_raw_fd(fd))
    }
}

// -- nonblocking toggle ------------------------------------------------------

/// Set or clear O_NONBLOCK on a Unix stream.
pub(crate) fn set_nonblocking(stream: &UnixStream, nonblocking: bool) -> Result<()> {
    stream
        .set_nonblocking(nonblocking)
        .map_err(HerdrError::from)
}

// -- small helpers ---------------------------------------------------------

/// Classify a std io error from a deadline-constrained operation.
pub(crate) fn deadline_io(error: io::Error, operation: &'static str) -> HerdrError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        HerdrError::Deadline { operation }
    } else {
        HerdrError::Io(error)
    }
}

/// Block until `stream` is readable or `deadline` expires, without touching
/// SO_RCVTIMEO.  Returns `Ok(true)` when data is ready, `Ok(false)` on
/// timeout, and `Err` on error.
pub(crate) fn poll_read_ready(stream: &UnixStream, deadline: Duration) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let millis = deadline.as_millis().min(i32::MAX as u128).max(1) as i32;
    let fd = stream.as_raw_fd();
    unsafe {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let ready = libc::poll(&raw mut pfd, 1, millis);
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(HerdrError::from(err));
            }
            return Ok(ready > 0);
        }
    }
}

/// Block indefinitely until `stream` becomes readable or closes.
pub(crate) fn poll_read_ready_infinite(stream: &UnixStream) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    // SAFETY: fd is valid for the duration of poll.  poll with timeout -1
    // blocks until the fd is readable or an error occurs.
    unsafe {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let ready = libc::poll(&raw mut pfd, 1, -1);
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(HerdrError::from(err));
            }
            return Ok(ready > 0);
        }
    }
}
