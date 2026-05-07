//! TollGate wire protocol message types.
//!
//! All 16 message types defined in the TollGate v2 protocol specification
//! (`docs/design/core/tollgate-protocol.md`).
//!
//! Messages are encoded as CBOR maps with integer keys per the canonical
//! CDDL schema (`protocol/tollgate.cddl`). Uses [`minicbor`] for zero-serde,
//! integer-key CBOR encoding.

use minicbor::decode::Error;
use minicbor::encode::Write;
use minicbor::{Decode, Decoder, Encode, Encoder};

// ---------------------------------------------------------------------------
// Fixed-size byte array wrappers
// ---------------------------------------------------------------------------

macro_rules! fixed_bytes {
    ($name:ident, $size:expr, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(pub [u8; $size]);

        impl<C> Encode<C> for $name {
            fn encode<W: Write>(
                &self,
                e: &mut Encoder<W>,
                _: &mut C,
            ) -> Result<(), minicbor::encode::Error<W::Error>> {
                e.bytes(&self.0)?;
                Ok(())
            }
        }

        impl<'b, C> Decode<'b, C> for $name {
            fn decode(d: &mut Decoder<'b>, _: &mut C) -> Result<Self, Error> {
                let bytes = d.bytes()?;
                bytes
                    .try_into()
                    .map($name)
                    .map_err(|_| Error::message(concat!("expected ", stringify!($size), " bytes")))
            }
        }

        impl From<[u8; $size]> for $name {
            fn from(v: [u8; $size]) -> Self {
                Self(v)
            }
        }
    };
}

fixed_bytes!(PubKey, 33, "Compressed secp256k1 public key (33 bytes).");
fixed_bytes!(Hash32, 32, "32-byte hash (channel IDs, product IDs, etc.).");
fixed_bytes!(Signature, 64, "Schnorr signature (64 bytes).");

// ---------------------------------------------------------------------------
// IntervalRange — CBOR array [u32, u32]
// ---------------------------------------------------------------------------

/// Metering interval range `[min_ms, max_ms]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalRange(pub [u32; 2]);

impl<C> Encode<C> for IntervalRange {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(2)?.u32(self.0[0])?.u32(self.0[1])?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for IntervalRange {
    fn decode(d: &mut Decoder<'b>, _: &mut C) -> Result<Self, Error> {
        d.array()?;
        let min = d.u32()?;
        let max = d.u32()?;
        Ok(IntervalRange([min, max]))
    }
}

// ---------------------------------------------------------------------------
// Simple enums (index_only)
// ---------------------------------------------------------------------------

/// Channel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Direction {
    #[n(0)]
    AB,
    #[n(1)]
    BA,
}

/// Bootstrap verification status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum BootstrapStatus {
    #[n(0)]
    Accepted,
    #[n(1)]
    Rejected,
}

/// Reason for channel close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum CloseReason {
    #[n(0)]
    Normal,
    #[n(1)]
    PriceRejected,
    #[n(2)]
    PeerLeaving,
}

/// Machine-readable rejection reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
#[repr(u8)]
pub enum ReasonCode {
    #[n(0x01)]
    PriceTooHigh = 0x01,
    #[n(0x02)]
    MintNotAccepted = 0x02,
    #[n(0x03)]
    UnitNotAccepted = 0x03,
    #[n(0x04)]
    IntervalOutOfRange = 0x04,
    #[n(0x05)]
    FundingInvalid = 0x05,
    #[n(0x06)]
    BalanceVerificationFailed = 0x06,
    #[n(0x07)]
    TransitLossExceeded = 0x07,
    #[n(0x08)]
    RenegotiationRequired = 0x08,
    #[n(0x09)]
    VersionUnsupported = 0x09,
    #[n(0xFF)]
    Other = 0xFF,
}

// ---------------------------------------------------------------------------
// MessageType — Rust-side discriminator (not encoded directly)
// ---------------------------------------------------------------------------

/// Discriminator for TollGate message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Announce = 0x00,
    PriceSheet = 0x01,
    Accept = 0x02,
    ChannelReady = 0x03,
    MeteringReport = 0x04,
    BalanceUpdate = 0x05,
    BalanceAck = 0x06,
    BootstrapToken = 0x07,
    BootstrapAck = 0x08,
    RolloverInit = 0x09,
    RolloverReady = 0x0A,
    ChannelClose = 0x0B,
    CloseAck = 0x0C,
    Reject = 0x0D,
    Disconnect = 0x0E,
    MeteringReportResponse = 0x0F,
}

// ---------------------------------------------------------------------------
// Nested types (no msg_type field)
// ---------------------------------------------------------------------------

/// A mint option with pricing.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct MintOption {
    #[n(1)]
    pub option_id: Hash32,
    #[n(2)]
    pub mint_url: String,
    #[n(3)]
    pub price_per_second: i64,
    #[n(4)]
    pub price_per_unit: i64,
    #[n(5)]
    pub mint_unit: String,
}

/// A product offering with mint options.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Product {
    #[n(1)]
    pub product_id: Hash32,
    #[n(2)]
    #[cbor(with = "minicbor::bytes")]
    pub extensions: Vec<u8>,
    #[n(3)]
    pub pricing_scale: u64,
    #[n(4)]
    pub mint_options: Vec<MintOption>,
}

// ---------------------------------------------------------------------------
// Message structs (all with msg_type at key 0)
// ---------------------------------------------------------------------------

/// 0x00 Announce — first message from each peer.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Announce {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub protocol_version: u8,
    #[n(2)]
    pub pubkey: PubKey,
    #[n(3)]
    pub unit: String,
    #[n(4)]
    pub capabilities: u32,
}

/// 0x01 PriceSheet — product offerings with pricing.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PriceSheet {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub products: Vec<Product>,
    #[n(2)]
    pub interval_range: IntervalRange,
}

/// 0x02 Accept — accept a price sheet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Accept {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub product_id: Hash32,
    #[n(2)]
    pub option_id: Hash32,
    #[n(3)]
    pub interval_range: IntervalRange,
    #[n(4)]
    #[cbor(with = "minicbor::bytes")]
    pub channel_funding: Vec<u8>,
}

/// 0x03 ChannelReady — confirm Spilman channel active.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ChannelReady {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub channel_id: Hash32,
    #[n(2)]
    pub direction: Direction,
}

/// 0x04 MeteringReport — unsigned cumulative resource stats.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct MeteringReport {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub elapsed_ms: u64,
    #[n(2)]
    pub delivered: u64,
    #[n(3)]
    pub received: u64,
    #[n(4)]
    pub new_product_id: Option<Hash32>,
    #[n(5)]
    pub new_pricing: Option<Vec<MintOption>>,
}

/// 0x05 BalanceUpdate — signed Spilman balance update.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BalanceUpdate {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub channel_id: Hash32,
    #[n(2)]
    pub cumulative_balance: u64,
    #[n(3)]
    pub balance_signature: Signature,
    #[n(4)]
    pub net_amount: u64,
}

/// 0x06 BalanceAck — creditor confirms balance update.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BalanceAck {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub channel_id: Hash32,
    #[n(2)]
    pub accepted_balance: u64,
}

/// 0x07 BootstrapToken — regular Cashu token for bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BootstrapToken {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    #[cbor(with = "minicbor::bytes")]
    pub token: Vec<u8>,
}

/// 0x08 BootstrapAck — acknowledge bootstrap token.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BootstrapAck {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub status: BootstrapStatus,
    #[n(2)]
    pub reason: Option<String>,
}

/// 0x09 RolloverInit — initiate channel rollover.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct RolloverInit {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub old_channel_id: Hash32,
    #[n(2)]
    #[cbor(with = "minicbor::bytes")]
    pub new_channel_funding: Vec<u8>,
}

/// 0x0A RolloverReady — new channel ready.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct RolloverReady {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub old_channel_id: Hash32,
    #[n(2)]
    pub new_channel_id: Hash32,
}

/// 0x0B ChannelClose — request cooperative close.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ChannelClose {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub channel_id: Hash32,
    #[n(2)]
    pub final_balance: u64,
    #[n(3)]
    pub final_signature: Signature,
    #[n(4)]
    pub reason: CloseReason,
}

/// 0x0C CloseAck — acknowledge cooperative close.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct CloseAck {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub channel_id: Hash32,
    #[n(2)]
    pub accepted_balance: u64,
}

/// 0x0D Reject — general-purpose rejection.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Reject {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub rejected_type: u8,
    #[n(2)]
    pub reason_code: ReasonCode,
    #[n(3)]
    pub reason_text: Option<String>,
}

/// 0x0E Disconnect — orderly teardown.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Disconnect {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub reason_code: ReasonCode,
}

/// 0x0F MeteringReportResponse — seller responds to MeteringReport with quota metadata.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct MeteringReportResponse {
    #[n(0)]
    pub msg_type: u8,
    #[n(1)]
    pub remaining_quota: i64,
    #[n(2)]
    pub next_checkin_ms: u64,
    #[n(3)]
    pub is_final: bool,
}

// ---------------------------------------------------------------------------
// Message enum — custom Encode/Decode for type dispatch
// ---------------------------------------------------------------------------

/// Top-level TollGate protocol message.
///
/// Encoded as a CBOR map with integer key 0 as the type discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Announce(Announce),
    PriceSheet(PriceSheet),
    Accept(Accept),
    ChannelReady(ChannelReady),
    MeteringReport(MeteringReport),
    BalanceUpdate(BalanceUpdate),
    BalanceAck(BalanceAck),
    BootstrapToken(BootstrapToken),
    BootstrapAck(BootstrapAck),
    RolloverInit(RolloverInit),
    RolloverReady(RolloverReady),
    ChannelClose(ChannelClose),
    CloseAck(CloseAck),
    Reject(Reject),
    Disconnect(Disconnect),
    MeteringReportResponse(MeteringReportResponse),
}

impl<C> Encode<C> for Message {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            Message::Announce(m) => m.encode(e, ctx),
            Message::PriceSheet(m) => m.encode(e, ctx),
            Message::Accept(m) => m.encode(e, ctx),
            Message::ChannelReady(m) => m.encode(e, ctx),
            Message::MeteringReport(m) => m.encode(e, ctx),
            Message::BalanceUpdate(m) => m.encode(e, ctx),
            Message::BalanceAck(m) => m.encode(e, ctx),
            Message::BootstrapToken(m) => m.encode(e, ctx),
            Message::BootstrapAck(m) => m.encode(e, ctx),
            Message::RolloverInit(m) => m.encode(e, ctx),
            Message::RolloverReady(m) => m.encode(e, ctx),
            Message::ChannelClose(m) => m.encode(e, ctx),
            Message::CloseAck(m) => m.encode(e, ctx),
            Message::Reject(m) => m.encode(e, ctx),
            Message::Disconnect(m) => m.encode(e, ctx),
            Message::MeteringReportResponse(m) => m.encode(e, ctx),
        }
    }
}

impl<'b, C> Decode<'b, C> for Message {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, Error> {
        let pos = d.position();
        // Probe: read map header, then key 0 + msg_type value.
        d.map()?;
        let key: u64 = d.decode()?;
        if key != 0 {
            return Err(Error::message("expected key 0 for type discriminator"));
        }
        let msg_type: u8 = d.decode()?;
        // Rewind to start of the CBOR item so the struct decoder
        // can read the whole map from the beginning.
        d.set_position(pos);

        match msg_type {
            0 => Ok(Message::Announce(d.decode_with(ctx)?)),
            1 => Ok(Message::PriceSheet(d.decode_with(ctx)?)),
            2 => Ok(Message::Accept(d.decode_with(ctx)?)),
            3 => Ok(Message::ChannelReady(d.decode_with(ctx)?)),
            4 => Ok(Message::MeteringReport(d.decode_with(ctx)?)),
            5 => Ok(Message::BalanceUpdate(d.decode_with(ctx)?)),
            6 => Ok(Message::BalanceAck(d.decode_with(ctx)?)),
            7 => Ok(Message::BootstrapToken(d.decode_with(ctx)?)),
            8 => Ok(Message::BootstrapAck(d.decode_with(ctx)?)),
            9 => Ok(Message::RolloverInit(d.decode_with(ctx)?)),
            10 => Ok(Message::RolloverReady(d.decode_with(ctx)?)),
            11 => Ok(Message::ChannelClose(d.decode_with(ctx)?)),
            12 => Ok(Message::CloseAck(d.decode_with(ctx)?)),
            13 => Ok(Message::Reject(d.decode_with(ctx)?)),
            14 => Ok(Message::Disconnect(d.decode_with(ctx)?)),
            15 => Ok(Message::MeteringReportResponse(d.decode_with(ctx)?)),
            _ => Err(Error::message("unknown message type")),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use minicbor::{decode, to_vec};

    fn roundtrip(msg: &Message) {
        let bytes = to_vec(msg).unwrap();
        let decoded: Message = decode(&bytes).unwrap();
        assert_eq!(*msg, decoded);
    }

    #[test]
    fn message_type_discriminators() {
        assert_eq!(MessageType::Announce as u8, 0x00);
        assert_eq!(MessageType::PriceSheet as u8, 0x01);
        assert_eq!(MessageType::Accept as u8, 0x02);
        assert_eq!(MessageType::ChannelReady as u8, 0x03);
        assert_eq!(MessageType::MeteringReport as u8, 0x04);
        assert_eq!(MessageType::BalanceUpdate as u8, 0x05);
        assert_eq!(MessageType::BalanceAck as u8, 0x06);
        assert_eq!(MessageType::BootstrapToken as u8, 0x07);
        assert_eq!(MessageType::BootstrapAck as u8, 0x08);
        assert_eq!(MessageType::RolloverInit as u8, 0x09);
        assert_eq!(MessageType::RolloverReady as u8, 0x0A);
        assert_eq!(MessageType::ChannelClose as u8, 0x0B);
        assert_eq!(MessageType::CloseAck as u8, 0x0C);
        assert_eq!(MessageType::Reject as u8, 0x0D);
        assert_eq!(MessageType::Disconnect as u8, 0x0E);
        assert_eq!(MessageType::MeteringReportResponse as u8, 0x0F);
    }

    #[test]
    fn reason_code_values() {
        assert_eq!(ReasonCode::PriceTooHigh as u8, 0x01);
        assert_eq!(ReasonCode::Other as u8, 0xFF);
    }

    #[test]
    fn announce_roundtrip() {
        roundtrip(&Message::Announce(Announce {
            msg_type: 0,
            protocol_version: 1,
            pubkey: PubKey([0x02; 33]),
            unit: "bytes".to_owned(),
            capabilities: 0x01,
        }));
    }

    #[test]
    fn disconnect_roundtrip() {
        roundtrip(&Message::Disconnect(Disconnect {
            msg_type: 14,
            reason_code: ReasonCode::VersionUnsupported,
        }));
    }

    #[test]
    fn disconnect_integer_keys() {
        let msg = Message::Disconnect(Disconnect {
            msg_type: 14,
            reason_code: ReasonCode::VersionUnsupported,
        });
        let bytes = to_vec(&msg).unwrap();
        // CBOR map(2) = 0xa2
        assert_eq!(bytes[0], 0xa2, "expected CBOR map(2)");
        // Key 0 as integer (not text "0")
        assert_eq!(bytes[1], 0x00, "expected integer key 0, not text key");
        // Value: integer 14
        assert_eq!(bytes[2], 0x0e, "expected integer value 14");
    }

    #[test]
    fn balance_update_roundtrip() {
        roundtrip(&Message::BalanceUpdate(BalanceUpdate {
            msg_type: 5,
            channel_id: Hash32([0xAA; 32]),
            cumulative_balance: 1000,
            balance_signature: Signature([0xBB; 64]),
            net_amount: 50,
        }));
    }

    #[test]
    fn price_sheet_roundtrip() {
        roundtrip(&Message::PriceSheet(PriceSheet {
            msg_type: 1,
            products: vec![Product {
                product_id: Hash32([0x11; 32]),
                extensions: vec![0xDE, 0xAD],
                pricing_scale: 100,
                mint_options: vec![MintOption {
                    option_id: Hash32([0x22; 32]),
                    mint_url: "https://mint.example.com".to_owned(),
                    price_per_second: 10,
                    price_per_unit: 1,
                    mint_unit: "sat".to_owned(),
                }],
            }],
            interval_range: IntervalRange([1000, 5000]),
        }));
    }

    #[test]
    fn accept_roundtrip() {
        roundtrip(&Message::Accept(Accept {
            msg_type: 2,
            product_id: Hash32([0x11; 32]),
            option_id: Hash32([0x22; 32]),
            interval_range: IntervalRange([1000, 5000]),
            channel_funding: vec![0xCA, 0xFE],
        }));
    }

    #[test]
    fn channel_ready_roundtrip() {
        roundtrip(&Message::ChannelReady(ChannelReady {
            msg_type: 3,
            channel_id: Hash32([0x33; 32]),
            direction: Direction::AB,
        }));
    }

    #[test]
    fn metering_report_roundtrip() {
        roundtrip(&Message::MeteringReport(MeteringReport {
            msg_type: 4,
            elapsed_ms: 5000,
            delivered: 1024,
            received: 900,
            new_product_id: None,
            new_pricing: None,
        }));
    }

    #[test]
    fn balance_ack_roundtrip() {
        roundtrip(&Message::BalanceAck(BalanceAck {
            msg_type: 6,
            channel_id: Hash32([0x44; 32]),
            accepted_balance: 999,
        }));
    }

    #[test]
    fn bootstrap_token_roundtrip() {
        roundtrip(&Message::BootstrapToken(BootstrapToken {
            msg_type: 7,
            token: vec![0xBE, 0xEF],
        }));
    }

    #[test]
    fn bootstrap_ack_roundtrip() {
        roundtrip(&Message::BootstrapAck(BootstrapAck {
            msg_type: 8,
            status: BootstrapStatus::Accepted,
            reason: None,
        }));
    }

    #[test]
    fn rollover_init_roundtrip() {
        roundtrip(&Message::RolloverInit(RolloverInit {
            msg_type: 9,
            old_channel_id: Hash32([0x55; 32]),
            new_channel_funding: vec![0xFA, 0xCE],
        }));
    }

    #[test]
    fn rollover_ready_roundtrip() {
        roundtrip(&Message::RolloverReady(RolloverReady {
            msg_type: 10,
            old_channel_id: Hash32([0x55; 32]),
            new_channel_id: Hash32([0x66; 32]),
        }));
    }

    #[test]
    fn channel_close_roundtrip() {
        roundtrip(&Message::ChannelClose(ChannelClose {
            msg_type: 11,
            channel_id: Hash32([0x77; 32]),
            final_balance: 500,
            final_signature: Signature([0xCC; 64]),
            reason: CloseReason::Normal,
        }));
    }

    #[test]
    fn close_ack_roundtrip() {
        roundtrip(&Message::CloseAck(CloseAck {
            msg_type: 12,
            channel_id: Hash32([0x77; 32]),
            accepted_balance: 500,
        }));
    }

    #[test]
    fn reject_roundtrip() {
        roundtrip(&Message::Reject(Reject {
            msg_type: 13,
            rejected_type: 1,
            reason_code: ReasonCode::PriceTooHigh,
            reason_text: Some("too expensive".to_owned()),
        }));
    }

    #[test]
    fn metering_report_with_optional_fields() {
        let msg = Message::MeteringReport(MeteringReport {
            msg_type: 4,
            elapsed_ms: 10000,
            delivered: 2048,
            received: 2000,
            new_product_id: Some(Hash32([0x99; 32])),
            new_pricing: Some(vec![MintOption {
                option_id: Hash32([0xAA; 32]),
                mint_url: "https://new-mint.example.com".to_owned(),
                price_per_second: 5,
                price_per_unit: 2,
                mint_unit: "msat".to_owned(),
            }]),
        });
        roundtrip(&msg);
    }

    #[test]
    fn bootstrap_ack_rejected_with_reason() {
        roundtrip(&Message::BootstrapAck(BootstrapAck {
            msg_type: 8,
            status: BootstrapStatus::Rejected,
            reason: Some("invalid signature".to_owned()),
        }));
    }

    #[test]
    fn metering_report_response_roundtrip() {
        roundtrip(&Message::MeteringReportResponse(MeteringReportResponse {
            msg_type: 15,
            remaining_quota: 98_950,
            next_checkin_ms: 5000,
            is_final: false,
        }));
    }

    #[test]
    fn metering_report_response_is_final() {
        roundtrip(&Message::MeteringReportResponse(MeteringReportResponse {
            msg_type: 15,
            remaining_quota: 5_000,
            next_checkin_ms: 1000,
            is_final: true,
        }));
    }
}
