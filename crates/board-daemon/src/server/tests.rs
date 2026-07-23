use super::*;
use board_core::protocol::RunOutcome;

fn changed(card_id: i64) -> Event {
    Event::BoardChanged {
        reason: BoardChangedReason::CardUpdated,
        card_id: Some(card_id),
        column_id: None,
    }
}
fn ended(run_id: i64) -> Event {
    Event::RunEnded {
        card_id: 1,
        run_id,
        outcome: RunOutcome::Ok,
    }
}

#[test]
fn consecutive_board_changes_coalesce_to_latest() {
    let mut b = Buffer::new(2);
    assert!(b.push_event(changed(1)));
    assert!(b.push_event(changed(2)));
    assert_eq!(b.entries.len(), 1);
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(2),
            ..
        }))
    ));
}

#[test]
fn terminal_event_is_not_overwritten_and_order_is_preserved() {
    let mut b = Buffer::new(3);
    assert!(b.push_event(changed(1)));
    assert!(b.push_event(ended(7)));
    assert!(b.push_event(changed(2)));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(1),
            ..
        }))
    ));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::RunEnded { run_id: 7, .. }))
    ));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(2),
            ..
        }))
    ));
}

#[test]
fn terminal_event_on_full_buffer_disconnects() {
    let mut b = Buffer::new(1);
    assert!(b.push_event(ended(1)));
    assert!(!b.push_event(ended(2)));
    assert!(b.closed);
}

#[tokio::test]
async fn capacity_one_flood_stays_bounded() {
    let out = Outbox::new(1);
    for id in 0..100 {
        assert!(out.event(changed(id)).await);
    }
    assert_eq!(out.buffer.lock().await.entries.len(), 1);
}
