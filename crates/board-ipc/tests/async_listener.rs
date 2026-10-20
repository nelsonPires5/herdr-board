//! The tokio server side: `AsyncLocalListener` + `AsyncLocalStream`.
#![cfg(feature = "tokio")]

mod support;

use std::io::{self, BufRead, BufReader, Write};
use std::time::Duration;

use board_ipc::{AsyncLocalListener, AsyncLocalStream, LocalStream};
use support::endpoint;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// Echo every line of every accepted connection, like boardd's split
/// reader/writer per connection.
fn spawn_echo(mut listener: AsyncLocalListener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read_half).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if write_half
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_clients_round_trip_through_the_async_listener() {
    let (_dir, path) = endpoint();
    let server = spawn_echo(AsyncLocalListener::bind(&path).unwrap());

    let replies = tokio::task::spawn_blocking(move || {
        (0..5)
            .map(|i| {
                let stream = LocalStream::connect(&path).unwrap();
                let mut writer = stream.try_clone().unwrap();
                writeln!(writer, "req-{i}").unwrap();
                let mut line = String::new();
                BufReader::new(stream).read_line(&mut line).unwrap();
                line
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    assert_eq!(
        replies,
        (0..5).map(|i| format!("req-{i}\n")).collect::<Vec<_>>()
    );
    server.abort();
}

#[tokio::test]
async fn async_clients_round_trip_through_the_async_listener() {
    let (_dir, path) = endpoint();
    let server = spawn_echo(AsyncLocalListener::bind(&path).unwrap());

    let mut client = AsyncLocalStream::connect(&path).await.unwrap();
    client.write_all(b"hello\n").await.unwrap();
    let mut reply = [0; 6];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"hello\n");
    server.abort();
}

#[tokio::test]
async fn accept_is_cancel_safe() {
    let (_dir, path) = endpoint();
    let mut listener = AsyncLocalListener::bind(&path).unwrap();
    tokio::select! {
        _ = listener.accept() => panic!("nobody connected"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    let mut client = AsyncLocalStream::connect(&path).await.unwrap();
    let mut accepted = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accept after a cancelled accept")
        .unwrap();
    client.write_all(b"x").await.unwrap();
    let mut byte = [0; 1];
    accepted.read_exact(&mut byte).await.unwrap();
    assert_eq!(&byte, b"x");
}

#[tokio::test]
async fn the_server_reads_eof_when_the_client_leaves() {
    let (_dir, path) = endpoint();
    let mut listener = AsyncLocalListener::bind(&path).unwrap();
    let client = AsyncLocalStream::connect(&path).await.unwrap();
    let mut accepted = listener.accept().await.unwrap();
    drop(client);
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), accepted.read_to_end(&mut rest))
        .await
        .expect("EOF must be observed");
    assert_eq!(read.unwrap(), 0);
}

#[tokio::test]
async fn a_second_listener_on_a_live_endpoint_fails() {
    let (_dir, path) = endpoint();
    let _first = AsyncLocalListener::bind(&path).unwrap();
    let error = AsyncLocalListener::bind(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error:?}");
}

#[cfg(windows)]
#[tokio::test]
async fn blocking_clients_see_the_daemon_process_as_the_pipe_server() {
    let (_dir, path) = endpoint();
    let server = spawn_echo(AsyncLocalListener::bind(&path).unwrap());
    let pid = tokio::task::spawn_blocking(move || {
        LocalStream::connect(&path)
            .unwrap()
            .server_process_id()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(pid, std::process::id());
    server.abort();
}
