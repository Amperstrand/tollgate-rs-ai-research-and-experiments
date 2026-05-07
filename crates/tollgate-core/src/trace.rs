//! Protocol trace types for structured event recording during test execution.
//!
//! This module provides data types for capturing protocol message exchanges
//! between actors (Client, Provider, Mint). The types are pure data — no
//! tracing dependency — so `tollgate-core` stays minimal.
//!
//! The [`spec_ref`] function maps each [`MessageType`] to the relevant
//! section of the TollGate design documents for spec cross-referencing.

use crate::protocol::MessageType;

/// Actor in a protocol trace (e.g., "Client", "Provider", "Mint").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceActor(pub String);

impl From<&str> for TraceActor {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Direction of a trace event, controlling the arrow style in diagrams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    /// Solid arrow `->>` — client-to-server request.
    Request,
    /// Dashed arrow `-->>` — server response or acknowledgment.
    Response,
    /// Standalone note over a single actor.
    Note,
}

impl TraceDirection {
    /// Returns the direction as a static string for serialization.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "Request",
            Self::Response => "Response",
            Self::Note => "Note",
        }
    }
}

/// A single step in the protocol trace.
///
/// Each event captures one message exchange (or note) between actors,
/// along with the spec reference and timing information.
#[derive(Debug, Clone)]
pub struct ProtocolTraceEvent {
    /// Source actor.
    pub actor: TraceActor,
    /// Target actor (`None` for notes).
    pub target: Option<TraceActor>,
    /// Direction (request, response, or note).
    pub direction: TraceDirection,
    /// Message type name (e.g., `"Announce"`).
    pub msg_type: String,
    /// Spec cross-reference (e.g., `"tollgate-protocol.md \u{00a7}3.1"`).
    pub spec_ref: String,
    /// Human-readable payload summary.
    pub payload: String,
    /// Optional additional context.
    pub note: Option<String>,
    /// Timestamp in milliseconds since Unix epoch.
    pub timestamp_ms: u64,
}

/// Maps a [`MessageType`] to its specification cross-reference.
///
/// Returns a string like `"tollgate-protocol.md \u{00a7}3.1"` pointing to the
/// relevant design document section.
#[must_use]
pub fn spec_ref(msg_type: MessageType) -> &'static str {
    match msg_type {
        MessageType::Announce => "tollgate-protocol.md \u{00a7}3.1",
        MessageType::PriceSheet => "tollgate-protocol.md \u{00a7}3.2",
        MessageType::Accept => "tollgate-protocol.md \u{00a7}3.3",
        MessageType::ChannelReady => "tollgate-protocol.md \u{00a7}3.4",
        MessageType::MeteringReport | MessageType::MeteringReportResponse => "tollgate-metering.md \u{00a7}2",
        MessageType::BalanceUpdate | MessageType::BalanceAck => {
            "tollgate-payment-channels.md \u{00a7}4"
        }
        MessageType::BootstrapToken | MessageType::BootstrapAck => {
            "tollgate-bootstrap.md \u{00a7}3"
        }
        MessageType::Disconnect => "tollgate-protocol.md \u{00a7}3.5",
        MessageType::RolloverInit
        | MessageType::RolloverReady
        | MessageType::ChannelClose
        | MessageType::CloseAck
        | MessageType::Reject => "tollgate-protocol.md",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_ref_announce() {
        assert_eq!(
            spec_ref(MessageType::Announce),
            "tollgate-protocol.md \u{00a7}3.1"
        );
    }

    #[test]
    fn spec_ref_metering() {
        assert_eq!(
            spec_ref(MessageType::MeteringReport),
            "tollgate-metering.md \u{00a7}2"
        );
    }

    #[test]
    fn spec_ref_bootstrap_token() {
        assert_eq!(
            spec_ref(MessageType::BootstrapToken),
            "tollgate-bootstrap.md \u{00a7}3"
        );
    }

    #[test]
    fn spec_ref_fallback() {
        assert_eq!(spec_ref(MessageType::Reject), "tollgate-protocol.md");
        assert_eq!(spec_ref(MessageType::RolloverInit), "tollgate-protocol.md");
        assert_eq!(spec_ref(MessageType::ChannelClose), "tollgate-protocol.md");
        assert_eq!(spec_ref(MessageType::CloseAck), "tollgate-protocol.md");
    }

    #[test]
    fn spec_ref_all_covered() {
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
            assert!(!spec_ref(*mt).is_empty(), "{mt:?} has empty spec_ref");
        }
    }

    #[test]
    fn trace_direction_as_str() {
        assert_eq!(TraceDirection::Request.as_str(), "Request");
        assert_eq!(TraceDirection::Response.as_str(), "Response");
        assert_eq!(TraceDirection::Note.as_str(), "Note");
    }

    #[test]
    fn trace_actor_from_str() {
        let actor: TraceActor = "Client".into();
        assert_eq!(actor, TraceActor("Client".to_owned()));
    }
}
