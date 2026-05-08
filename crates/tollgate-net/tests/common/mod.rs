//! Shared test infrastructure for collecting and rendering protocol traces.
//!
//! [`TraceCollector`] implements `tracing_subscriber::Layer` to intercept
//! structured trace events emitted by the [`trace_event!`] macro. Collected
//! events can be rendered as Mermaid sequence diagrams, NDJSON traces, or

#![allow(dead_code)]
//! Markdown reports.
//!
//! This module lives under `tests/common/` so it is shared across integration
//! test files but NOT auto-discovered as a test itself.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tollgate_core::trace::{ProtocolTraceEvent, TraceActor, TraceDirection};
use tracing::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

// ---------------------------------------------------------------------------
// trace_event! macro
// ---------------------------------------------------------------------------

/// Emits a structured protocol trace event via `tracing::info!`.
///
/// Use this in integration tests to record protocol message exchanges.
/// The [`TraceCollector`] layer intercepts these events for rendering.
///
/// # Arguments
///
/// * `$actor` — Source actor name (e.g., `"Client"`)
/// * `$target` — Target actor name (e.g., `"Provider"`), empty string for notes
/// * `$direction` — `"Request"`, `"Response"`, or `"Note"`
/// * `$msg_type` — Message type name (e.g., `"Announce"`)
/// * `$spec_ref` — Spec cross-reference from `tollgate_core::trace::spec_ref`
/// * `$payload` — Human-readable payload summary
#[macro_export]
macro_rules! trace_event {
    ($actor:expr, $target:expr, $direction:expr, $msg_type:expr, $spec_ref:expr, $payload:expr) => {
        tracing::info!(
            trace_event = true,
            actor = $actor,
            target = $target,
            direction = $direction,
            msg_type = $msg_type,
            spec_ref = $spec_ref,
            payload = $payload,
        )
    };
}

// ---------------------------------------------------------------------------
// TraceEventVisitor — extracts fields from tracing events
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TraceEventVisitor {
    is_trace: bool,
    actor: Option<String>,
    target: Option<String>,
    direction: Option<String>,
    msg_type: Option<String>,
    spec_ref: Option<String>,
    payload: Option<String>,
}

impl Visit for TraceEventVisitor {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "trace_event" && value {
            self.is_trace = true;
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "actor" => self.actor = Some(value.to_owned()),
            "target" => self.target = Some(value.to_owned()),
            "direction" => self.direction = Some(value.to_owned()),
            "msg_type" => self.msg_type = Some(value.to_owned()),
            "spec_ref" => self.spec_ref = Some(value.to_owned()),
            "payload" => self.payload = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "actor" => self.actor = Some(format!("{value:?}")),
            "target" => self.target = Some(format!("{value:?}")),
            "direction" => self.direction = Some(format!("{value:?}")),
            "msg_type" => self.msg_type = Some(format!("{value:?}")),
            "spec_ref" => self.spec_ref = Some(format!("{value:?}")),
            "payload" => self.payload = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// TraceCollector
// ---------------------------------------------------------------------------

/// Collects protocol trace events from a tracing subscriber layer.
///
/// # Usage
///
/// ```ignore
/// let collector = TraceCollector::new();
/// let subscriber = tracing_subscriber::registry().with(collector.clone());
/// tracing::subscriber::set_default(subscriber);
///
/// trace_event!("Client", "Provider", "Request", "Announce",
///     "tollgate-protocol.md §3.1", "{version: 1, unit: bytes}");
///
/// let mermaid = collector.to_mermaid();
/// collector.write_artifacts(Path::new("target/traces"), "my_test").unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct TraceCollector {
    pub events: Arc<Mutex<Vec<ProtocolTraceEvent>>>,
}

impl TraceCollector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn collect(&self) -> Vec<ProtocolTraceEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn to_mermaid(&self) -> String {
        let events = self.collect();
        let mut out = String::from("sequenceDiagram\n    autonumber\n");

        if events.is_empty() {
            return out;
        }

        let actors: BTreeSet<&str> = events
            .iter()
            .flat_map(|e| {
                let mut a: Vec<&str> = vec![e.actor.0.as_str()];
                if let Some(ref t) = e.target {
                    a.push(t.0.as_str());
                }
                a
            })
            .collect();

        for actor in &actors {
            out.push_str("    participant ");
            out.push_str(actor);
            out.push('\n');
        }
        out.push('\n');

        let first_ts = events[0].timestamp_ms;
        let mut current_spec_ref = "";
        let mut i = 0;
        let mut open_phase: Option<Phase> = None;

        while i < events.len() {
            let evt = &events[i];
            let phase = resolve_phase(&events, i);

            if evt.spec_ref != current_spec_ref {
                current_spec_ref = &evt.spec_ref;
                append_spec_ref_note(&mut out, &events[i..], current_spec_ref);
            }

            if open_phase != Some(phase) {
                if let Some(p) = open_phase {
                    close_phase_block(&mut out, p);
                }
                open_phase = Some(phase);
                write!(
                    out,
                    "    rect rgb({})\n    Note right of {}: {} phase\n",
                    phase.color(),
                    events[i].actor.0,
                    phase.label(),
                )
                .unwrap();
            }

            if evt.msg_type == "MeteringReport" {
                let loop_start = i;
                while i < events.len()
                    && events[i].msg_type == "MeteringReport"
                    && resolve_phase(&events, i) == Phase::Metering
                {
                    i += 1;
                }
                if i - loop_start >= 2 {
                    out.push_str("    loop Metering Intervals\n");
                    for evt in events.iter().take(i).skip(loop_start) {
                        append_mermaid_event(&mut out, evt, first_ts, true);
                    }
                    out.push_str("    end\n");
                } else {
                    for evt in events.iter().take(i).skip(loop_start) {
                        append_mermaid_event(&mut out, evt, first_ts, false);
                    }
                }
            } else {
                append_mermaid_event(&mut out, evt, first_ts, false);
                i += 1;
            }
        }

        if let Some(p) = open_phase {
            close_phase_block(&mut out, p);
        }

        out
    }

    pub fn to_json(&self) -> String {
        self.collect()
            .iter()
            .map(|e| {
                let target = e.target.as_ref().map_or_else(
                    || "null".to_owned(),
                    |t| format!("\"{}\"", json_escape(&t.0)),
                );
                let note = e
                    .note
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), |n| format!("\"{}\"", json_escape(n)));
                format!(
                    "{{\"actor\":\"{}\",\"target\":{},\"direction\":\"{}\",\
                     \"msg_type\":\"{}\",\"spec_ref\":\"{}\",\"payload\":\"{}\",\
                     \"note\":{},\"timestamp_ms\":{}}}",
                    json_escape(&e.actor.0),
                    target,
                    e.direction.as_str(),
                    json_escape(&e.msg_type),
                    json_escape(&e.spec_ref),
                    json_escape(&e.payload),
                    note,
                    e.timestamp_ms,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn to_markdown(&self) -> String {
        let events = self.collect();
        if events.is_empty() {
            return String::new();
        }

        let first_ts = events[0].timestamp_ms;
        let mut out = String::from("# Protocol Trace\n\n");

        for (idx, e) in events.iter().enumerate() {
            let delta = e.timestamp_ms.saturating_sub(first_ts);
            let target_display = e
                .target
                .as_ref()
                .map(|t| format!(" \u{2192} {}", t.0))
                .unwrap_or_default();

            write!(
                out,
                "## Step {}: {}\n\n\
                 - **Spec**: {}\n\
                 - **Actor**: {}{}\n\
                 - **Direction**: {}\n\
                 - **Payload**: {}\n\
                 - **Timing**: +{}ms\n\n",
                idx + 1,
                e.msg_type,
                e.spec_ref,
                e.actor.0,
                target_display,
                e.direction.as_str(),
                e.payload,
                delta,
            )
            .unwrap();
        }

        out
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn write_artifacts(&self, dir: &Path, test_name: &str) -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{test_name}.mmd")), self.to_mermaid())?;
        fs::write(dir.join(format!("{test_name}.json")), self.to_json())?;
        fs::write(dir.join(format!("{test_name}.md")), self.to_markdown())?;
        Ok(())
    }
}

impl Default for TraceCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Layer implementation
// ---------------------------------------------------------------------------

impl<S> Layer<S> for TraceCollector
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = TraceEventVisitor::default();
        event.record(&mut visitor);

        if !visitor.is_trace {
            return;
        }

        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);

        let direction = match visitor.direction.as_deref() {
            Some("Request") => TraceDirection::Request,
            Some("Response") => TraceDirection::Response,
            _ => TraceDirection::Note,
        };

        let target = visitor.target.filter(|t| !t.is_empty()).map(TraceActor);

        let evt = ProtocolTraceEvent {
            actor: TraceActor(visitor.actor.unwrap_or_default()),
            target,
            direction,
            msg_type: visitor.msg_type.unwrap_or_default(),
            spec_ref: visitor.spec_ref.unwrap_or_default(),
            payload: visitor.payload.unwrap_or_default(),
            note: None,
            timestamp_ms: now_ms,
        };

        self.events.lock().unwrap().push(evt);
    }
}

// ---------------------------------------------------------------------------
// Mermaid rendering helpers
// ---------------------------------------------------------------------------

/// Phase classification for Mermaid `rect` background coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Setup,
    Payment,
    Metering,
    Teardown,
}

impl Phase {
    fn from_event(evt: &ProtocolTraceEvent) -> Self {
        match evt.msg_type.as_str() {
            "BootstrapToken" | "BootstrapAck" | "Balance" => Phase::Payment,
            "MeteringReport" => Phase::Metering,
            "Disconnect" => Phase::Teardown,
            _ => Phase::Setup,
        }
    }

    fn color(self) -> &'static str {
        match self {
            Phase::Setup => "230, 245, 255",    // light blue
            Phase::Payment => "230, 255, 237",  // light green
            Phase::Metering => "255, 248, 225", // light amber
            Phase::Teardown => "255, 235, 233", // light red
        }
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Setup => "Setup",
            Phase::Payment => "Payment",
            Phase::Metering => "Metering",
            Phase::Teardown => "Teardown",
        }
    }
}

fn append_spec_ref_note(out: &mut String, remaining: &[ProtocolTraceEvent], spec_ref: &str) {
    let mut first_actor: Option<&str> = None;
    let mut last_target: Option<&str> = None;

    for evt in remaining {
        if evt.spec_ref != spec_ref {
            break;
        }
        if first_actor.is_none() {
            first_actor = Some(&evt.actor.0);
        }
        if let Some(ref t) = evt.target {
            last_target = Some(&t.0);
        }
    }

    match (first_actor, last_target) {
        (Some(a), Some(t)) => {
            out.push_str("    Note over ");
            out.push_str(a);
            out.push(',');
            out.push_str(t);
            out.push_str(": ");
            out.push_str(&mermaid_escape(spec_ref));
            out.push('\n');
        }
        (Some(a), None) => {
            out.push_str("    Note over ");
            out.push_str(a);
            out.push_str(": ");
            out.push_str(&mermaid_escape(spec_ref));
            out.push('\n');
        }
        _ => {}
    }
}

fn resolve_phase(events: &[ProtocolTraceEvent], idx: usize) -> Phase {
    if events[idx].msg_type != "Balance" {
        return Phase::from_event(&events[idx]);
    }
    let mut lookback = idx;
    while lookback > 0 {
        lookback -= 1;
        if events[lookback].msg_type != "Balance" {
            return Phase::from_event(&events[lookback]);
        }
    }
    Phase::Setup
}

fn close_phase_block(out: &mut String, _phase: Phase) {
    out.push_str("    end\n");
}

fn append_mermaid_event(out: &mut String, evt: &ProtocolTraceEvent, first_ts: u64, indented: bool) {
    let indent = if indented { "        " } else { "    " };
    let delta = evt.timestamp_ms.saturating_sub(first_ts);
    let payload = mermaid_escape(&evt.payload);
    let msg_type = mermaid_escape(&evt.msg_type);

    match (evt.direction, &evt.target) {
        (TraceDirection::Request, Some(target)) => {
            out.push_str(indent);
            out.push_str(&evt.actor.0);
            out.push_str("->>");
            out.push_str(&target.0);
            out.push_str(": ");
            out.push_str(&msg_type);
            out.push(' ');
            out.push_str(&payload);
            out.push('\n');
        }
        (TraceDirection::Response, Some(target)) => {
            out.push_str(indent);
            out.push_str(&evt.actor.0);
            out.push_str("-->>");
            out.push_str(&target.0);
            out.push_str(": ");
            out.push_str(&msg_type);
            out.push(' ');
            out.push_str(&payload);
            out.push('\n');
        }
        (TraceDirection::Note, Some(target)) => {
            out.push_str(indent);
            out.push_str("Note over ");
            out.push_str(&evt.actor.0);
            out.push(',');
            out.push_str(&target.0);
            out.push_str(": ");
            out.push_str(&payload);
            out.push('\n');
        }
        _ => {
            out.push_str(indent);
            out.push_str("Note over ");
            out.push_str(&evt.actor.0);
            out.push_str(": ");
            out.push_str(&msg_type);
            out.push(' ');
            out.push_str(&payload);
            out.push('\n');
        }
    }

    out.push_str(indent);
    out.push_str("Note right of ");
    out.push_str(&evt.actor.0);
    out.push_str(": +");
    out.push_str(&delta.to_string());
    out.push_str("ms\n");
}

/// Escapes characters that are special in Mermaid diagram syntax.
///
/// Mermaid uses `{}` for block delimiters (alt, opt, loop, rect),
/// `<>` for some HTML-like constructs, and `#` for entity references.
/// Payload text inserted into Note/arrow labels must escape these.
pub fn mermaid_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '{' => out.push_str("#123;"),
            '}' => out.push_str("#125;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '#' => out.push_str("#35;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{:04x}", u32::from(c)).unwrap(),
            c => out.push(c),
        }
    }
    out
}
