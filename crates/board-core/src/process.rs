//! Resolve a bare program name the way a shell would. On Windows,
//! `Command::new("pi")` only finds `pi.exe`, but npm installs `pi.cmd`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `Command::new` with Windows `PATH` x `PATHEXT` resolution.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(resolve(program))
}

pub fn resolve(program: impl AsRef<OsStr>) -> PathBuf {
    let path_var = std::env::var_os("PATH");
    let pathext = std::env::var("PATHEXT").ok();
    resolve_in(program.as_ref(), path_var.as_deref(), pathext.as_deref())
}

/// Pure core. Unix: identity. Windows: a bare name without extension is
/// searched in each `PATH` dir with each `PATHEXT` suffix; no match (or an
/// explicit path/extension) returns the input unchanged.
pub fn resolve_in(program: &OsStr, path_var: Option<&OsStr>, pathext: Option<&str>) -> PathBuf {
    let program = Path::new(program);
    if !cfg!(windows) || program.extension().is_some() || program.components().count() > 1 {
        return program.to_path_buf();
    }
    let exts = pathext.unwrap_or(".COM;.EXE;.BAT;.CMD");
    for dir in std::env::split_paths(path_var.unwrap_or_default()) {
        for ext in exts.split(';').filter_map(|e| e.strip_prefix('.')) {
            // NTFS is case-insensitive; lower-case keeps the conventional spelling.
            let candidate = dir.join(program).with_extension(ext.to_ascii_lowercase());
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    program.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn resolve_in_finds_cmd_shim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pi.cmd"), "@echo off\r\n").unwrap();
        let resolved = resolve_in(
            OsStr::new("pi"),
            Some(dir.path().as_os_str()),
            Some(".COM;.EXE;.BAT;.CMD"),
        );
        assert_eq!(resolved, dir.path().join("pi.cmd"));
    }

    #[test]
    fn explicit_paths_and_extensions_pass_through() {
        assert_eq!(
            resolve_in(OsStr::new("pi.exe"), None, None),
            PathBuf::from("pi.exe")
        );
        let p = Path::new("dir").join("pi");
        assert_eq!(resolve_in(p.as_os_str(), None, None), p);
    }

    #[test]
    fn unknown_program_is_returned_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_in(
                OsStr::new("nope"),
                Some(dir.path().as_os_str()),
                Some(".CMD")
            ),
            PathBuf::from("nope")
        );
    }
}
