//! Security descriptors and identities: the owner-only DACL every server
//! instance is created with, the check every client makes that the server it
//! reached belongs to this user, and the owner-only directory DACL.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, GetSecurityInfo,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Owners only an elevated administrator (or the system) can give an
/// object: BUILTIN\Administrators is the default owner of what an elevated
/// process creates, LocalSystem that of a service. A non-administrator cannot
/// assign either, and an administrator can already act as any user.
const PRIVILEGED_OWNERS: &[&str] = &["S-1-5-32-544", "S-1-5-18"];

/// A security descriptor built from SDDL, freed on drop.
#[derive(Debug)]
struct Descriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable after construction and only read by
// the kernel; it is freed exactly once in `Drop`.
unsafe impl Send for Descriptor {}
// SAFETY: as above — shared access never mutates the descriptor.
unsafe impl Sync for Descriptor {}

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Descriptor> {
        let sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; on success `descriptor` receives a
        // LocalAlloc'd buffer that `Drop` frees.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
        // (or Get*SecurityInfo) and freed only here.
        unsafe { LocalFree(self.0) };
    }
}

/// A protected DACL granting `GENERIC_ALL` to the current user's SID and
/// nobody else (`D:P(A;;GA;;;<SID>)`) — the named-pipe counterpart of the
/// unix socket's `0600`.
#[derive(Debug)]
pub(crate) struct OwnerOnly {
    descriptor: Descriptor,
}

impl OwnerOnly {
    pub(crate) fn new() -> io::Result<OwnerOnly> {
        let sid = current_user_sid()?;
        Descriptor::from_sddl(&format!("D:P(A;;GA;;;{sid})"))
            .map(|descriptor| OwnerOnly { descriptor })
    }

    /// Non-inheritable attributes pointing at this descriptor; valid while
    /// `self` lives.
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        }
    }
}

/// Refuse a pipe server that does not belong to this user.
///
/// Pipe names share one machine-wide namespace, so another local user can
/// create a predictable name first; the owner-only DACL only protects the
/// server side. The pipe object's owner is whoever created its first
/// instance, and a non-administrator can only own objects as themselves, so
/// an owner of this user (or of a privileged principal, see
/// [`PRIVILEGED_OWNERS`]) is trusted. Otherwise the server process's token
/// user decides. Anything else fails with `PermissionDenied`, which is never
/// "absent", so callers treat the endpoint as occupied rather than stale.
pub(crate) fn verify_server(handle: HANDLE, path: &Path) -> io::Result<()> {
    let me = current_user_sid()?;
    authorize_server(&me, object_owner_sid(handle), || {
        process_user_sid(server_process_id(handle)?)
    })
    .map_err(|reason| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing the pipe server at {}: {reason}; another user may have created this \
                 endpoint first",
                path.display()
            ),
        )
    })
}

/// The decision behind [`verify_server`], with the lookups injected: `Ok`
/// when `owner` is `me` or a privileged principal, or else when the server
/// process runs as `me`; otherwise why not.
pub(crate) fn authorize_server(
    me: &str,
    owner: io::Result<String>,
    process_user: impl FnOnce() -> io::Result<String>,
) -> Result<(), String> {
    let owner_text = match &owner {
        Ok(owner) if owner == me || PRIVILEGED_OWNERS.contains(&owner.as_str()) => return Ok(()),
        Ok(owner) => format!("it is owned by {owner}"),
        Err(error) => format!("its owner could not be read ({error})"),
    };
    match process_user() {
        Ok(user) if user == me => Ok(()),
        Ok(user) => Err(format!("{owner_text} and its process runs as {user}")),
        Err(error) => Err(format!(
            "{owner_text} and its process user could not be read ({error})"
        )),
    }
}

pub(crate) fn server_process_id(handle: HANDLE) -> io::Result<u32> {
    let mut pid = 0_u32;
    // SAFETY: the handle is a valid pipe handle for the call.
    if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// The owner SID of the kernel object behind `handle`.
fn object_owner_sid(handle: HANDLE) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is valid for the call (opened with READ_CONTROL via
    // GENERIC_READ); on success `descriptor` is LocalAlloc'd and `owner`
    // points into it, so it is read before the descriptor is freed.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(descriptor);
    if owner.is_null() {
        return Err(io::Error::other("the object has no owner"));
    }
    let sid = sid_string(owner);
    drop(descriptor);
    sid
}

/// The user SID of process `pid`.
fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: OpenProcess returns a fresh handle (or null); it is owned
    // exactly once below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh, valid handle owned by nobody else.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    token_user_sid(process.as_raw_handle())
}

/// The current process user's SID in string form (`S-1-5-21-…`), looked up
/// once.
pub(crate) fn current_user_sid() -> io::Result<String> {
    static CURRENT: OnceLock<String> = OnceLock::new();
    if let Some(sid) = CURRENT.get() {
        return Ok(sid.clone());
    }
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing.
    let sid = token_user_sid(unsafe { GetCurrentProcess() })?;
    Ok(CURRENT.get_or_init(|| sid).clone())
}

/// The token user of `process` (a handle with query access).
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: `process` is valid for the call; the token handle is owned
    // exactly once by `OwnedHandle`.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a fresh, valid handle owned by nobody else.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut len = 0_u32;
    // SAFETY: a null buffer with length 0 only queries the needed size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut len,
        )
    };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps the TOKEN_USER (pointer-aligned) correctly aligned.
    let mut buffer = vec![0_u64; (len as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `len` writable, aligned bytes.
    let queried = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    };
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation(TokenUser) filled `buffer` with a TOKEN_USER
    // whose SID pointer refers into the same buffer, alive for this scope.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid)
}

/// `sid` (valid for the call) in string form.
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut string = ptr::null_mut();
    // SAFETY: the caller guarantees `sid` is valid; on success `string` is a
    // LocalAlloc'd, NUL-terminated UTF-16 string freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `string` is NUL-terminated (above); it is read up to the NUL
    // and then freed exactly once.
    Ok(unsafe { take_local_wide(string) })
}

/// Read a LocalAlloc'd NUL-terminated UTF-16 string, then free it.
///
/// # Safety
/// `string` must be a valid, NUL-terminated, LocalAlloc'd buffer that nothing
/// else frees.
unsafe fn take_local_wide(string: *mut u16) -> String {
    // SAFETY: per the contract above.
    unsafe {
        let len = (0..).take_while(|&i| *string.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(string, len));
        LocalFree(string.cast());
        text
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Replace the DACL of the directory `path` with a protected one granting
/// full control to the current user only, inherited by everything inside
/// (`D:P(A;OICI;FA;;;<SID>)`) — the directory counterpart of `chmod 0700`.
/// Existing children are re-evaluated against it, so their inherited access
/// narrows too.
pub(crate) fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
    let sid = current_user_sid()?;
    let descriptor = Descriptor::from_sddl(&format!("D:P(A;OICI;FA;;;{sid})"))?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = ptr::null_mut();
    // SAFETY: `descriptor` is valid; `dacl` points into it and is only used
    // while `descriptor` lives.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let name = wide_path(path);
    // SAFETY: `name` is NUL-terminated and `dacl` is valid (above).
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// The DACL of the file or directory `path` in SDDL form (e.g.
/// `D:PAI(A;OICI;FA;;;S-1-5-21-…)`), for checks and diagnostics.
pub(crate) fn dacl_sddl(path: &Path) -> io::Result<String> {
    let name = wide_path(path);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; on success `descriptor` is
    // LocalAlloc'd and freed by `Descriptor`.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(descriptor);
    let mut string = ptr::null_mut();
    // SAFETY: `descriptor` is valid; on success `string` is a LocalAlloc'd,
    // NUL-terminated UTF-16 string.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut string,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `string` is NUL-terminated and freed only here.
    Ok(unsafe { take_local_wide(string) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1-2-3-1001";
    const OTHER: &str = "S-1-5-21-1-2-3-1002";

    fn never() -> io::Result<String> {
        panic!("an owner match must not need the process lookup")
    }

    #[test]
    fn a_server_owned_by_this_user_is_trusted() {
        assert_eq!(authorize_server(ME, Ok(ME.to_string()), never), Ok(()));
    }

    #[test]
    fn a_privileged_owner_is_trusted() {
        for owner in PRIVILEGED_OWNERS {
            assert_eq!(authorize_server(ME, Ok(owner.to_string()), never), Ok(()));
        }
    }

    #[test]
    fn a_foreign_owner_is_trusted_only_when_the_process_runs_as_this_user() {
        assert_eq!(
            authorize_server(ME, Ok(OTHER.to_string()), || Ok(ME.to_string())),
            Ok(())
        );
        let refused = authorize_server(ME, Ok(OTHER.to_string()), || Ok(OTHER.to_string()))
            .expect_err("another user's pipe");
        assert!(refused.contains(OTHER), "{refused}");
        let refused = authorize_server(ME, Ok(OTHER.to_string()), || {
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .expect_err("an unreadable foreign process");
        assert!(refused.contains("could not be read"), "{refused}");
    }

    #[test]
    fn unreadable_identity_fails_closed() {
        let refused = authorize_server(ME, Err(io::ErrorKind::PermissionDenied.into()), || {
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .expect_err("nothing could be verified");
        assert!(refused.contains("owner could not be read"), "{refused}");
    }

    #[test]
    fn the_current_user_sid_is_stable() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(current_user_sid().unwrap(), sid);
    }
}
