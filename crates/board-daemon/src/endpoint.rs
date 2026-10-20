//! The boardd endpoint: bind it, secure it, release it on clean shutdown.
//!
//! The endpoint is named by `paths::socket_path()` on every platform, and the
//! transport itself is `board_ipc`'s:
//!
//! - **unix** — an AF_UNIX socket file at that path. A stale file is removed
//!   before binding, the socket is made owner-only (`0600`) immediately after,
//!   and the file is removed again on clean shutdown.
//! - **Windows** — the named pipe `\\.\pipe\` + that path, created with an
//!   owner-only DACL by `board_ipc`; nothing is bound on disk. In its place the
//!   daemon writes an identity *marker* file at the path, the same shape Herdr
//!   uses: `<pid>:<nanos>` (see [`Endpoint`]). It is what lets the CLI tell a
//!   live boardd from a stale leftover without a socket inode.

use std::path::{Path, PathBuf};

use anyhow::Context;
use board_ipc::AsyncLocalListener;

/// A bound boardd endpoint. Dropping it does nothing; [`Endpoint::release`]
/// is the clean-shutdown cleanup, exactly as the socket file used to be removed
/// only after the accept loop ended.
///
/// **Windows marker contract** (the CLI implements the client side): after the
/// pipe is bound, the file at the socket path is atomically replaced with the
/// ASCII text `<pid>:<nanos>` — no newline — where `<pid>` is boardd's decimal
/// process id and `<nanos>` the decimal nanoseconds since the Unix epoch at
/// bind time (a per-start token, not a clock to compare against). A marker
/// whose `<pid>` equals the pipe's `GetNamedPipeServerProcessId` names the live
/// daemon; a marker whose pipe is absent is stale. On clean shutdown the daemon
/// deletes the marker only if it still holds exactly the text this start wrote.
#[derive(Debug)]
pub(crate) struct Endpoint {
    path: PathBuf,
    #[cfg(windows)]
    marker: String,
}

/// Bind the daemon endpoint at `socket_path`, creating its directory first.
/// Must run inside the tokio runtime.
pub(crate) fn bind(socket_path: &Path) -> anyhow::Result<(AsyncLocalListener, Endpoint)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Remove any stale socket file first.
        let _ = std::fs::remove_file(socket_path);
        create_parent(socket_path)?;
        let listener = bind_secured_socket(socket_path, |path| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        })?;
        Ok((
            listener,
            Endpoint {
                path: socket_path.to_path_buf(),
            },
        ))
    }
    #[cfg(windows)]
    {
        // The pipe needs no directory, but the marker does.
        create_parent(socket_path)?;
        let listener = AsyncLocalListener::bind(socket_path).map_err(|error| {
            let addr_in_use = error.kind() == std::io::ErrorKind::AddrInUse;
            let error = anyhow::Error::new(error)
                .context(format!("cannot bind the boardd pipe for {socket_path:?}"));
            if addr_in_use {
                error.context("another process already serves this boardd endpoint")
            } else {
                error
            }
        })?;
        let marker = marker_text(std::process::id(), unix_nanos());
        write_marker(socket_path, &marker)
            .with_context(|| format!("cannot write the boardd marker {socket_path:?}"))?;
        Ok((
            listener,
            Endpoint {
                path: socket_path.to_path_buf(),
                marker,
            },
        ))
    }
}

impl Endpoint {
    /// Clean-shutdown cleanup. Leaves running panes alone; only the endpoint's
    /// on-disk trace goes: the socket file (unix) or this start's own marker
    /// (Windows — a marker some later daemon wrote is never deleted).
    pub(crate) fn release(self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.path);
        }
        #[cfg(windows)]
        {
            if std::fs::read_to_string(&self.path).is_ok_and(|text| text == self.marker) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn create_parent(socket_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = socket_path.parent() {
        // Name the directory: without it the failure resurfaces below as an
        // opaque `bind` error about the socket path instead.
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create the boardd socket directory {parent:?}"))?;
    }
    Ok(())
}

/// Bind the daemon socket and make it owner-only. When securing the socket
/// fails, remove the half-created file so a later start is not blocked by a
/// stale socket, then surface the security error. `secure` is injectable so
/// tests can force a permission-change failure deterministically.
///
/// The bind→chmod window is deliberately left un-temporarized: (1) the span is
/// fully synchronous with no await points, so no other task can observe or
/// connect through the socket mid-window; (2) an AF_UNIX `connect` requires
/// write permission on the socket file, and under the default directory umask
/// (0755) group/other lack it; (3) any chmod failure is fail-closed — the
/// half-created file is removed and startup errors. Do NOT "harden" this by
/// binding a temp name and renaming it into place: that would reopen a window
/// where the well-known path does not yet point at the secured file.
#[cfg(unix)]
fn bind_secured_socket(
    socket_path: &Path,
    secure: impl FnOnce(&Path) -> std::io::Result<()>,
) -> anyhow::Result<AsyncLocalListener> {
    let listener = AsyncLocalListener::bind(socket_path)
        .with_context(|| format!("cannot bind the boardd socket {socket_path:?}"))?;
    if let Err(error) = secure(socket_path) {
        let _ = std::fs::remove_file(socket_path);
        return Err(error)
            .with_context(|| format!("cannot secure the boardd socket {socket_path:?}"));
    }
    Ok(listener)
}

/// The marker text: `<pid>:<nanos>`, Herdr's own marker shape.
#[cfg(windows)]
fn marker_text(pid: u32, nanos: u128) -> String {
    format!("{pid}:{nanos}")
}

#[cfg(windows)]
fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Atomically replace the file at `path` with `text`: written to a sibling
/// temp file and renamed over, so a reader never sees a partial marker and a
/// stale one from a crashed start is simply replaced.
#[cfg(windows)]
fn write_marker(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::Builder::new()
        .prefix(".boardd-marker")
        .tempfile_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.flush()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_socket_securing_removes_the_partial_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("boardd.sock");

        let error = bind_secured_socket(&socket_path, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected chmod failure",
            ))
        })
        .expect_err("a failed security step must fail startup");

        assert!(
            error
                .to_string()
                .contains("cannot secure the boardd socket"),
            "error must name the security step: {error}"
        );
        assert!(
            !socket_path.exists(),
            "the half-created socket must be removed after a security failure"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_bind_replaces_a_stale_socket_secures_it_and_release_removes_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("run").join("boardd.sock");
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        std::fs::write(&socket_path, "stale").unwrap();

        let (_listener, endpoint) = bind(&socket_path).unwrap();
        let metadata = std::fs::metadata(&socket_path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        board_ipc::LocalStream::connect(&socket_path).expect("the bound socket accepts");

        endpoint.release();
        assert!(!socket_path.exists());
    }

    #[test]
    fn bind_names_an_uncreatable_directory() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();

        let error = bind(&blocker.join("boardd.sock")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot create the boardd socket directory"),
            "{error}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_bind_writes_a_pid_marker_naming_the_pipe_server() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("board").join("boardd.sock");

        let (_listener, endpoint) = bind(&socket_path).unwrap();
        let marker = std::fs::read_to_string(&socket_path).unwrap();
        let (pid, nanos) = marker.split_once(':').expect("<pid>:<nanos>");
        let pid: u32 = pid.parse().unwrap();
        assert!(nanos.parse::<u128>().unwrap() > 0, "{marker}");
        assert!(!marker.ends_with('\n'), "{marker:?}");
        assert_eq!(pid, std::process::id());

        let client = board_ipc::LocalStream::connect(&socket_path).unwrap();
        assert_eq!(client.server_process_id().unwrap(), pid);

        endpoint.release();
        assert!(!socket_path.exists(), "clean shutdown removes its marker");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_bind_replaces_a_stale_marker() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("boardd.sock");
        std::fs::write(&socket_path, "1:1").unwrap();

        let (_listener, _endpoint) = bind(&socket_path).unwrap();
        let marker = std::fs::read_to_string(&socket_path).unwrap();
        assert!(
            marker.starts_with(&format!("{}:", std::process::id())),
            "{marker}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_second_bind_fails_and_leaves_the_live_marker() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("boardd.sock");

        let (_listener, _endpoint) = bind(&socket_path).unwrap();
        let live = std::fs::read_to_string(&socket_path).unwrap();

        let error = bind(&socket_path).unwrap_err();
        assert!(
            format!("{error:#}").contains("already serves this boardd endpoint"),
            "{error:#}"
        );
        assert_eq!(std::fs::read_to_string(&socket_path).unwrap(), live);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_release_keeps_a_marker_another_start_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("boardd.sock");

        let (listener, endpoint) = bind(&socket_path).unwrap();
        drop(listener);
        std::fs::write(&socket_path, "4242:99").unwrap();

        endpoint.release();
        assert_eq!(std::fs::read_to_string(&socket_path).unwrap(), "4242:99");
    }
}
