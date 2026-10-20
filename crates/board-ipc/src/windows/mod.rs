//! Named-pipe implementation. All Windows `unsafe` lives under this module.
//!
//! Every handle is overlapped: a blocking call issues one overlapped
//! operation and waits on its own event with the configured timeout, then
//! cancels with `CancelIoEx` and waits for the cancellation so the stack
//! `OVERLAPPED` and buffer always outlive the I/O. Separate operations on one
//! handle never serialize against each other, which is what lets a reader
//! clone block while a writer clone sends.

#[cfg(feature = "tokio")]
pub(crate) mod async_pipe;
mod listener;
pub(crate) mod security;
mod stream;

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE};

pub(crate) use listener::Listener;
pub(crate) use security::OwnerOnly;
pub(crate) use stream::Stream;

/// In/out buffer size for server instances (a hint; pipes grow as needed).
const PIPE_BUFFER: u32 = 64 * 1024;

/// A manual-reset event for one overlapped operation.
#[derive(Debug)]
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Event> {
        // SAFETY: null attributes/name are documented as valid; the returned
        // handle is owned exactly once by `OwnedHandle`.
        let handle = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a fresh, valid event handle owned by nobody else.
        Ok(Event(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
}

/// A timeout as `WaitForSingleObject` milliseconds, rounded up so a short
/// timeout never becomes a zero-length poll; `None` is `INFINITE`.
fn wait_millis(timeout: Option<Duration>) -> u32 {
    match timeout {
        None => INFINITE,
        Some(timeout) => {
            let millis = timeout.as_nanos().div_ceil(1_000_000);
            u32::try_from(millis)
                .unwrap_or(INFINITE - 1)
                .min(INFINITE - 1)
        }
    }
}

/// Create one server instance of the pipe `name` (NUL-terminated UTF-16).
/// `first` demands that no instance exist yet, which is how a second server
/// on the same endpoint is refused: that case fails with `AddrInUse`, the
/// error unix `bind` gives for an endpoint already in use.
fn create_instance(
    name: &[u16],
    path: &Path,
    first: bool,
    security: &OwnerOnly,
) -> io::Result<OwnedHandle> {
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let attributes = security.attributes();
    // SAFETY: `name` is NUL-terminated and `attributes` (with the descriptor
    // `security` keeps alive) is valid for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(first_instance_error(
            io::Error::last_os_error(),
            path,
            first,
        ));
    }
    // SAFETY: `handle` is a fresh, valid pipe handle owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// `ERROR_ACCESS_DENIED` on a first-instance create means another server
/// already owns the name.
fn first_instance_error(error: io::Error, path: &Path, first: bool) -> io::Error {
    if first && error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
        io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another server is already listening on {}", path.display()),
        )
    } else {
        error
    }
}
