//! Event deserialization from synthetic / captured JSON.

use board_herdr::{parse_event_line, watch_subscriptions, AgentStatus, HerdrEvent, Subscription};

#[test]
fn parses_agent_status_changed_envelope() {
    // The wire shape herdr emits: EventEnvelope with underscore kind names.
    let line = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"working","agent":"claude","custom_status":null,"state_labels":{}}}"#;
    match parse_event_line(line).expect("event") {
        HerdrEvent::AgentStatusChanged {
            pane_id,
            workspace_id,
            status,
            agent,
        } => {
            assert_eq!(pane_id, "w1:p2");
            assert_eq!(workspace_id.as_deref(), Some("w1"));
            assert_eq!(status, AgentStatus::Working);
            assert_eq!(agent.as_deref(), Some("claude"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn parses_all_agent_status_values() {
    for (s, want) in [
        ("idle", AgentStatus::Idle),
        ("working", AgentStatus::Working),
        ("blocked", AgentStatus::Blocked),
        ("done", AgentStatus::Done),
        ("unknown", AgentStatus::Unknown),
    ] {
        let line = format!(
            r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"p","workspace_id":"w","agent_status":"{s}"}}}}"#
        );
        match parse_event_line(&line).unwrap() {
            HerdrEvent::AgentStatusChanged { status, .. } => assert_eq!(status, want),
            _ => panic!("wrong variant"),
        }
    }
}

#[test]
fn parses_pane_exited_and_closed() {
    for kind in ["pane_exited", "pane_closed"] {
        let line = format!(
            r#"{{"event":"{kind}","data":{{"type":"{kind}","pane_id":"w3:p9","workspace_id":"w3"}}}}"#
        );
        match parse_event_line(&line).unwrap() {
            HerdrEvent::PaneExited {
                pane_id,
                workspace_id,
            } => {
                assert_eq!(pane_id, "w3:p9");
                assert_eq!(workspace_id.as_deref(), Some("w3"));
            }
            other => panic!("wrong variant for {kind}: {other:?}"),
        }
    }
}

#[test]
fn parses_data_object_without_envelope_wrapper() {
    // Tolerant to a bare data object (no outer "event" key).
    let line = r#"{"type":"pane_exited","pane_id":"p1","workspace_id":"w1"}"#;
    assert!(matches!(
        parse_event_line(line),
        Some(HerdrEvent::PaneExited { .. })
    ));
}

#[test]
fn unknown_event_kind_becomes_other() {
    let line =
        r#"{"event":"workspace_focused","data":{"type":"workspace_focused","workspace_id":"w1"}}"#;
    match parse_event_line(line).unwrap() {
        HerdrEvent::Other(v) => assert_eq!(v["event"], "workspace_focused"),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn ignores_unknown_fields_in_status_event() {
    let line = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"p","workspace_id":"w","agent_status":"idle","future_field":123,"display_agent":"Claude"}}"#;
    assert!(matches!(
        parse_event_line(line),
        Some(HerdrEvent::AgentStatusChanged { .. })
    ));
}

#[test]
fn acks_and_blank_lines_are_skipped() {
    assert!(
        parse_event_line(r#"{"id":"subscribe","result":{"type":"subscription_started"}}"#)
            .is_none()
    );
    assert!(parse_event_line(r#"{"id":"x","error":{"code":"boom","message":"nope"}}"#).is_none());
    assert!(parse_event_line("   ").is_none());
    assert!(parse_event_line("not json").is_none());
}

#[test]
fn missing_agent_status_defaults_to_unknown() {
    let line = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"p","workspace_id":"w"}}"#;
    match parse_event_line(line).unwrap() {
        HerdrEvent::AgentStatusChanged { status, .. } => assert_eq!(status, AgentStatus::Unknown),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn watch_subscriptions_builds_expected_set() {
    let subs = watch_subscriptions(&["w1:p1".to_string(), "w1:p2".to_string()]);
    // 2 globals + 2 per-pane.
    assert_eq!(subs.len(), 4);
    assert_eq!(subs[0], Subscription::pane_exited());
    assert_eq!(subs[1], Subscription::pane_closed());
    assert_eq!(subs[2], Subscription::agent_status("w1:p1"));
    assert_eq!(subs[3], Subscription::agent_status("w1:p2"));
}

#[test]
fn parses_dotted_event_key_without_data_type() {
    // Exact shape captured live from herdr 0.7.3: kind only in the top-level
    // `event` key, dotted, with no `type` inside `data`.
    let line = r#"{"data":{"agent":"claude","agent_status":"done","pane_id":"w1:pB","workspace_id":"w1"},"event":"pane.agent_status_changed"}"#;
    match parse_event_line(line).unwrap() {
        HerdrEvent::AgentStatusChanged {
            pane_id, status, ..
        } => {
            assert_eq!(pane_id, "w1:pB");
            assert_eq!(status, AgentStatus::Done);
        }
        _ => panic!("wrong variant"),
    }
    let exited = r#"{"data":{"pane_id":"w1:p9","workspace_id":"w1"},"event":"pane.exited"}"#;
    assert!(matches!(
        parse_event_line(exited).unwrap(),
        HerdrEvent::PaneExited { .. }
    ));
}
