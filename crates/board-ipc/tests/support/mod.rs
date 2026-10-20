#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::thread::JoinHandle;

use board_ipc::{LocalListener, LocalStream};

/// A fresh endpoint path. tempfile's default dir is short enough for
/// AF_UNIX's 108-byte limit (`/tmp/.tmpXXXXXX/ep.sock`).
pub fn endpoint() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ep.sock");
    (dir, path)
}

/// A listener on a fresh endpoint.
pub fn listener() -> (tempfile::TempDir, PathBuf, LocalListener) {
    let (dir, path) = endpoint();
    let listener = LocalListener::bind(&path).expect("bind");
    (dir, path, listener)
}

/// Accept `connections` clients, echoing every line back.
pub fn echo_server(listener: LocalListener, connections: usize) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for stream in listener.incoming().take(connections) {
            let stream = stream.expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if writeln!(writer, "{line}").is_err() {
                    break;
                }
            }
        }
    })
}

/// Accept one client and hand it to `serve`.
pub fn serve_one<F>(listener: LocalListener, serve: F) -> JoinHandle<()>
where
    F: FnOnce(LocalStream) + Send + 'static,
{
    std::thread::spawn(move || serve(listener.accept().expect("accept")))
}
