//! Overlapped named-pipe stream (client side and accepted server side).

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED, ERROR_SEM_TIMEOUT, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeW};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::{wait_millis, Event};

/// One pipe connection; clones share the handle and its settings, like
/// dup'd unix descriptors share `SO_RCVTIMEO`/`O_NONBLOCK`.
#[derive(Debug, Clone)]
pub(crate) struct Stream {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    handle: OwnedHandle,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
    nonblocking: AtomicBool,
}

impl Stream {
    pub(super) fn from_handle(handle: OwnedHandle) -> Stream {
        Stream {
            shared: Arc::new(Shared {
                handle,
                read_timeout: Mutex::new(None),
                write_timeout: Mutex::new(None),
                nonblocking: AtomicBool::new(false),
            }),
        }
    }

    /// Open the pipe for `path`, retrying `ERROR_PIPE_BUSY` (a live server
    /// between accepts) via `WaitNamedPipeW` until `timeout` expires.
    pub(crate) fn connect_with_deadline(path: &Path, timeout: Duration) -> io::Result<Stream> {
        let name = crate::endpoint::wide_pipe_name(path)?;
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: `name` is NUL-terminated; the returned handle is owned
            // exactly once below. SECURITY_IDENTIFICATION stops the server
            // from impersonating this client beyond identifying it.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                // SAFETY: a fresh, valid handle owned by nobody else.
                let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
                // Nothing is sent before the server is known to be ours.
                super::security::verify_server(handle.as_raw_handle(), path)?;
                return Ok(Stream::from_handle(handle));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(connect_timed_out());
            }
            // 0 would mean "the server's default wait"; always pass >= 1ms.
            let millis = wait_millis(Some(remaining)).max(1);
            // SAFETY: `name` is NUL-terminated.
            if unsafe { WaitNamedPipeW(name.as_ptr(), millis) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_SEM_TIMEOUT as i32) {
                    return Err(error);
                }
            }
        }
    }

    pub(crate) fn try_clone(&self) -> io::Result<Stream> {
        Ok(self.clone())
    }

    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        store_timeout(&self.shared.read_timeout, timeout)
    }

    pub(crate) fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        store_timeout(&self.shared.write_timeout, timeout)
    }

    pub(crate) fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.shared.nonblocking.store(nonblocking, Ordering::SeqCst);
        Ok(())
    }

    /// Pipes have no readiness signal, but a zero-byte overlapped read on a
    /// byte-mode pipe stays pending until data is buffered or the peer
    /// closes, and consumes nothing (libuv's pipe-readiness technique). So
    /// the wait is one kernel wait — no polling, also for `None`.
    pub(crate) fn poll_read_ready(&self, timeout: Option<Duration>) -> io::Result<bool> {
        // A zero timeout still waits ~1ms, as unix `poll` did for board-herdr.
        let timeout = timeout.map(|timeout| timeout.max(Duration::from_millis(1)));
        let handle = self.raw();
        let mut probe = [0_u8; 1];
        let ptr = probe.as_mut_ptr();
        // SAFETY: `probe` outlives the operation — `overlapped` does not
        // return until the read completed or its cancellation was observed.
        let result = overlapped(
            handle,
            timeout,
            io::ErrorKind::TimedOut,
            |overlapped| unsafe { ReadFile(handle, ptr, 0, ptr::null_mut(), overlapped) },
        );
        match result {
            Ok(_) => Ok(true),
            Err(error) if is_closed(&error) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn server_process_id(&self) -> io::Result<u32> {
        super::security::server_process_id(self.raw())
    }

    pub(crate) fn as_handle(&self) -> BorrowedHandle<'_> {
        self.shared.handle.as_handle()
    }

    pub(crate) fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (len, timeout, expired) = if self.shared.nonblocking.load(Ordering::SeqCst) {
            match self.available()? {
                None => return Ok(0),
                Some(0) => return Err(io::ErrorKind::WouldBlock.into()),
                // Data is buffered, so the read completes at once; the zero
                // wait only matters if another clone drains it first.
                Some(available) => (
                    buf.len().min(available as usize),
                    Some(Duration::ZERO),
                    io::ErrorKind::WouldBlock,
                ),
            }
        } else {
            (
                buf.len(),
                load_timeout(&self.shared.read_timeout),
                io::ErrorKind::TimedOut,
            )
        };
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        let handle = self.raw();
        let ptr = buf.as_mut_ptr();
        // SAFETY: `buf` outlives the operation — `overlapped` does not return
        // until the read completed or its cancellation was observed.
        let result = overlapped(handle, timeout, expired, |overlapped| unsafe {
            ReadFile(handle, ptr, len, ptr::null_mut(), overlapped)
        });
        match result {
            Err(error) if is_closed(&error) => Ok(0),
            other => other,
        }
    }

    /// Writes ignore nonblocking mode and are bounded by the write timeout:
    /// a pipe write completes once the data is buffered.
    pub(crate) fn write(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let handle = self.raw();
        let timeout = load_timeout(&self.shared.write_timeout);
        // SAFETY: as in `read`, `buf` outlives the operation.
        overlapped(
            handle,
            timeout,
            io::ErrorKind::TimedOut,
            |overlapped| unsafe {
                WriteFile(handle, buf.as_ptr(), len, ptr::null_mut(), overlapped)
            },
        )
    }

    /// Bytes waiting in the pipe, or `None` when the peer has closed.
    fn available(&self) -> io::Result<Option<u32>> {
        let mut available = 0_u32;
        // SAFETY: a null buffer of size 0 only reports the available count.
        let peeked = unsafe {
            PeekNamedPipe(
                self.raw(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = io::Error::last_os_error();
            return if is_closed(&error) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Ok(Some(available))
    }

    fn raw(&self) -> HANDLE {
        self.shared.handle.as_raw_handle()
    }
}

/// Run one overlapped operation started by `start` to completion, or until
/// `timeout` expires (then fail with `expired`). Always waits for the
/// operation to finish or be cancelled before returning, so the caller's
/// buffer and the `OVERLAPPED` here are never used after return.
fn overlapped(
    handle: HANDLE,
    timeout: Option<Duration>,
    expired: io::ErrorKind,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<usize> {
    let event = Event::new()?;
    // SAFETY: an all-zero OVERLAPPED is the documented initial state.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.raw();
    if start(&mut overlapped) == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(error);
        }
        // SAFETY: `event` is a valid handle for the call.
        let waited = unsafe { WaitForSingleObject(event.raw(), wait_millis(timeout)) };
        if waited != WAIT_OBJECT_0 {
            let wait_error = (waited != WAIT_TIMEOUT).then(io::Error::last_os_error);
            let mut transferred = 0_u32;
            // SAFETY: `overlapped` belongs to the in-flight operation on
            // `handle`; waiting (bWait = TRUE) for its result after the cancel
            // guarantees the kernel is done with it.
            let completed = unsafe {
                CancelIoEx(handle, &overlapped);
                GetOverlappedResult(handle, &overlapped, &mut transferred, 1)
            };
            if completed != 0 {
                // Finished before the cancel took effect.
                return Ok(transferred as usize);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_OPERATION_ABORTED as i32) {
                return Err(error);
            }
            // A cancelled write may already have moved part of the buffer.
            if overlapped.InternalHigh > 0 {
                return Ok(overlapped.InternalHigh);
            }
            return Err(wait_error.unwrap_or_else(|| expired.into()));
        }
    }
    let mut transferred = 0_u32;
    // SAFETY: the operation has completed (synchronously or signalled).
    if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(transferred as usize)
}

/// The peer closed (or is closing) its end: reads see end-of-file.
fn is_closed(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|code| code as u32),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
    )
}

fn store_timeout(slot: &Mutex<Option<Duration>>, timeout: Option<Duration>) -> io::Result<()> {
    if timeout == Some(Duration::ZERO) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        ));
    }
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = timeout;
    Ok(())
}

fn load_timeout(slot: &Mutex<Option<Duration>>) -> Option<Duration> {
    *slot.lock().unwrap_or_else(PoisonError::into_inner)
}

fn connect_timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "connect deadline expired waiting for a free pipe instance",
    )
}
