//! Tokio named-pipe listener/stream behind `AsyncLocalListener`/`AsyncLocalStream`.

use std::ffi::OsString;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

use super::{first_instance_error, OwnerOnly};

/// How long `connect` retries a busy pipe, as the blocking `connect` does.
const CONNECT_BUSY_WAIT: Duration = Duration::from_secs(5);
const BUSY_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct Listener {
    path: PathBuf,
    name: OsString,
    security: OwnerOnly,
    /// The instance clients connect to next; `None` only after creating a
    /// replacement failed (the next `accept` retries).
    listening: Option<NamedPipeServer>,
}

impl Listener {
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let name = crate::endpoint::pipe_name(path)?;
        let security = OwnerOnly::new()?;
        let first = create(&name, path, true, &security)?;
        Ok(Listener {
            path: path.to_path_buf(),
            name,
            security,
            listening: Some(first),
        })
    }

    /// Cancel safe: a dropped `accept` leaves the listening instance (and any
    /// client already attached to it) for the next call.
    pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
        if self.listening.is_none() {
            self.listening = Some(self.create_next()?);
        }
        let connected = match &self.listening {
            Some(server) => server.connect().await,
            None => return Err(io::ErrorKind::NotConnected.into()),
        };
        if let Err(error) = connected {
            // The instance is unusable (e.g. the client already left);
            // replace it so the accept loop cannot spin on it.
            self.listening = self.create_next().ok();
            return Err(error);
        }
        // Serve the name again before handing off (standard accept pattern).
        let next = self.create_next().ok();
        match std::mem::replace(&mut self.listening, next) {
            Some(server) => Ok(Stream::Server(server)),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    fn create_next(&self) -> io::Result<NamedPipeServer> {
        create(&self.name, &self.path, false, &self.security)
    }
}

fn create(
    name: &OsString,
    path: &Path,
    first: bool,
    security: &OwnerOnly,
) -> io::Result<NamedPipeServer> {
    let mut attributes = security.attributes();
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor
    // `security` keeps alive for the duration of the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .pipe_mode(PipeMode::Byte)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    }
    .map_err(|error| first_instance_error(error, path, first))
}

#[derive(Debug)]
pub(crate) enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl Stream {
    pub(crate) async fn connect(path: &Path) -> io::Result<Stream> {
        let name = crate::endpoint::pipe_name(path)?;
        let deadline = Instant::now() + CONNECT_BUSY_WAIT;
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => {
                    // Nothing is sent before the server is known to be ours.
                    super::security::verify_server(client.as_raw_handle(), path)?;
                    return Ok(Stream::Client(client));
                }
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Stream::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Server(pipe) => Pin::new(pipe).poll_write(cx, buf),
            Stream::Client(pipe) => Pin::new(pipe).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(pipe) => Pin::new(pipe).poll_flush(cx),
            Stream::Client(pipe) => Pin::new(pipe).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
            Stream::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
        }
    }
}
