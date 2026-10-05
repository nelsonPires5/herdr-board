//! Socket-path resolution, deadline configuration, and platform-aware
//! transport helpers: AF_UNIX here, the Windows named pipe in
//! `transport_windows.rs` (same `pub(crate)` signatures).
//!
//! All Unix `unsafe` blocks live in this module. The safe wrappers handle:
//! - AF_UNIX path-length validation
//! - Non-blocking connect with a deadline
//! - SOCK_CLOEXEC / SOCK_NONBLOCK atomically on Linux, portable fallback
//!   elsewhere
//! - Clearing the read timeout so blocking iterators wait forever — on
//!   platforms where `set_read_timeout(None)` returns `EINVAL` (macOS) we
//!   fall back to a huge sentinel duration.

use std::io;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::HerdrError;
#[cfg(unix)]
use crate::error::Result;

#[cfg(unix)]
pub(crate) type Stream = UnixStream;

#[cfg(windows)]
#[path = "transport_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{
    connect_with_deadline, poll_read_ready, poll_read_ready_infinite, set_nonblocking, Stream,
};

// -- socket-path resolution ---------------------------------------------------

/// Default socket path: `$HERDR_SOCKET_PATH` (herdr's canonical variable,
/// injected into panes/plugins so named sessions resolve to their own socket),
/// else `$HERDR_SOCKET` (this crate's override), else the default session's
/// `~/.config/herdr/herdr.sock` (`%APPDATA%\herdr\herdr.sock` on Windows).
pub fn default_socket_path() -> PathBuf {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    default_socket_path_from(
        var("HERDR_SOCKET_PATH").or_else(|| var("HERDR_SOCKET")),
        var("HOME"),
        var("APPDATA"),
    )
}

/// Pure resolution: explicit socket, else the platform's default session path.
fn default_socket_path_from(
    explicit: Option<String>,
    home: Option<String>,
    appdata: Option<String>,
) -> PathBuf {
    if let Some(path) = explicit {
        return PathBuf::from(path);
    }
    if cfg!(windows) {
        if let Some(appdata) = appdata {
            return PathBuf::from(appdata).join("herdr").join("herdr.sock");
        }
    }
    PathBuf::from(home.unwrap_or_else(|| "/root".to_string())).join(".config/herdr/herdr.sock")
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_default_socket_is_under_appdata() {
        let path = super::default_socket_path_from(
            None,
            None,
            Some(r"C:\Users\a b\AppData\Roaming".into()),
        );
        assert_eq!(
            path,
            std::path::PathBuf::from(r"C:\Users\a b\AppData\Roaming\herdr\herdr.sock")
        );
    }

    #[test]
    fn explicit_socket_env_wins() {
        let path = super::default_socket_path_from(Some("/x/h.sock".into()), None, None);
        assert_eq!(path, std::path::PathBuf::from("/x/h.sock"));
    }
}
