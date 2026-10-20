//! Windows process-tree ownership for [`super::LocalSpawner`].
//!
//! Killing a child on Windows ends only that one process: a `.cmd` shim's
//! `cmd.exe` dies while the `node` it started keeps running. Each local child
//! is therefore placed in its own anonymous Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so [`ProcessTree::terminate`] (and
//! dropping the tree) ends every process the child started.
//!
//! The child is assigned right after `spawn` returns. A grandchild started in
//! that window would escape the job; a shim has not reached its first
//! `CreateProcess` by then in practice, and `std` offers no way to spawn
//! suspended and resume.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// One child's Job Object. Dropping it closes the last handle, which kills
/// whatever is still running in the job.
#[derive(Debug)]
pub(crate) struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    /// Create a kill-on-close job and assign `child` to it.
    pub(crate) fn adopt(child: &Child) -> io::Result<ProcessTree> {
        // SAFETY: both pointer arguments may be null (default security, no
        // name); a null return is the documented failure value.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a freshly created handle that nothing else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a properly sized, initialised struct of the
        // class named, and outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles are open for the duration of the call; `Child`
        // keeps its process handle until it is dropped.
        let ok = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ProcessTree { job })
    }

    /// Terminate every process in the job, the child included.
    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job handle is open for as long as `self` lives.
        let ok = unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
