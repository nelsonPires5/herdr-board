//! AF_UNIX implementation. All unix `unsafe` lives here; it is the deadline
//! connect and readiness poll board-herdr's transport has always used, moved
//! verbatim apart from returning `io::Error` (deadline = `TimedOut`).

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub(crate) struct Stream(pub(crate) UnixStream);

impl Stream {
    pub(crate) fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path).map(Stream)
    }

    /// Open a blocking AF_UNIX stream to `path`, bounded by `timeout`.
    ///
    /// SOCK_CLOEXEC / SOCK_NONBLOCK are set atomically on Linux, with a
    /// portable `fcntl` fallback elsewhere; O_NONBLOCK is cleared again
    /// before returning so callers get std blocking semantics.
    pub(crate) fn connect_with_deadline(path: &Path, timeout: Duration) -> io::Result<Stream> {
        crate::endpoint::check(path)?;
        let path_bytes = path.as_os_str().as_bytes();

        // SAFETY: all libc pointers below refer to initialized local storage for
        // the duration of each call. `fd` has one owner and is closed on every
        // error path or transferred exactly once to `UnixStream`.
        unsafe {
            let socket_kind = libc::SOCK_STREAM;
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let socket_kind = socket_kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
            let fd = libc::socket(libc::AF_UNIX, socket_kind, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
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
                    return Err(close_error(io::Error::last_os_error()));
                }
                let status_flags = libc::fcntl(fd, libc::F_GETFL);
                if status_flags < 0
                    || libc::fcntl(fd, libc::F_SETFL, status_flags | libc::O_NONBLOCK) < 0
                {
                    return Err(close_error(io::Error::last_os_error()));
                }
            }
            let mut address: libc::sockaddr_un = std::mem::zeroed();
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            std::ptr::copy_nonoverlapping(
                path_bytes.as_ptr().cast(),
                address.sun_path.as_mut_ptr(),
                path_bytes.len(),
            );
            let address_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path)
                + path_bytes.len()
                + 1) as libc::socklen_t;
            if libc::connect(
                fd,
                (&raw const address).cast::<libc::sockaddr>(),
                address_len,
            ) < 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINPROGRESS) {
                    return Err(close_error(error));
                }
                let millis = timeout.as_millis().min(i32::MAX as u128).max(1) as i32;
                let mut poll_fd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ready = libc::poll(&raw mut poll_fd, 1, millis);
                if ready == 0 {
                    return Err(close_error(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "connect deadline expired",
                    )));
                }
                if ready < 0 {
                    return Err(close_error(io::Error::last_os_error()));
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
                    return Err(close_error(io::Error::last_os_error()));
                }
                if socket_error != 0 {
                    return Err(close_error(io::Error::from_raw_os_error(socket_error)));
                }
            }
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) < 0 {
                return Err(close_error(io::Error::last_os_error()));
            }
            Ok(Stream(UnixStream::from_raw_fd(fd)))
        }
    }

    pub(crate) fn try_clone(&self) -> io::Result<Stream> {
        self.0.try_clone().map(Stream)
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    pub(crate) fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    pub(crate) fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    /// `poll(POLLIN)` until readable (incl. hangup) or `timeout`; `None`
    /// blocks indefinitely. Restarts on `EINTR`.
    pub(crate) fn poll_read_ready(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let millis = match timeout {
            Some(timeout) => timeout.as_millis().min(i32::MAX as u128).max(1) as i32,
            None => -1,
        };
        let mut pfd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: `pfd` is initialized local storage and the fd stays
            // open (borrowed from `self`) for the duration of the call.
            let ready = unsafe { libc::poll(&raw mut pfd, 1, millis) };
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            return Ok(ready > 0);
        }
    }

    pub(crate) fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0).read(buf)
    }

    pub(crate) fn write(&self, buf: &[u8]) -> io::Result<usize> {
        (&self.0).write(buf)
    }
}

#[derive(Debug)]
pub(crate) struct Listener(UnixListener);

impl Listener {
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        UnixListener::bind(path).map(Listener)
    }

    pub(crate) fn accept(&self) -> io::Result<Stream> {
        self.0.accept().map(|(stream, _)| Stream(stream))
    }

    pub(crate) fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }
}
