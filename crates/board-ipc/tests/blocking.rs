//! Blocking `LocalStream`/`LocalListener` behaviour shared by both platforms.

mod support;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use board_ipc::{endpoint, LocalListener, LocalStream};
use support::{echo_server, endpoint, listener, serve_one};

const LONG: Duration = Duration::from_secs(5);

fn is_deadline(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[test]
fn round_trip_with_both_connect_flavours() {
    let (_dir, path, listener) = listener();
    let server = echo_server(listener, 2);

    for stream in [
        LocalStream::connect(&path).unwrap(),
        LocalStream::connect_with_deadline(&path, LONG).unwrap(),
    ] {
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writer.write_all(b"{\"id\":\"1\"}\n").unwrap();
        writer.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"id\":\"1\"}\n");
    }
    server.join().unwrap();
}

#[test]
fn connecting_to_a_missing_endpoint_is_absent() {
    let (_dir, path) = endpoint();
    let error = LocalStream::connect(&path).unwrap_err();
    assert!(endpoint::is_absent(&error), "{error:?}");
    let error = LocalStream::connect_with_deadline(&path, LONG).unwrap_err();
    assert!(endpoint::is_absent(&error), "{error:?}");
}

#[test]
fn a_dropped_listener_leaves_an_absent_endpoint() {
    // unix leaves a stale socket file (ConnectionRefused); Windows has no
    // pipe instance left (NotFound). Both read as "nothing is listening".
    let (_dir, path, listener) = listener();
    drop(listener);
    let error = LocalStream::connect_with_deadline(&path, LONG).unwrap_err();
    assert!(endpoint::is_absent(&error), "{error:?}");
}

#[test]
fn a_second_bind_on_a_live_endpoint_fails_with_addr_in_use() {
    let (_dir, path, _listener) = listener();
    let error = LocalListener::bind(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error:?}");
}

#[test]
fn read_timeout_fires_and_the_stream_stays_usable() {
    let (_dir, path, listener) = listener();
    let (release, released) = mpsc::channel::<()>();
    let server = serve_one(listener, move |mut stream| {
        released.recv().unwrap();
        stream.write_all(b"late\n").unwrap();
        let _ = released.recv();
    });

    let stream = LocalStream::connect_with_deadline(&path, LONG).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let started = Instant::now();
    let error = (&stream).read(&mut [0; 16]).unwrap_err();
    assert!(is_deadline(&error), "{error:?}");
    assert!(started.elapsed() >= Duration::from_millis(80));

    stream.set_read_timeout(None).unwrap();
    release.send(()).unwrap();
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).unwrap();
    assert_eq!(line, "late\n");
    drop(release);
    server.join().unwrap();
}

#[test]
fn a_zero_read_timeout_is_rejected_like_std() {
    let (_dir, path, listener) = listener();
    let server = echo_server(listener, 1);
    let stream = LocalStream::connect(&path).unwrap();
    let error = stream.set_read_timeout(Some(Duration::ZERO)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    drop(stream);
    server.join().unwrap();
}

#[test]
fn clones_share_the_read_timeout() {
    let (_dir, path, listener) = listener();
    let (done, finished) = mpsc::channel::<()>();
    let server = serve_one(listener, move |_stream| {
        let _ = finished.recv();
    });
    let stream = LocalStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut clone = stream.try_clone().unwrap();
    let error = clone.read(&mut [0; 4]).unwrap_err();
    assert!(is_deadline(&error), "{error:?}");
    drop(done);
    server.join().unwrap();
}

#[test]
fn write_timeout_fires_when_the_peer_stops_reading() {
    let (_dir, path, listener) = listener();
    let (done, finished) = mpsc::channel::<()>();
    let server = serve_one(listener, move |_stream| {
        let _ = finished.recv();
    });
    let mut stream = LocalStream::connect(&path).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    let error = (0..1024)
        .find_map(|_| stream.write(&chunk).err())
        .expect("the peer's buffer must eventually fill");
    assert!(is_deadline(&error), "{error:?}");
    drop(done);
    server.join().unwrap();
}

#[test]
fn poll_read_ready_reports_data_without_consuming_it() {
    let (_dir, path, listener) = listener();
    let (send, sent) = mpsc::channel::<()>();
    let server = serve_one(listener, move |mut stream| {
        sent.recv().unwrap();
        stream.write_all(b"one\n").unwrap();
        sent.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(b"two\n").unwrap();
        let _ = sent.recv();
    });

    let stream = LocalStream::connect(&path).unwrap();
    assert!(!stream
        .poll_read_ready(Some(Duration::from_millis(50)))
        .unwrap());

    send.send(()).unwrap();
    assert!(stream.poll_read_ready(Some(LONG)).unwrap());
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "one\n");

    // `None` waits until the delayed write lands.
    send.send(()).unwrap();
    assert!(stream.poll_read_ready(None).unwrap());
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "two\n");
    drop(send);
    server.join().unwrap();
}

#[test]
fn eof_when_the_server_drops_the_connection() {
    let (_dir, path, listener) = listener();
    let server = serve_one(listener, drop);
    let mut stream = LocalStream::connect(&path).unwrap();
    server.join().unwrap();
    assert!(stream.poll_read_ready(Some(LONG)).unwrap());
    assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);
    // Nonblocking reads report the same EOF rather than WouldBlock.
    stream.set_nonblocking(true).unwrap();
    assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);
}

#[test]
fn eof_when_the_client_drops_the_connection() {
    let (_dir, path, listener) = listener();
    let (result, received) = mpsc::channel();
    let server = serve_one(listener, move |mut stream| {
        let mut all = Vec::new();
        result
            .send(stream.read_to_end(&mut all).map(|_| all))
            .unwrap();
    });
    let mut stream = LocalStream::connect(&path).unwrap();
    stream.write_all(b"bye").unwrap();
    drop(stream);
    assert_eq!(received.recv_timeout(LONG).unwrap().unwrap(), b"bye");
    server.join().unwrap();
}

#[test]
fn a_blocked_reader_clone_does_not_stall_a_writer_clone() {
    let (_dir, path, listener) = listener();
    let server = echo_server(listener, 1);
    let stream = LocalStream::connect(&path).unwrap();
    let mut writer = stream.try_clone().unwrap();

    let (lines, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            lines.send(line).unwrap();
        }
    });
    // Let the reader block in its read before writing on the other clone.
    std::thread::sleep(Duration::from_millis(100));
    writer.write_all(b"ping\n").unwrap();
    assert_eq!(received.recv_timeout(LONG).unwrap(), "ping\n");
    writer.write_all(b"pong\n").unwrap();
    assert_eq!(received.recv_timeout(LONG).unwrap(), "pong\n");
    reader.join().unwrap();
    drop(writer);
    server.join().unwrap();
}

#[test]
fn nonblocking_read_would_block_until_data_arrives() {
    let (_dir, path, listener) = listener();
    let (send, sent) = mpsc::channel::<()>();
    let server = serve_one(listener, move |mut stream| {
        sent.recv().unwrap();
        stream.write_all(b"data").unwrap();
        let _ = sent.recv();
    });
    let mut stream = LocalStream::connect(&path).unwrap();
    stream.set_nonblocking(true).unwrap();
    let error = stream.read(&mut [0; 8]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);

    send.send(()).unwrap();
    assert!(stream.poll_read_ready(Some(LONG)).unwrap());
    let mut buf = [0; 8];
    let n = stream.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"data");
    drop(send);
    server.join().unwrap();
}

#[test]
fn nonblocking_accept_would_block_until_a_client_connects() {
    let (_dir, path, listener) = listener();
    listener.set_nonblocking(true).unwrap();
    let error = listener.accept().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);

    let mut client = LocalStream::connect(&path).unwrap();
    client.write_all(b"x").unwrap();
    let deadline = Instant::now() + LONG;
    let mut accepted = loop {
        match listener.accept() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "no connection accepted");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept failed: {error:?}"),
        }
    };
    let mut buf = [0; 1];
    accepted.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"x");

    // The listener keeps serving after a handoff.
    let _second = LocalStream::connect(&path).unwrap();
}

#[cfg(unix)]
#[test]
fn connect_with_deadline_rejects_an_overlong_socket_path() {
    let path = std::path::PathBuf::from(format!("/tmp/{}.sock", "x".repeat(200)));
    let error = LocalStream::connect_with_deadline(&path, LONG).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENAMETOOLONG));
}
