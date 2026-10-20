//! Blocking named-pipe listener: one listening instance at a time, replaced
//! before each connected instance is handed off.

use std::fmt;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Pipes::ConnectNamedPipe;
use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::{create_instance, Event, OwnerOnly, Stream};

pub(crate) struct Listener {
    path: PathBuf,
    name: Vec<u16>,
    security: OwnerOnly,
    nonblocking: AtomicBool,
    /// The instance clients connect to next. `None` only after creating a
    /// replacement failed; the next `accept` retries.
    listening: Mutex<Option<Instance>>,
}

impl fmt::Debug for Listener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Listener")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Listener {
    pub(crate) fn bind(path: &Path) -> io::Result<Listener> {
        let name = crate::endpoint::wide_pipe_name(path)?;
        let security = OwnerOnly::new()?;
        let first = create_instance(&name, path, true, &security)?;
        Ok(Listener {
            path: path.to_path_buf(),
            name,
            security,
            nonblocking: AtomicBool::new(false),
            listening: Mutex::new(Some(Instance::new(first))),
        })
    }

    pub(crate) fn accept(&self) -> io::Result<Stream> {
        let mut listening = self
            .listening
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut instance = match listening.take() {
            Some(instance) => instance,
            None => Instance::new(self.create()?),
        };
        let wait = if self.nonblocking.load(Ordering::SeqCst) {
            0
        } else {
            INFINITE
        };
        if let Err(error) = instance.wait_connected(wait) {
            // A still-pending instance stays listening; a failed one is
            // dropped and replaced on the next call.
            if error.kind() == io::ErrorKind::WouldBlock {
                *listening = Some(instance);
            }
            return Err(error);
        }
        // Keep the name served before handing off, so a client arriving now
        // sees a listening (or busy) pipe rather than none. If that fails,
        // hand off anyway; the next accept retries.
        *listening = self.create().ok().map(Instance::new);
        Ok(Stream::from_handle(instance.handle))
    }

    pub(crate) fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }

    fn create(&self) -> io::Result<OwnedHandle> {
        create_instance(&self.name, &self.path, false, &self.security)
    }
}

/// A server instance and its in-flight `ConnectNamedPipe`, if any.
/// Field order matters: `connect` is dropped (cancelled) before the handle
/// it refers to is closed.
struct Instance {
    connect: Option<Box<PendingConnect>>,
    handle: OwnedHandle,
}

// SAFETY: the OVERLAPPED inside `connect` is only touched by the thread
// holding the listener's lock; the kernel's use of it is thread-agnostic.
unsafe impl Send for Instance {}

impl Instance {
    fn new(handle: OwnedHandle) -> Instance {
        Instance {
            connect: None,
            handle,
        }
    }

    /// Wait up to `wait` ms for a client; `WouldBlock` when none arrived.
    fn wait_connected(&mut self, wait: u32) -> io::Result<()> {
        let handle = self.handle.as_raw_handle();
        let mut connect = match self.connect.take() {
            Some(connect) => connect,
            None => match PendingConnect::start(handle)? {
                Some(connect) => connect,
                None => return Ok(()),
            },
        };
        // SAFETY: the event handle is valid for the call.
        match unsafe { WaitForSingleObject(connect.event.raw(), wait) } {
            WAIT_OBJECT_0 => {}
            waited => {
                let error = if waited == WAIT_TIMEOUT {
                    io::ErrorKind::WouldBlock.into()
                } else {
                    io::Error::last_os_error()
                };
                // Still in flight: keep it for the next call (or `Drop`).
                self.connect = Some(connect);
                return Err(error);
            }
        }
        connect.finish(handle)
    }
}

/// An overlapped `ConnectNamedPipe` whose `OVERLAPPED` must stay put while
/// the kernel owns it (hence boxed).
struct PendingConnect {
    handle: HANDLE,
    overlapped: OVERLAPPED,
    event: Event,
    in_flight: bool,
}

impl PendingConnect {
    /// Issue the connect. `None` when a client is already connected.
    fn start(handle: HANDLE) -> io::Result<Option<Box<PendingConnect>>> {
        let event = Event::new()?;
        let mut connect = Box::new(PendingConnect {
            handle,
            // SAFETY: an all-zero OVERLAPPED is the documented initial state.
            overlapped: unsafe { std::mem::zeroed() },
            event,
            in_flight: false,
        });
        connect.overlapped.hEvent = connect.event.raw();
        // SAFETY: the boxed OVERLAPPED stays at a fixed address until the
        // operation completes or `Drop` cancels it.
        if unsafe { ConnectNamedPipe(handle, &mut connect.overlapped) } != 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            // Connected between create and ConnectNamedPipe; or it already
            // left, which the handed-off stream reads as EOF.
            Some(ERROR_PIPE_CONNECTED | ERROR_NO_DATA) => Ok(None),
            Some(ERROR_IO_PENDING) => {
                connect.in_flight = true;
                Ok(Some(connect))
            }
            _ => Err(error),
        }
    }

    /// Collect the result of a signalled connect.
    fn finish(&mut self, handle: HANDLE) -> io::Result<()> {
        let mut transferred = 0_u32;
        // SAFETY: the operation completed (its event is signalled).
        let connected =
            unsafe { GetOverlappedResult(handle, &self.overlapped, &mut transferred, 0) };
        let error = io::Error::last_os_error();
        self.in_flight = false;
        if connected != 0 || error.raw_os_error() == Some(ERROR_NO_DATA as i32) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Drop for PendingConnect {
    fn drop(&mut self) {
        if self.in_flight {
            let mut transferred = 0_u32;
            // SAFETY: the pipe handle is still open (see `Instance` field
            // order); waiting for the cancelled result releases the OVERLAPPED.
            unsafe {
                CancelIoEx(self.handle, &self.overlapped);
                GetOverlappedResult(self.handle, &self.overlapped, &mut transferred, 1);
            }
        }
    }
}
