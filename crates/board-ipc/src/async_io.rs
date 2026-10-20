//! Tokio listener/stream for the boardd server (`tokio` feature).

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    pub(crate) type Stream = tokio::net::UnixStream;

    #[derive(Debug)]
    pub(crate) struct Listener(tokio::net::UnixListener);

    impl Listener {
        pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
            tokio::net::UnixListener::bind(path).map(Listener)
        }

        pub(crate) async fn accept(&mut self) -> io::Result<Stream> {
            self.0.accept().await.map(|(stream, _)| stream)
        }
    }

    pub(crate) async fn connect(path: &Path) -> io::Result<Stream> {
        tokio::net::UnixStream::connect(path).await
    }
}

#[cfg(windows)]
mod imp {
    pub(crate) use crate::windows::async_pipe::{Listener, Stream};

    pub(crate) async fn connect(path: &std::path::Path) -> std::io::Result<Stream> {
        Stream::connect(path).await
    }
}

/// The read half from [`AsyncLocalStream::into_split`].
#[cfg(unix)]
pub type AsyncReadHalf = tokio::net::unix::OwnedReadHalf;
/// The write half from [`AsyncLocalStream::into_split`].
#[cfg(unix)]
pub type AsyncWriteHalf = tokio::net::unix::OwnedWriteHalf;
/// The read half from [`AsyncLocalStream::into_split`].
#[cfg(windows)]
pub type AsyncReadHalf = tokio::io::ReadHalf<AsyncLocalStream>;
/// The write half from [`AsyncLocalStream::into_split`].
#[cfg(windows)]
pub type AsyncWriteHalf = tokio::io::WriteHalf<AsyncLocalStream>;

/// The boardd server listener. Must be created inside a tokio runtime.
///
/// unix: `tokio::net::UnixListener::bind` — the socket file gets the
/// process umask; the daemon still owns the follow-up `chmod 0600` (and
/// removing the file if that fails) on the same path. Windows: a named pipe
/// whose first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
/// an owner-only DACL, and remote clients rejected; nothing exists on disk,
/// so there is no permission step. Binding an endpoint another listener
/// owns fails with `AddrInUse` (Windows) / std's bind error (unix).
#[derive(Debug)]
pub struct AsyncLocalListener {
    inner: imp::Listener,
}

impl AsyncLocalListener {
    pub fn bind(path: &Path) -> io::Result<AsyncLocalListener> {
        imp::Listener::bind(path).map(|inner| AsyncLocalListener { inner })
    }

    /// Wait for the next client. Cancel safe, so it can be a `select!`
    /// branch next to a shutdown signal. An error affects only that one
    /// attempt; keep accepting.
    pub async fn accept(&mut self) -> io::Result<AsyncLocalStream> {
        self.inner
            .accept()
            .await
            .map(|inner| AsyncLocalStream { inner })
    }
}

/// One async connection (accepted by [`AsyncLocalListener`] or opened with
/// [`AsyncLocalStream::connect`]).
#[derive(Debug)]
pub struct AsyncLocalStream {
    inner: imp::Stream,
}

impl AsyncLocalStream {
    /// Connect to the endpoint at `path` (Windows: waits up to 5s for a
    /// free pipe instance). Must be called inside a tokio runtime.
    pub async fn connect(path: &Path) -> io::Result<AsyncLocalStream> {
        imp::connect(path)
            .await
            .map(|inner| AsyncLocalStream { inner })
    }

    /// Split into independently owned read and write halves (unix: tokio's
    /// zero-cost owned halves; Windows: `tokio::io::split`).
    pub fn into_split(self) -> (AsyncReadHalf, AsyncWriteHalf) {
        #[cfg(unix)]
        {
            self.inner.into_split()
        }
        #[cfg(windows)]
        {
            tokio::io::split(self)
        }
    }
}

impl AsyncRead for AsyncLocalStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for AsyncLocalStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
