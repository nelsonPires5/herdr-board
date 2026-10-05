//! Transport contract shared by the Unix socket and the Windows named pipe.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use board_ipc::{Listener, Stream};

/// A socket path under a directory whose name contains a space and a non-ASCII
/// letter, like `C:\Users\Jan Łukasz\…`.
fn socket_path(dir: &tempfile::TempDir) -> PathBuf {
    let nested = dir.path().join("Jan Ł");
    std::fs::create_dir_all(&nested).unwrap();
    nested.join("t.sock")
}

fn echo_server(listener: Listener) {
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                writer.write_all(line.as_bytes()).unwrap();
                line.clear();
            }
        }
    });
}

#[test]
fn round_trips_a_line_over_a_path_with_space_and_non_ascii() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    echo_server(Listener::bind(&path).unwrap());

    let stream = Stream::connect(&path).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    writer.write_all(b"hello\n").unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "hello\n");
}

#[test]
fn connect_to_a_missing_endpoint_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    assert!(Stream::connect(dir.path().join("absent.sock")).is_err());
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn read_timeout_surfaces_as_timed_out_or_would_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    let listener = Listener::bind(&path).unwrap();
    let _held = thread::spawn(move || {
        let conn = listener.incoming().next().unwrap().unwrap();
        thread::sleep(Duration::from_secs(2));
        drop(conn);
    });
    let mut stream = Stream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let err = stream.read(&mut [0u8; 8]).unwrap_err();
    assert!(matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn eof_after_server_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    let listener = Listener::bind(&path).unwrap();
    thread::spawn(move || {
        let mut conn = listener.incoming().next().unwrap().unwrap();
        conn.write_all(b"bye\n").unwrap();
        // conn dropped here: peer closes.
    });
    let stream = Stream::connect(&path).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    assert_eq!(reader.read_line(&mut line).unwrap(), 4);
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).unwrap(),
        0,
        "closed peer reads as EOF"
    );
}

/// A reader parked waiting for data must not block a write on its clone —
/// synchronous Windows pipe handles serialize I/O per file object.
#[test]
fn pending_read_does_not_block_a_write_on_the_clone() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    echo_server(Listener::bind(&path).unwrap());

    let stream = Stream::connect(&path).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let reader_thread = thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    });
    thread::sleep(Duration::from_millis(200)); // reader is now waiting
    let started = Instant::now();
    writer.write_all(b"ping\n").unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "write stalled"
    );
    assert_eq!(reader_thread.join().unwrap(), "ping\n");
}

#[cfg(windows)]
#[test]
fn second_bind_of_the_same_name_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    let _first = Listener::bind(&path).unwrap();
    assert!(Listener::bind(&path).is_err());
}

#[cfg(windows)]
#[test]
fn pipe_name_prefixes_the_verbatim_path() {
    let name = board_ipc::pipe_name(std::path::Path::new(r"C:\Users\a b\herdr.sock"));
    assert_eq!(
        name,
        std::ffi::OsString::from(r"\\.\pipe\C:\Users\a b\herdr.sock")
    );
}

#[cfg(windows)]
#[test]
fn nonblocking_read_without_data_would_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    let listener = Listener::bind(&path).unwrap();
    let _held = thread::spawn(move || {
        let conn = listener.incoming().next().unwrap().unwrap();
        thread::sleep(Duration::from_secs(2));
        drop(conn);
    });
    let mut stream = Stream::connect(&path).unwrap();
    stream.set_nonblocking(true).unwrap();
    let err = stream.read(&mut [0u8; 8]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
    assert!(!stream
        .wait_readable(Some(Duration::from_millis(50)))
        .unwrap());
}

/// A client that connects and leaves before the server accepts is still an
/// accepted connection that reads EOF (Unix `accept` semantics); the listener
/// keeps serving afterwards.
#[test]
fn accept_after_the_client_already_left_yields_eof_and_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    let listener = Listener::bind(&path).unwrap();
    drop(Stream::connect(&path).unwrap());

    let mut gone = listener.accept().unwrap().0;
    assert_eq!(gone.read(&mut [0u8; 8]).unwrap(), 0);

    let client = thread::spawn(move || {
        let mut stream = Stream::connect(&path).unwrap();
        stream.write_all(b"x").unwrap();
    });
    let mut next = listener.accept().unwrap().0;
    let mut byte = [0u8; 1];
    next.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
    client.join().unwrap();
}

/// Existence probe that does not consume a pipe instance (no connection).
#[cfg(windows)]
#[test]
fn endpoint_exists_reports_a_live_pipe_without_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir);
    assert!(!board_ipc::endpoint_exists(&path));
    let listener = Listener::bind(&path).unwrap();
    assert!(board_ipc::endpoint_exists(&path));
    // The single instance is still free: a real client connects and is accepted.
    let client = thread::spawn(move || Stream::connect(&path).unwrap());
    listener.accept().unwrap();
    client.join().unwrap();
}
