//! herdr session registry.
//!
//! Session enumeration is NOT part of the herdr socket API — a session only
//! knows itself. So the registry shells out to `herdr session list --json`
//! (binary from `$HERDR_BIN_PATH`, else `herdr` on `$PATH`) and caches the
//! parsed result for a few seconds.
//!
//! It also resolves a card/run's `session` field (`Option<&str>`, `None` =
//! default) to a concrete herdr socket path: `None` maps to the daemon's own
//! bound herdr socket, whose session *name* is found by matching `socket_path`
//! (falling back to the synthetic name `"default"` if nothing matches).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use board_core::protocol::SessionInfo;
use serde::Deserialize;

/// One session as reported by `herdr session list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionEntry {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub socket_path: String,
}

#[derive(Debug, Deserialize)]
struct SessionListJson {
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

/// Parse the `herdr session list --json` payload. Kept separate from the shell
/// -out so it can be unit-tested against captured JSON.
pub fn parse_session_list(json: &str) -> anyhow::Result<Vec<SessionEntry>> {
    let parsed: SessionListJson =
        serde_json::from_str(json).context("parsing `herdr session list --json`")?;
    Ok(parsed.sessions)
}

/// A resolved session: the concrete socket to talk to, plus its display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSession {
    pub name: String,
    pub socket: PathBuf,
}

/// How long `herdr session list --json` may run before it is killed. Every
/// caller reaches this through the blocking pool, and a hung herdr must not pin
/// one of those threads forever (the registry is on the path of every request
/// that resolves a session).
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the deadline loop re-checks the child. Short enough to stay
/// invisible next to a normal sub-100ms `session list`.
const FETCH_POLL: Duration = Duration::from_millis(10);

/// Caches `herdr session list --json` for [`SessionRegistry::ttl`].
pub struct SessionRegistry {
    herdr_bin: String,
    /// The daemon's own bound herdr socket (the default session).
    default_socket: PathBuf,
    ttl: Duration,
    /// Bounded wall-clock budget for one shell-out (see [`FETCH_TIMEOUT`]).
    fetch_timeout: Duration,
    cache: Mutex<Option<(Instant, Vec<SessionEntry>)>>,
}

impl SessionRegistry {
    /// Build a registry. `default_socket` is the herdr socket the daemon itself
    /// connects to (`board_herdr::default_socket_path()`).
    pub fn new(default_socket: PathBuf) -> SessionRegistry {
        let herdr_bin = std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "herdr".to_string());
        SessionRegistry {
            herdr_bin,
            default_socket,
            ttl: Duration::from_secs(3),
            fetch_timeout: FETCH_TIMEOUT,
            cache: Mutex::new(None),
        }
    }

    /// The daemon's bound herdr socket (default session).
    pub fn default_socket(&self) -> &Path {
        &self.default_socket
    }

    /// Session list (cached). Errors carry clear context if the CLI fails.
    pub fn list(&self) -> anyhow::Result<Vec<SessionEntry>> {
        {
            let guard = self.cache.lock().unwrap();
            if let Some((at, entries)) = guard.as_ref() {
                if at.elapsed() < self.ttl {
                    return Ok(entries.clone());
                }
            }
        }
        let entries = self.fetch()?;
        *self.cache.lock().unwrap() = Some((Instant::now(), entries.clone()));
        Ok(entries)
    }

    /// Shell out with a bounded deadline.
    ///
    /// `std::process::Command::output` waits forever, and this runs on the
    /// blocking pool for every session-resolving request, so a wedged `herdr`
    /// would leak blocking threads until the pool is exhausted. Stays
    /// synchronous on purpose: the caller is already off the async runtime.
    fn fetch(&self) -> anyhow::Result<Vec<SessionEntry>> {
        let mut child = Command::new(&self.herdr_bin)
            .args(["session", "list", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running `{} session list --json`", self.herdr_bin))?;

        // Drain both pipes concurrently: a child that fills a 64K pipe buffer
        // would otherwise never exit and always hit the deadline below.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let deadline = Instant::now() + self.fetch_timeout;
        let status = loop {
            match child
                .try_wait()
                .with_context(|| format!("waiting for `{} session list --json`", self.herdr_bin))?
            {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    // Kill and reap so the child never becomes a zombie. The
                    // drain threads end on their own once the pipes close; they
                    // are deliberately not joined here so a grandchild holding
                    // the write end cannot extend this deadline.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(anyhow!(
                        "`{} session list --json` timed out after {:?} and was killed",
                        self.herdr_bin,
                        self.fetch_timeout
                    ));
                }
                None => std::thread::sleep(FETCH_POLL),
            }
        };

        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            return Err(anyhow!(
                "`{} session list --json` failed ({}): {}",
                self.herdr_bin,
                status,
                stderr.trim()
            ));
        }
        parse_session_list(&String::from_utf8_lossy(&stdout))
    }

    /// Session list mapped to the protocol [`SessionInfo`] shape.
    pub fn session_infos(&self) -> anyhow::Result<Vec<SessionInfo>> {
        Ok(self
            .list()?
            .into_iter()
            .map(|e| SessionInfo {
                name: e.name,
                default: e.default,
                running: e.running,
            })
            .collect())
    }

    /// Resolve a card/run's `session` to a socket + name.
    ///
    /// - `None` → the daemon's bound socket; name is the entry whose
    ///   `socket_path` matches it, else the synthetic `"default"`.
    /// - `Some(name)` → the matching **running** session's socket; a missing or
    ///   stopped session is an error listing the known running sessions.
    pub fn resolve(&self, session: Option<&str>) -> anyhow::Result<ResolvedSession> {
        let entries = self.list()?;
        match session {
            None => {
                let name = entries
                    .iter()
                    .find(|e| socket_eq(&e.socket_path, &self.default_socket))
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| "default".to_string());
                Ok(ResolvedSession {
                    name,
                    socket: self.default_socket.clone(),
                })
            }
            Some(want) => {
                let entry = entries.iter().find(|e| e.name == want).ok_or_else(|| {
                    anyhow!(
                        "herdr session '{want}' not found; known: {}",
                        known_running(&entries)
                    )
                })?;
                if !entry.running {
                    return Err(anyhow!(
                        "herdr session '{want}' is not running; running: {}",
                        known_running(&entries)
                    ));
                }
                if entry.socket_path.is_empty() {
                    return Err(anyhow!("herdr session '{want}' has no socket_path"));
                }
                Ok(ResolvedSession {
                    name: entry.name.clone(),
                    socket: PathBuf::from(&entry.socket_path),
                })
            }
        }
    }
}

/// Read one child pipe to EOF on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    })
}

fn socket_eq(a: &str, b: &Path) -> bool {
    !a.is_empty() && Path::new(a) == b
}

fn known_running(entries: &[SessionEntry]) -> String {
    let names: Vec<&str> = entries
        .iter()
        .filter(|e| e.running)
        .map(|e| e.name.as_str())
        .collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests;
