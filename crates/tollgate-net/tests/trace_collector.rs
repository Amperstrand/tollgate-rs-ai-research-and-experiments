//! Tests for the shared TraceCollector infrastructure.
//!
//! Validates Mermaid rendering, JSON/Markdown output, and artifact writing.
//! The actual implementation lives in [`common::TraceCollector`].

mod common;

use std::fs;

use common::TraceCollector;
use tollgate_core::protocol::MessageType;
use tollgate_core::trace::{spec_ref, ProtocolTraceEvent, TraceActor, TraceDirection};

#[test]
fn collector_starts_empty() {
    let collector = TraceCollector::new();
    assert!(collector.collect().is_empty());
}

#[test]
fn mermaid_empty_produces_valid_header() {
    let collector = TraceCollector::new();
    let mmd = collector.to_mermaid();
    assert!(mmd.starts_with("sequenceDiagram"));
    assert!(mmd.contains("autonumber"));
    assert!(!mmd.contains("participant"));
}

#[test]
fn json_empty() {
    let collector = TraceCollector::new();
    assert!(collector.to_json().is_empty());
}

#[test]
fn markdown_empty() {
    let collector = TraceCollector::new();
    assert!(collector.to_markdown().is_empty());
}

#[test]
fn spec_ref_all_message_types_covered() {
    let all = [
        MessageType::Announce,
        MessageType::PriceSheet,
        MessageType::Accept,
        MessageType::ChannelReady,
        MessageType::MeteringReport,
        MessageType::BalanceUpdate,
        MessageType::BalanceAck,
        MessageType::BootstrapToken,
        MessageType::BootstrapAck,
        MessageType::RolloverInit,
        MessageType::RolloverReady,
        MessageType::ChannelClose,
        MessageType::CloseAck,
        MessageType::Reject,
        MessageType::Disconnect,
    ];
    for mt in &all {
        assert!(!spec_ref(*mt).is_empty());
    }
}

#[test]
fn write_artifacts_creates_files() {
    let dir = std::env::temp_dir().join("tollgate_trace_test_artifacts");
    let collector = TraceCollector::new();
    collector.write_artifacts(&dir, "empty_test").unwrap();

    assert!(dir.join("empty_test.mmd").exists());
    assert!(dir.join("empty_test.json").exists());
    assert!(dir.join("empty_test.md").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn mermaid_with_manual_events() {
    let collector = TraceCollector::new();
    let base_ts = 1_000_000;

    {
        let mut events = collector.events.lock().unwrap();
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Client".to_owned()),
            target: Some(TraceActor("Provider".to_owned())),
            direction: TraceDirection::Request,
            msg_type: "Announce".to_owned(),
            spec_ref: "tollgate-protocol.md \u{00a7}3.1".to_owned(),
            payload: "{version: 1}".to_owned(),
            note: None,
            timestamp_ms: base_ts,
        });
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Provider".to_owned()),
            target: Some(TraceActor("Client".to_owned())),
            direction: TraceDirection::Response,
            msg_type: "Announce".to_owned(),
            spec_ref: "tollgate-protocol.md \u{00a7}3.1".to_owned(),
            payload: "{version: 1}".to_owned(),
            note: None,
            timestamp_ms: base_ts + 2,
        });
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Provider".to_owned()),
            target: Some(TraceActor("Client".to_owned())),
            direction: TraceDirection::Request,
            msg_type: "PriceSheet".to_owned(),
            spec_ref: "tollgate-protocol.md \u{00a7}3.2".to_owned(),
            payload: "{products: 1}".to_owned(),
            note: None,
            timestamp_ms: base_ts + 5,
        });
    }

    let mmd = collector.to_mermaid();
    assert!(mmd.contains("participant Client"));
    assert!(mmd.contains("participant Provider"));
    assert!(mmd.contains("Client->>Provider: Announce"));
    assert!(mmd.contains("Provider-->>Client: Announce"));
    assert!(mmd.contains("Provider->>Client: PriceSheet"));
    assert!(mmd.contains("+0ms"));
    assert!(mmd.contains("+2ms"));
    assert!(mmd.contains("+5ms"));
}

#[test]
fn mermaid_with_metering_loop() {
    let collector = TraceCollector::new();
    let base_ts = 1_000_000;

    {
        let mut events = collector.events.lock().unwrap();
        for i in 0_u32..4 {
            events.push(ProtocolTraceEvent {
                actor: TraceActor("Client".to_owned()),
                target: Some(TraceActor("Provider".to_owned())),
                direction: TraceDirection::Request,
                msg_type: "MeteringReport".to_owned(),
                spec_ref: "tollgate-metering.md \u{00a7}2".to_owned(),
                payload: format!("{{elapsed: {}ms}}", 5000 * (i + 1)),
                note: None,
                timestamp_ms: base_ts + u64::from(i) * 5000,
            });
        }
    }

    let mmd = collector.to_mermaid();
    assert!(mmd.contains("loop Metering Intervals"));
    assert!(mmd.contains("end"));
    assert!(mmd.contains("MeteringReport"));
}

#[test]
fn json_output_format() {
    let collector = TraceCollector::new();
    {
        let mut events = collector.events.lock().unwrap();
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Client".to_owned()),
            target: Some(TraceActor("Provider".to_owned())),
            direction: TraceDirection::Request,
            msg_type: "Announce".to_owned(),
            spec_ref: "tollgate-protocol.md \u{00a7}3.1".to_owned(),
            payload: "{version: 1}".to_owned(),
            note: None,
            timestamp_ms: 1000,
        });
    }

    let json = collector.to_json();
    assert!(json.contains("\"actor\":\"Client\""));
    assert!(json.contains("\"target\":\"Provider\""));
    assert!(json.contains("\"direction\":\"Request\""));
    assert!(json.contains("\"msg_type\":\"Announce\""));
    assert!(json.contains("\"timestamp_ms\":1000"));
}

#[test]
fn markdown_output_format() {
    let collector = TraceCollector::new();
    {
        let mut events = collector.events.lock().unwrap();
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Client".to_owned()),
            target: Some(TraceActor("Provider".to_owned())),
            direction: TraceDirection::Request,
            msg_type: "Announce".to_owned(),
            spec_ref: "tollgate-protocol.md \u{00a7}3.1".to_owned(),
            payload: "{version: 1}".to_owned(),
            note: None,
            timestamp_ms: 1000,
        });
    }

    let md = collector.to_markdown();
    assert!(md.contains("# Protocol Trace"));
    assert!(md.contains("## Step 1: Announce"));
    assert!(md.contains("**Spec**: tollgate-protocol.md"));
    assert!(md.contains("**Actor**: Client"));
    assert!(md.contains("**Direction**: Request"));
    assert!(md.contains("**Timing**: +0ms"));
}

#[test]
fn json_escape_special_characters() {
    let escaped = common::json_escape("hello \"world\"\nline2\ttab");
    assert_eq!(escaped, "hello \\\"world\\\"\\nline2\\ttab");
}

#[test]
fn note_event_without_target() {
    let collector = TraceCollector::new();
    {
        let mut events = collector.events.lock().unwrap();
        events.push(ProtocolTraceEvent {
            actor: TraceActor("Client".to_owned()),
            target: None,
            direction: TraceDirection::Note,
            msg_type: "SystemNote".to_owned(),
            spec_ref: "tollgate-protocol.md".to_owned(),
            payload: "session started".to_owned(),
            note: None,
            timestamp_ms: 1000,
        });
    }

    let mmd = collector.to_mermaid();
    assert!(mmd.contains("Note over Client: SystemNote session started"));

    let md = collector.to_markdown();
    assert!(md.contains("**Actor**: Client\n"));
    assert!(!md.contains("\u{2192}"));
}
