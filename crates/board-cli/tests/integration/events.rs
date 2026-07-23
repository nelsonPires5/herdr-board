use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use board_core::client::BoardClient;
use board_core::protocol::{Event, Response};

use super::{fake_card, todo_id, TestDaemon};

#[test]
fn subscribe_receives_board_changed_on_card_create() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let todo = todo_id(&mut c);

    let mut sub = c.subscribe().unwrap();
    // Give the daemon a moment to register the subscription's forwarder.
    std::thread::sleep(Duration::from_millis(300));

    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let handle = std::thread::spawn(move || {
        if let Some(ev) = sub.next() {
            let _ = tx.send(ev);
        }
    });

    // Trigger an event on a separate connection.
    let mut c2 = td.client();
    c2.card_create(&fake_card(todo)).unwrap();

    let ev = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive an event");
    assert!(matches!(ev, Event::BoardChanged { .. }));
    let _ = handle.join();
}

#[test]
fn delayed_event_reader_survives_board_change_flood() {
    let td = TestDaemon::start(&[]);
    let mut stream = UnixStream::connect(&td.socket).unwrap();
    stream
        .write_all(b"{\"id\":\"sub\",\"method\":\"events.subscribe\",\"params\":{}}\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));

    // Deliberately leave both the acknowledgement and events unread while a
    // burst is produced. Board-change notifications may coalesce, but the
    // response must remain first and the subscriber must stay connected.
    let mut client = td.client();
    let todo = todo_id(&mut client);
    for n in 0..200 {
        let mut card = fake_card(todo);
        card.title = format!("flood-{n}");
        client.card_create(&card).unwrap();
    }

    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let ack: Response = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(ack.id, "sub");

    line.clear();
    reader.read_line(&mut line).unwrap();
    let event: Event = serde_json::from_str(line.trim_end()).unwrap();
    assert!(matches!(event, Event::BoardChanged { .. }));
}
