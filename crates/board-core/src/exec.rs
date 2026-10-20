//! Program resolution and bounded subprocess runs shared by every crate that
//! shells out to a harness or helper CLI.
//!
//! - [`resolve_program`]: the one place a bare program name (`pi`,
//!   `opencode`, `code`, …) becomes something `std::process::Command` can
//!   spawn. On unix this is the identity — `Command` already searches `PATH`
//!   with `execvp` semantics, and changing that would change behaviour. On
//!   Windows `Command` only finds `<name>.exe`, while npm-style installs ship
//!   `<name>.cmd` shims; the `which` crate applies `PATHEXT` so those resolve.
//! - [`run_bounded`]: run an argv with a wall-clock budget and a stdout cap,
//!   so a hung or chatty CLI cannot block a caller forever or exhaust memory.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Resolve a program name or path to what should be handed to
/// [`Command::new`].
///
/// - **unix**: always `Some(program)` unchanged (identity), so `Command`
///   keeps its native `PATH` search and a missing binary still surfaces as a
///   spawn error exactly as before.
/// - **Windows**: the `PATH`/`PATHEXT` lookup a shell would do (`opencode` →
///   `…\opencode.cmd`, `code` → `…\code.cmd`, `C:\x\tool` → `C:\x\tool.exe`);
///   `None` when nothing matches.
///
/// An empty or whitespace-only name never resolves.
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.trim().is_empty() {
        return None;
    }
    resolve_platform(program)
}

#[cfg(unix)]
fn resolve_platform(program: &str) -> Option<PathBuf> {
    Some(PathBuf::from(program))
}

#[cfg(windows)]
fn resolve_platform(program: &str) -> Option<PathBuf> {
    which::which(program).ok()
}

/// A [`Command`] for `program`, resolved through [`resolve_program`]. An
/// unresolvable name falls back to the bare name so the spawn reports the
/// usual not-found error instead of the caller needing a second error path.
pub fn command(program: &str) -> Command {
    match resolve_program(program) {
        Some(path) => Command::new(path),
        None => Command::new(program),
    }
}

/// What a bounded run produced: the child's exit status and its full stdout.
#[derive(Debug)]
pub struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
}

/// Poll interval while reaping a child whose stdout already reached EOF.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Run `argv` (program resolved via [`resolve_program`]) with stdin/stderr
/// null and stdout captured, bounded in both dimensions a pathological CLI
/// could exhaust:
/// - **wall clock**: the child is killed (and reaped) once `timeout` passes;
/// - **stdout**: a reader thread drains stdout into a buffer capped at
///   `max_output` bytes; exceeding it kills the child.
///
/// Returns `None` on an empty argv, an unresolvable program, a spawn failure,
/// a timeout, oversized output, or a failed read. A non-zero exit is *not*
/// `None`: the caller inspects [`BoundedOutput::status`].
///
/// On every failure path the reader thread is deliberately **detached, not
/// joined**: killing the child does not close the pipe when a grandchild
/// still holds its write end (on Windows, killing a `.cmd` shim's `cmd.exe`
/// leaves `node` running), so a join could outlive the deadline. The thread
/// ends on its own once the pipe finally closes; its send then fails
/// harmlessly against the dropped channel.
pub fn run_bounded(argv: &[String], timeout: Duration, max_output: usize) -> Option<BoundedOutput> {
    let (program, args) = argv.split_first()?;
    let program = resolve_program(program)?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buffer.len() + n > max_output {
                        let _ = tx.send(None);
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
        let _ = tx.send(Some(buffer));
    });
    let deadline = Instant::now() + timeout;
    let stdout = match rx.recv_timeout(timeout) {
        Ok(Some(buffer)) => buffer,
        // Timeout, oversized output, or read failure: kill the child and reap
        // it so it never lingers as a zombie; the reader is detached (see the
        // function docs) rather than joined.
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    // The reader saw EOF and already sent, so joining it cannot block.
    let _ = reader.join();
    // EOF does not mean the process has exited (it may have closed stdout
    // early). Reap it within the same deadline; a child that will not exit
    // after EOF is killed like a timeout.
    let status = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(remaining.min(POLL_INTERVAL)),
            Err(_) => break None,
        }
    };
    match status {
        Some(status) => Some(BoundedOutput { status, stdout }),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}
