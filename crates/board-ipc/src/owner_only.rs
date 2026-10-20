//! Owner-only access for files the board keeps private on Windows.
//!
//! unix callers `chmod 0700`/`0600`; the Windows counterpart is a protected
//! DACL naming only the current user, built by the same code that secures
//! the boardd pipe. It lives here so the daemon and the CLI share one
//! implementation of the Windows security calls.

use std::io;
use std::path::Path;

/// Give the directory at `path` a protected DACL granting full control to
/// the current user only, inherited by every file and directory inside
/// (`D:P(A;OICI;FA;;;<SID>)`). Inherited entries from the parent (such as
/// "Authenticated Users: Modify" under `C:\`) no longer apply, and existing
/// children are re-evaluated against the new DACL.
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    crate::sys::security::restrict_dir_to_owner(path)
}

/// The DACL of `path` in SDDL form, e.g. `D:PAI(A;OICI;FA;;;S-1-5-21-…)`.
pub fn dacl_sddl(path: &Path) -> io::Result<String> {
    crate::sys::security::dacl_sddl(path)
}

/// The current process user's SID in string form (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    crate::sys::security::current_user_sid()
}
