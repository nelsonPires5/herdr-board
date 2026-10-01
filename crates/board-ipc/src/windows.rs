//! Named-pipe transport. All Win32 `unsafe` for the workspace lives here.
//!
//! Reads call `ReadFile` only after `PeekNamedPipe` reports data or EOF: I/O on
//! a synchronous pipe handle is serialized per file object, so a blocking read
//! would stall a write on a cloned handle. Timeouts and non-blocking mode are
//! emulated with a peek loop.
// ponytail: peek polling adds up to ~20 ms latency on idle streams, and a
// blocked WriteFile (server not reading, pipe buffer full) still serializes a
// concurrent peek; switch to overlapped I/O if either ever matters.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PeekNamedPipe, WaitNamedPipeW,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const DEFAULT_CONNECT: Duration = Duration::from_secs(2);
const BUFFER: u32 = 64 * 1024;

/// `\\.\pipe\` + the verbatim socket path — Herdr's Windows convention.
pub fn pipe_name(path: &Path) -> OsString {
    let mut name = OsString::from(r"\\.\pipe\");
    name.push(path.as_os_str());
    name
}

/// Whether a server currently serves the pipe for `path`, without connecting
/// (a busy pipe still exists; only `ERROR_FILE_NOT_FOUND` means absent).
pub fn endpoint_exists(path: &Path) -> bool {
    // SAFETY: NUL-terminated UTF-16 name alive for the call.
    if unsafe { WaitNamedPipeW(wide(&pipe_name(path)).as_ptr(), 1) } != 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32)
}

fn wide(name: &OsString) -> Vec<u16> {
    name.encode_wide().chain(Some(0)).collect()
}

fn is_eof(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == ERROR_BROKEN_PIPE as i32
            || code == ERROR_PIPE_NOT_CONNECTED as i32
            || code == ERROR_NO_DATA as i32
    )
}

/// Settings shared by clones, like `SO_RCVTIMEO` / `O_NONBLOCK` on one fd.
#[derive(Default)]
struct Shared {
    /// Read timeout in ms; 0 = none.
    read_timeout_ms: AtomicU64,
    nonblocking: AtomicBool,
}

pub struct PipeStream {
    file: File,
    shared: Arc<Shared>,
}

/// Connect to the named pipe for `path`, waiting up to `timeout` while every
/// server instance is busy, then verify the server runs as the current user.
pub fn connect_timeout(path: &Path, timeout: Duration) -> io::Result<PipeStream> {
    let name = pipe_name(path);
    let deadline = Instant::now() + timeout;
    loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            // Identification only: the server may learn who we are but not
            // impersonate us.
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
        {
            Ok(file) => {
                verify_same_user(&file)?;
                return Ok(PipeStream::from_file(file));
            }
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "named pipe busy"));
                }
                let ms = left.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32;
                // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive for the call.
                unsafe { WaitNamedPipeW(wide(&name).as_ptr(), ms) };
            }
            Err(error) => return Err(error),
        }
    }
}

impl PipeStream {
    fn from_file(file: File) -> Self {
        Self {
            file,
            shared: Arc::default(),
        }
    }

    /// Generic like `UnixStream::connect`, so callers compile on both platforms.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        connect_timeout(path.as_ref(), DEFAULT_CONNECT)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            shared: Arc::clone(&self.shared),
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let ms = match timeout {
            Some(t) if t.is_zero() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "zero read timeout",
                ))
            }
            Some(t) => t.as_millis().clamp(1, u128::from(u64::MAX)) as u64,
            None => 0,
        };
        self.shared.read_timeout_ms.store(ms, Ordering::Relaxed);
        Ok(())
    }

    /// Accepted for API parity with `UnixStream`; pipe writes are not bounded.
    // ponytail: writes block only when the peer stops reading and the 64 KiB
    // pipe buffer is full; bound them with overlapped I/O if a hung peer shows up.
    pub fn set_write_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.shared
            .nonblocking
            .store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    /// `Ok(true)` when data or EOF is available, `Ok(false)` on timeout.
    /// `None` waits forever.
    pub fn wait_readable(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut nap = Duration::from_millis(1);
        loop {
            if self.peek_ready()? {
                return Ok(true);
            }
            let sleep = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(false);
                    }
                    nap.min(left)
                }
                None => nap,
            };
            std::thread::sleep(sleep);
            nap = (nap * 2).min(Duration::from_millis(20));
        }
    }

    fn peek_ready(&self) -> io::Result<bool> {
        let mut available = 0u32;
        // SAFETY: valid pipe handle owned by `self.file`; null buffer with size 0
        // asks only for the available byte count.
        let ok = unsafe {
            PeekNamedPipe(
                self.file.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            return if is_eof(&error) { Ok(true) } else { Err(error) };
        }
        Ok(available > 0)
    }
}

impl Read for &PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let nonblocking = self.shared.nonblocking.load(Ordering::Relaxed);
        let timeout = match (
            nonblocking,
            self.shared.read_timeout_ms.load(Ordering::Relaxed),
        ) {
            (true, _) => Some(Duration::ZERO),
            (false, 0) => None,
            (false, ms) => Some(Duration::from_millis(ms)),
        };
        if !self.wait_readable(timeout)? {
            return Err(if nonblocking {
                io::ErrorKind::WouldBlock.into()
            } else {
                io::Error::new(io::ErrorKind::TimedOut, "named pipe read timed out")
            });
        }
        match (&self.file).read(buf) {
            Err(error) if is_eof(&error) => Ok(0),
            other => other,
        }
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl Write for &PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.file).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Server side of a named pipe; `accept` hands out one connected instance at a
/// time and immediately creates the next one.
pub struct PipeListener {
    name: OsString,
    next: Mutex<File>,
}

impl PipeListener {
    /// Fails if any process already serves this name (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let name = pipe_name(path.as_ref());
        let first = create_instance(&name, true)?;
        Ok(Self {
            name,
            next: Mutex::new(first),
        })
    }

    pub fn accept(&self) -> io::Result<(PipeStream, ())> {
        let mut next = self
            .next
            .lock()
            .map_err(|_| io::Error::other("listener poisoned"))?;
        // SAFETY: valid server pipe handle; synchronous (no OVERLAPPED).
        let ok = unsafe { ConnectNamedPipe(next.as_raw_handle() as HANDLE, std::ptr::null_mut()) };
        if ok == 0 {
            let error = io::Error::last_os_error();
            // PIPE_CONNECTED: the client arrived first. NO_DATA: it arrived and
            // already left — still an accepted connection that reads EOF, as
            // with a Unix `accept`.
            if !matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_PIPE_CONNECTED as i32 || code == ERROR_NO_DATA as i32
            ) {
                return Err(error);
            }
        }
        let connected = std::mem::replace(&mut *next, create_instance(&self.name, false)?);
        Ok((PipeStream::from_file(connected), ()))
    }

    pub fn incoming(&self) -> impl Iterator<Item = io::Result<PipeStream>> + '_ {
        std::iter::from_fn(move || Some(self.accept().map(|(stream, ())| stream)))
    }
}

fn create_instance(name: &OsString, first: bool) -> io::Result<File> {
    let open_mode = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: NUL-terminated name; null security attributes = default DACL
    // (write access for creator, SYSTEM, Administrators only).
    let handle = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fresh, valid, exclusively owned handle.
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

/// Reject a pipe whose server process runs as another user (pipe squatting:
/// the `\\.\pipe\` namespace is global).
fn verify_same_user(file: &File) -> io::Result<()> {
    let mut server_pid = 0u32;
    // SAFETY: valid client pipe handle and out pointer.
    if unsafe { GetNamedPipeServerProcessId(file.as_raw_handle() as HANDLE, &mut server_pid) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    if user_sid(server_pid)? != user_sid(me)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "named pipe server runs as a different user",
        ));
    }
    Ok(())
}

fn user_sid(pid: u32) -> io::Result<Vec<u8>> {
    // SAFETY: every handle is checked, then owned by an `OwnedHandle` that
    // closes it; buffers outlive the calls that fill them.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle::from_raw_handle(process as RawHandle);
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process.as_raw_handle() as HANDLE, TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::from_raw_handle(token as RawHandle);
        let mut len = 0u32;
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut len,
        );
        // u64 storage keeps TOKEN_USER's pointer field aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &mut len,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let sid_len = GetLengthSid(sid) as usize;
        Ok(std::slice::from_raw_parts(sid.cast::<u8>(), sid_len).to_vec())
    }
}
