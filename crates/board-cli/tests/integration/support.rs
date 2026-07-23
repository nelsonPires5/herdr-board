use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{
    CardCreateParams, CardStatus, ColumnCreateParams, DaemonStatus, Request, Response, Trigger,
};

pub(crate) const BOARD_BIN: &str = env!("CARGO_BIN_EXE_board");

pub(crate) fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A daemon under test, torn down on drop.
pub(crate) struct TestDaemon {
    child: Child,
    pub(crate) socket: PathBuf,
    pub(crate) _dir: tempfile::TempDir,
}

impl TestDaemon {
    pub(crate) fn start(extra: &[(&str, &str)]) -> TestDaemon {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("board.db");
        let socket = dir.path().join("boardd.sock");
        let cfg = dir.path().join("config.toml");
        let fake = fixtures_dir().join("fake-agent.sh");
        std::fs::write(
            &cfg,
            format!(
                "[harness.fake]\nargv = [\"bash\", \"{}\"]\n\n[daemon]\nspawner = \"local\"\n",
                fake.display()
            ),
        )
        .unwrap();

        let mut cmd = Command::new(BOARD_BIN);
        cmd.arg("daemon").arg("--foreground");
        cmd.env("BOARD_DB", &db)
            .env("BOARD_SOCKET", &socket)
            .env("HERDR_BOARD_CONFIG", &cfg)
            .env("BOARD_SPAWNER", "local")
            .env("BOARD_BIN", BOARD_BIN)
            .env("HOME", dir.path())
            .env("BOARD_TICK_MS", "150")
            .env("BOARD_LOCAL_POLL_MS", "150")
            .env("FAKE_AGENT_SLEEP", "0.3")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn daemon");

        let td = TestDaemon {
            child,
            socket,
            _dir: dir,
        };
        td.wait_ready();
        td
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(mut c) = UnixClient::connect(&self.socket) {
                if c.daemon_status().is_ok() {
                    return;
                }
            }
            if Instant::now() >= deadline {
                panic!("daemon did not become ready");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn client(&self) -> UnixClient {
        UnixClient::connect(&self.socket).expect("connect")
    }

    /// Run the `board` binary against this daemon's socket and capture its output.
    pub(crate) fn board(&self, args: &[&str]) -> std::process::Output {
        self.board_in(self._dir.path(), args)
    }

    pub(crate) fn board_in(&self, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new(BOARD_BIN)
            .args(args)
            .current_dir(cwd)
            .env("BOARD_SOCKET", &self.socket)
            .env("BOARD_DB", self._dir.path().join("board.db"))
            .env("HERDR_BOARD_CONFIG", self._dir.path().join("config.toml"))
            .env("HOME", self._dir.path())
            .env_remove("BOARD_SCOPE_PATH")
            .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
            .stdin(Stdio::null())
            .output()
            .expect("run board binary")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let mut cancelled_open_work = false;

        if let Ok(mut client) = UnixClient::connect(&self.socket) {
            // Cancel every kind of open card. A cancellation wakes dispatch, so
            // rescan briefly to catch work that was queued at the same time.
            let cancel_deadline = Instant::now() + Duration::from_millis(350);
            loop {
                let mut active_cards = Vec::new();
                if let Ok(boards) = client.board_list() {
                    for board in boards.boards {
                        if let Ok(snapshot) = client.board_get_by_id(board.id) {
                            active_cards.extend(
                                snapshot
                                    .cards
                                    .into_iter()
                                    .filter(|card| {
                                        matches!(
                                            card.status,
                                            CardStatus::Queued
                                                | CardStatus::Running
                                                | CardStatus::Blocked
                                                | CardStatus::Awaiting
                                        )
                                    })
                                    .map(|card| card.id),
                            );
                        }
                    }
                }
                active_cards.sort_unstable();
                active_cards.dedup();

                if active_cards.is_empty() {
                    break;
                }
                for card_id in active_cards {
                    if client.run_cancel(card_id).is_ok() {
                        cancelled_open_work = true;
                    }
                }
                if Instant::now() >= cancel_deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }

        if cancelled_open_work {
            // Keep the original listener alive while already-forked `board
            // comment`/`done` CLIs finish. Otherwise they auto-start a
            // replacement daemon when the socket disappears.
            std::thread::sleep(Duration::from_millis(750));
        }

        if let Ok(mut client) = UnixClient::connect(&self.socket) {
            let _ = client.daemon_stop();
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Ok(None) | Err(_) => break,
            }
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// -- helpers -----------------------------------------------------------------

pub(crate) fn col(name: &str, trigger: Trigger) -> ColumnCreateParams {
    ColumnCreateParams {
        name: name.to_string(),
        trigger: Some(trigger),
        ..Default::default()
    }
}

pub(crate) fn fake_card(column_id: i64) -> CardCreateParams {
    CardCreateParams {
        title: "task".to_string(),
        description: Some("do the thing".to_string()),
        harness: Some("fake".to_string()),
        column_id: Some(column_id),
        ..Default::default()
    }
}

pub(crate) fn todo_id(c: &mut UnixClient) -> i64 {
    c.board_get().unwrap().columns[0].id
}

/// Poll `pred` until it returns true or the timeout elapses.
pub(crate) fn poll(
    c: &mut UnixClient,
    secs: u64,
    mut pred: impl FnMut(&mut UnixClient) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred(c) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[derive(Clone, Copy)]
pub(crate) enum FakeStop {
    Error,
    StayLive,
    Disappear,
    Replace,
}

/// Minimal boardd-shaped listener for daemon-stop state-machine tests.
pub(crate) struct FakeListener {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeListener {
    pub(crate) fn bind(dir: &Path, mode: FakeStop) -> Self {
        let path = dir.join("fake-boardd.sock");
        let listener = UnixListener::bind(&path).expect("bind fake boardd listener");
        listener
            .set_nonblocking(true)
            .expect("set fake listener nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_path = path.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if handle_fake_connection(stream, &thread_path, mode) {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            path,
            stop,
            thread: Some(thread),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FakeListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake accept so the test thread can be joined without removing the
        // socket path (stale-socket tests intentionally inspect that path).
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_fake_connection(mut stream: UnixStream, path: &Path, mode: FakeStop) -> bool {
    // Test listeners must also work on macOS, where SO_RCVTIMEO can return
    // EINVAL for AF_UNIX streams. A short nonblocking loop still lets Drop's
    // request-less wake connection terminate promptly.
    stream
        .set_nonblocking(true)
        .expect("set fake stream nonblocking");
    let mut line = String::new();
    let request = {
        let mut reader = BufReader::new(stream.try_clone().expect("clone fake stream"));
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            match reader.read_line(&mut line) {
                Ok(0) => return false,
                Ok(_) if line.ends_with('\n') => {
                    break match serde_json::from_str::<Request>(line.trim_end()) {
                        Ok(request) => request,
                        Err(_) => return false,
                    };
                }
                Ok(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(_) | Err(_) => return false,
            }
        }
    };
    stream
        .set_nonblocking(false)
        .expect("restore fake stream blocking mode");
    let response = match request.method.as_str() {
        "daemon.stop" => match mode {
            FakeStop::Error => Response::err(request.id, 9, "stop rejected"),
            FakeStop::StayLive | FakeStop::Disappear | FakeStop::Replace => {
                Response::ok(request.id, serde_json::json!({"stopping": true}))
            }
        },
        "daemon.status" => Response::ok(
            request.id,
            serde_json::to_value(DaemonStatus {
                version: "fake".to_string(),
                db_path: "fake".to_string(),
                herdr_connected: false,
                active_runs: 0,
                queued_runs: 0,
            })
            .expect("serialize fake status"),
        ),
        _ => Response::err(request.id, 1, "unknown fake method"),
    };
    let mut wire = serde_json::to_string(&response).expect("serialize fake response");
    wire.push('\n');
    stream
        .write_all(wire.as_bytes())
        .expect("write fake response");
    stream.flush().expect("flush fake response");

    match mode {
        FakeStop::Disappear if response.error.is_none() && response.result.is_some() => {
            let _ = std::fs::remove_file(path);
            true
        }
        FakeStop::Replace if response.error.is_none() && response.result.is_some() => {
            let replacement = path.with_extension("replacement");
            std::fs::write(&replacement, b"replacement daemon socket").expect("write replacement");
            // Rename over the live socket atomically: an unlink/rename gap can
            // let the stop client observe a genuine disappearance and succeed.
            std::fs::rename(replacement, path).expect("install replacement");
            true
        }
        _ => false,
    }
}

pub(crate) fn run_board_stop(socket: &Path) -> std::process::Output {
    Command::new(BOARD_BIN)
        .args(["daemon", "--stop"])
        .env("BOARD_SOCKET", socket)
        .output()
        .expect("run board daemon --stop")
}
