//! TollGate wire protocol message types.
//!
//! All 15 message types defined in the TollGate v2 protocol specification
//! (`docs/design/core/tollgate-protocol.md`).
//!
//! Messages are encoded as CBOR maps with integer keys per the canonical
//! CDDL schema (`protocol/tollgate.cddl`).

use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! fixed_bytes {
    ($name:ident, $size:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(pub [u8; $size]);

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_bytes(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct BytesVisitor;

                impl<'de> Visitor<'de> for BytesVisitor {
                    type Value = [u8; $size];

                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "a byte string of exactly {} bytes", $size)
                    }

                    fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                        v.try_into().map_err(|_| {
                            E::custom(format!("expected {} bytes, got {}", $size, v.len()))
                        })
                    }

                    fn visit_byte_buf<E: serde::de::Error>(
                        self,
                        v: Vec<u8>,
                    ) -> Result<Self::Value, E> {
                        v.try_into().map_err(|bytes: Vec<u8>| {
                            E::custom(format!("expected {} bytes, got {}", $size, bytes.len()))
                        })
                    }
                }

                d.deserialize_byte_buf(BytesVisitor).map(Self)
            }
        }

        impl From<[u8; $size]> for $name {
            fn from(v: [u8; $size]) -> Self {
                Self(v)
            }
        }
    };
}

fixed_bytes!(PubKey, 33);
fixed_bytes!(Hash32, 32);
fixed_bytes!(Signature, 64);

/// Discriminator for TollGate message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
}

/// Top-level TollGate protocol message.
///
/// Encoded as a CBOR map with integer key 0 as the type discriminator.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "0")]
pub enum Message {
    #[serde(rename = "0")]
    Announce(Announce),
    #[serde(rename = "1")]
    PriceSheet(PriceSheet),
    #[serde(rename = "2")]
    Accept(Accept),
    #[serde(rename = "3")]
    ChannelReady(ChannelReady),
    #[serde(rename = "4")]
    MeteringReport(MeteringReport),
    #[serde(rename = "5")]
    BalanceUpdate(BalanceUpdate),
    #[serde(rename = "6")]
    BalanceAck(BalanceAck),
    #[serde(rename = "7")]
    BootstrapToken(BootstrapToken),
    #[serde(rename = "8")]
    BootstrapAck(BootstrapAck),
    #[serde(rename = "9")]
    RolloverInit(RolloverInit),
    #[serde(rename = "10")]
    RolloverReady(RolloverReady),
    #[serde(rename = "11")]
    ChannelClose(ChannelClose),
    #[serde(rename = "12")]
    CloseAck(CloseAck),
    #[serde(rename = "13")]
    Reject(Reject),
    #[serde(rename = "14")]
    Disconnect(Disconnect),
}

/// 0x00 Announce — first message from each peer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Announce {
    #[serde(rename = "1")]
    pub protocol_version: u8,
    #[serde(rename = "2")]
    pub pubkey: PubKey,
    #[serde(rename = "3")]
    pub unit: String,
    #[serde(rename = "4")]
    pub capabilities: u32,
}

/// 0x01 PriceSheet — product offerings with pricing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PriceSheet {
    #[serde(rename = "1")]
    pub products: Vec<Product>,
    #[serde(rename = "2")]
    pub interval_range: IntervalRange,
}

/// A product offering with mint options.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Product {
    #[serde(rename = "1")]
    pub product_id: Hash32,
    #[serde(rename = "2")]
    pub extensions: Vec<u8>,
    #[serde(rename = "3")]
    pub pricing_scale: u64,
    #[serde(rename = "4")]
    pub mint_options: Vec<MintOption>,
}

/// A mint option with pricing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MintOption {
    #[serde(rename = "1")]
    pub option_id: Hash32,
    #[serde(rename = "2")]
    pub mint_url: String,
    #[serde(rename = "3")]
    pub price_per_second: i64,
    #[serde(rename = "4")]
    pub price_per_unit: i64,
    #[serde(rename = "5")]
    pub mint_unit: String,
}

/// Metering interval range [min_ms, max_ms].
pub type IntervalRange = [u32; 2];

/// 0x02 Accept — accept a price sheet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Accept {
    #[serde(rename = "1")]
    pub product_id: Hash32,
    #[serde(rename = "2")]
    pub option_id: Hash32,
    #[serde(rename = "3")]
    pub interval_range: IntervalRange,
    #[serde(rename = "4")]
    pub channel_funding: Vec<u8>,
}

/// Channel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    #[serde(rename = "0")]
    AB = 0,
    #[serde(rename = "1")]
    BA = 1,
}

/// 0x03 ChannelReady — confirm Spilman channel active.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelReady {
    #[serde(rename = "1")]
    pub channel_id: Hash32,
    #[serde(rename = "2")]
    pub direction: Direction,
}

/// 0x04 MeteringReport — unsigned cumulative resource stats.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MeteringReport {
    #[serde(rename = "1")]
    pub elapsed_ms: u64,
    #[serde(rename = "2")]
    pub delivered: u64,
    #[serde(rename = "3")]
    pub received: u64,
    #[serde(rename = "4")]
    pub new_product_id: Option<Hash32>,
    #[serde(rename = "5")]
    pub new_pricing: Option<Vec<MintOption>>,
}

/// 0x05 BalanceUpdate — signed Spilman balance update.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BalanceUpdate {
    #[serde(rename = "1")]
    pub channel_id: Hash32,
    #[serde(rename = "2")]
    pub cumulative_balance: u64,
    #[serde(rename = "3")]
    pub balance_signature: Signature,
    #[serde(rename = "4")]
    pub net_amount: u64,
}

/// 0x06 BalanceAck — creditor confirms balance update.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BalanceAck {
    #[serde(rename = "1")]
    pub channel_id: Hash32,
    #[serde(rename = "2")]
    pub accepted_balance: u64,
}

/// 0x07 BootstrapToken — regular Cashu token for bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BootstrapToken {
    #[serde(rename = "1")]
    pub token: Vec<u8>,
}

/// Bootstrap verification status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BootstrapStatus {
    #[serde(rename = "0")]
    Accepted = 0,
    #[serde(rename = "1")]
    Rejected = 1,
}

/// 0x08 BootstrapAck — acknowledge bootstrap token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BootstrapAck {
    #[serde(rename = "1")]
    pub status: BootstrapStatus,
    #[serde(rename = "2")]
    pub reason: Option<String>,
}

/// 0x09 RolloverInit — initiate channel rollover.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RolloverInit {
    #[serde(rename = "1")]
    pub old_channel_id: Hash32,
    #[serde(rename = "2")]
    pub new_channel_funding: Vec<u8>,
}

/// 0x0A RolloverReady — new channel ready.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RolloverReady {
    #[serde(rename = "1")]
    pub old_channel_id: Hash32,
    #[serde(rename = "2")]
    pub new_channel_id: Hash32,
}

/// Reason for channel close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CloseReason {
    #[serde(rename = "0")]
    Normal = 0,
    #[serde(rename = "1")]
    PriceRejected = 1,
    #[serde(rename = "2")]
    PeerLeaving = 2,
}

/// 0x0B ChannelClose — request cooperative close.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelClose {
    #[serde(rename = "1")]
    pub channel_id: Hash32,
    #[serde(rename = "2")]
    pub final_balance: u64,
    #[serde(rename = "3")]
    pub final_signature: Signature,
    #[serde(rename = "4")]
    pub reason: CloseReason,
}

/// 0x0C CloseAck — acknowledge cooperative close.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CloseAck {
    #[serde(rename = "1")]
    pub channel_id: Hash32,
    #[serde(rename = "2")]
    pub accepted_balance: u64,
}

/// Machine-readable rejection reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum ReasonCode {
    PriceTooHigh = 0x01,
    MintNotAccepted = 0x02,
    UnitNotAccepted = 0x03,
    IntervalOutOfRange = 0x04,
    FundingInvalid = 0x05,
    BalanceVerificationFailed = 0x06,
    TransitLossExceeded = 0x07,
    RenegotiationRequired = 0x08,
    VersionUnsupported = 0x09,
    Other = 0xFF,
}

/// 0x0D Reject — general-purpose rejection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reject {
    #[serde(rename = "1")]
    pub rejected_type: u8,
    #[serde(rename = "2")]
    pub reason_code: ReasonCode,
    #[serde(rename = "3")]
    pub reason_text: Option<String>,
}

/// 0x0E Disconnect — orderly teardown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Disconnect {
    #[serde(rename = "1")]
    pub reason_code: ReasonCode,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn reason_code_values() {
        assert_eq!(ReasonCode::PriceTooHigh as u8, 0x01);
        assert_eq!(ReasonCode::Other as u8, 0xFF);
    }

    #[test]
    fn disconnect_roundtrip() {
        let msg = Message::Disconnect(Disconnect {
            reason_code: ReasonCode::VersionUnsupported,
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).unwrap();
        let decoded: Message = ciborium::from_reader(&buf[..]).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn announce_roundtrip() {
        let msg = Message::Announce(Announce {
            protocol_version: 1,
            pubkey: PubKey([0x02; 33]),
            unit: "bytes".to_owned(),
            capabilities: 0x01,
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).unwrap();
        let decoded: Message = ciborium::from_reader(&buf[..]).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn balance_update_roundtrip() {
        let msg = Message::BalanceUpdate(BalanceUpdate {
            channel_id: Hash32([0xAA; 32]),
            cumulative_balance: 1000,
            balance_signature: Signature([0xBB; 64]),
            net_amount: 50,
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&msg, &mut buf).unwrap();
        let decoded: Message = ciborium::from_reader(&buf[..]).unwrap();
        assert_eq!(msg, decoded);
    }
}
