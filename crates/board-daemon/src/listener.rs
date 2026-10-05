//! The boardd accept surface: a tokio Unix socket, or a Windows named pipe
//! (`\\.\pipe\` + socket path) rotated one instance per client.

use std::io;
#[cfg(any(windows, test))]
use std::path::Path;

#[cfg(unix)]
pub(crate) type Conn = tokio::net::UnixStream;
#[cfg(windows)]
pub(crate) type Conn = tokio::net::windows::named_pipe::NamedPipeServer;

pub(crate) struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    name: std::ffi::OsString,
    #[cfg(windows)]
    next: Conn,
}

impl Listener {
    /// Unix production binds through `bind_secured_socket` + `from_unix`.
    #[cfg(all(unix, test))]
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        Ok(Self::from_unix(tokio::net::UnixListener::bind(path)?))
    }

    #[cfg(unix)]
    pub(crate) fn from_unix(inner: tokio::net::UnixListener) -> Self {
        Self { inner }
    }

    /// `first_pipe_instance` makes a squatted or duplicate name a startup error.
    /// The default DACL grants this user, SYSTEM and Administrators full access
    /// but also gives Everyone (and Anonymous) read; a request needs write, so
    /// only this user, SYSTEM or an administrator can talk to boardd.
    #[cfg(windows)]
    pub(crate) fn bind(path: &Path) -> io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = board_ipc::pipe_name(path);
        let next = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        Ok(Self { name, next })
    }

    pub(crate) async fn accept(&mut self) -> io::Result<Conn> {
        #[cfg(unix)]
        {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            /// The client connected and already left (`ERROR_NO_DATA`).
            const ERROR_NO_DATA: i32 = 232;
            loop {
                let connected = self.next.connect().await;
                // Always rotate: an instance that failed to connect is dead,
                // and keeping it would stop boardd from accepting anyone.
                let fresh = ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&self.name)?;
                let conn = std::mem::replace(&mut self.next, fresh);
                match connected {
                    Ok(()) => return Ok(conn),
                    Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA) => continue,
                    Err(error) => return Err(error),
                }
            }
        }
    }
}
