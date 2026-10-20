//! Endpoint naming and error classification shared by every transport.

use std::io;
use std::path::Path;

/// Prefix of every local named pipe; the endpoint path follows verbatim.
#[cfg(windows)]
pub const PIPE_PREFIX: &str = r"\\.\pipe\";

/// Longest pipe name Windows accepts, prefix included, in UTF-16 units.
#[cfg(windows)]
pub const MAX_PIPE_NAME_LEN: usize = 256;

/// The named pipe that serves the endpoint at `path`.
///
/// Herdr names its pipe `\\.\pipe\` + `HERDR_SOCKET_PATH` verbatim (the
/// on-disk `.sock` is only a `<pid>:<nanos>` marker file), so the path string
/// is not normalized beyond dropping a `\\?\` verbatim prefix (what
/// `std::fs::canonicalize` adds) — that keeps a canonicalized path and Herdr's
/// plain one naming the same pipe. A path that already is a `\\.\pipe\` name is
/// used as-is. Fails with `InvalidInput` when the result exceeds
/// [`MAX_PIPE_NAME_LEN`].
#[cfg(windows)]
pub fn pipe_name(path: &Path) -> io::Result<std::ffi::OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = PIPE_PREFIX.encode_utf16().collect();
    let name = if starts_with_ascii(&wide, PIPE_PREFIX) {
        wide
    } else {
        let tail = if starts_with_ascii(&wide, r"\\?\UNC\") {
            // `\\?\UNC\server\share` is `\\server\share`.
            let mut tail: Vec<u16> = r"\\".encode_utf16().collect();
            tail.extend_from_slice(&wide[8..]);
            tail
        } else if starts_with_ascii(&wide, r"\\?\") {
            wide[4..].to_vec()
        } else {
            wide
        };
        let mut name = prefix;
        name.extend_from_slice(&tail);
        name
    };
    if name.len() > MAX_PIPE_NAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "endpoint {} is too long: its pipe name is {} UTF-16 units, Windows allows {}",
                path.display(),
                name.len(),
                MAX_PIPE_NAME_LEN
            ),
        ));
    }
    Ok(std::ffi::OsString::from_wide(&name))
}

/// Case-insensitive ASCII prefix match on UTF-16 units.
#[cfg(windows)]
fn starts_with_ascii(wide: &[u16], prefix: &str) -> bool {
    wide.len() >= prefix.len()
        && wide.iter().zip(prefix.bytes()).all(|(&unit, byte)| {
            u8::try_from(unit).is_ok_and(|unit| unit.eq_ignore_ascii_case(&byte))
        })
}

/// `pipe_name` as a NUL-terminated UTF-16 string for the Win32 calls.
#[cfg(windows)]
pub(crate) fn wide_pipe_name(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide: Vec<u16> = pipe_name(path)?.encode_wide().collect();
    wide.push(0);
    Ok(wide)
}

/// Reject a path the platform cannot name an endpoint with, before any
/// system call: `ENAMETOOLONG` when it does not fit `sockaddr_un.sun_path`
/// (unix), `InvalidInput` when the pipe name is too long (Windows).
pub fn check(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let max_path = std::mem::size_of::<libc::sockaddr_un>()
            - std::mem::offset_of!(libc::sockaddr_un, sun_path);
        if path.as_os_str().as_bytes().len() >= max_path {
            return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        pipe_name(path).map(drop)
    }
}

/// Whether a connect error means *nothing is listening* at the endpoint (a
/// stale or never-created socket), as opposed to a live server that refused,
/// timed out, or is busy.
///
/// unix: `NotFound` (no socket file) or `ConnectionRefused` (a socket file
/// with no listener). Windows: `NotFound` (`ERROR_FILE_NOT_FOUND` /
/// `ERROR_PATH_NOT_FOUND`: no pipe instance exists). `ERROR_PIPE_BUSY` is a
/// live server with every instance in use and is never absent.
pub fn is_absent(error: &io::Error) -> bool {
    match error.kind() {
        io::ErrorKind::NotFound => true,
        #[cfg(unix)]
        io::ErrorKind::ConnectionRefused => true,
        _ => false,
    }
}
