//! Detached process spawn that inherits only the handles it names.
//!
//! `std::process::Command` on Windows always calls `CreateProcessW` with
//! `bInheritHandles = TRUE`, which copies *every* inheritable handle into the
//! child — including a caller's stdout pipe that the CLI itself inherited. A
//! long-lived detached daemon would then hold that pipe open and a piped
//! `board …` would never see EOF. `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limits
//! inheritance to exactly stdin/stdout (NUL) and the given stderr file.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::{
    CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    UpdateProcThreadAttribute, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// Spawn `exe args…` with a windowless console, in its own process group,
/// stdin/stdout on NUL and stderr on `stderr`. Returns the child's PID. `args`
/// are plain tokens (no quoting is applied beyond the executable path).
///
/// `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`: console programs the daemon
/// later runs inherit this hidden console instead of each popping a window.
pub fn spawn_detached(exe: &Path, args: &[&str], stderr: File) -> io::Result<u32> {
    let nul_in = OpenOptions::new().read(true).open("NUL")?;
    let nul_out = OpenOptions::new().write(true).open("NUL")?;
    let handles: [HANDLE; 3] = [
        nul_in.as_raw_handle() as HANDLE,
        nul_out.as_raw_handle() as HANDLE,
        stderr.as_raw_handle() as HANDLE,
    ];
    for handle in handles {
        // SAFETY: each handle is owned by a live `File` above.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }

    let mut command_line: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(args.iter().flat_map(|arg| {
            std::iter::once(u16::from(b' ')).chain(std::ffi::OsStr::new(arg).encode_wide())
        }))
        .chain(Some(0))
        .collect();

    // SAFETY: the attribute list lives in `list_buf` (u64-aligned) for the whole
    // call and is deleted on every path after a successful init; `handles`
    // outlives `CreateProcessW`, which copies what it needs.
    unsafe {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut list_buf = vec![0u64; size.div_ceil(8)];
        let list = list_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            if UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(&handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = handles[0];
            startup.StartupInfo.hStdOutput = handles[1];
            startup.StartupInfo.hStdError = handles[2];
            startup.lpAttributeList = list;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessW(
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
                std::ptr::null(),
                std::ptr::null(),
                &startup.StartupInfo,
                &mut info,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            Ok(info.dwProcessId)
        })();
        DeleteProcThreadAttributeList(list);
        result
    }
}
