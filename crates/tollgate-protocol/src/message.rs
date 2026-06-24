//! Wire messages and the identifiers that appear in them.

use alloc::string::String;
use alloc::vec::Vec;

use minicbor::bytes::{ByteArray, ByteVec};
use minicbor::{Decode, Encode};

use crate::product::{MintPrice, option_id, product_id};

/// A peer's identity: the raw secp256k1 *compressed* public key (33 bytes).
///
/// Everything in the protocol and core works with these raw bytes. `npub` is
/// only the human-readable Bech32 rendering of the same key and lives at the
/// edges (config, logs, UI) — never on the wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PublicKey([u8; 33]);

impl PublicKey {
    pub const LEN: usize = 33;

    pub const fn from_bytes(bytes: [u8; 33]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 33] {
        &self.0
    }
}

/// The 15 TollGate message types (see `docs/design/core/tollgate-protocol.md`).
/// The discriminant is the value stored at CBOR field key `0`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

impl MessageType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::Announce,
            0x01 => Self::PriceSheet,
            0x02 => Self::Accept,
            0x03 => Self::ChannelReady,
            0x04 => Self::MeteringReport,
            0x05 => Self::BalanceUpdate,
            0x06 => Self::BalanceAck,
            0x07 => Self::BootstrapToken,
            0x08 => Self::BootstrapAck,
            0x09 => Self::RolloverInit,
            0x0A => Self::RolloverReady,
            0x0B => Self::ChannelClose,
            0x0C => Self::CloseAck,
            0x0D => Self::Reject,
            0x0E => Self::Disconnect,
            _ => return None,
        })
    }
}

/// [`MessageType::MeteringReport`] (0x04): cumulative, **unsigned** resource
/// stats exchanged each interval so both sides compute the same cost. Counters
/// are cumulative since the session baseline; no sequence number is needed (the
/// protocol is self-healing — a lost report is corrected by the next one's
/// totals).
///
/// Key 5 (`new_pricing`, the updated pricing array used for price renegotiation)
/// is reserved until the PriceSheet pricing types exist; only `new_product_id`
/// (key 4) is carried for now.
#[derive(Clone, PartialEq, Eq, Debug, Encode, Decode)]
#[cbor(map)]
pub struct MeteringReport {
    #[n(0)]
    pub type_tag: u8,
    /// Milliseconds since session start (cumulative).
    #[n(1)]
    pub elapsed_ms: u64,
    /// Cumulative units delivered TO the peer since session start.
    #[n(2)]
    pub delivered: u64,
    /// Cumulative units received FROM the peer since session start.
    #[n(3)]
    pub received: u64,
    /// Updated product id for the next interval, if the provider is changing
    /// price (renegotiation); `None` otherwise.
    #[n(4)]
    pub new_product_id: Option<ByteArray<32>>,
}

impl MeteringReport {
    pub fn new(elapsed_ms: u64, delivered: u64, received: u64) -> Self {
        Self {
            type_tag: MessageType::MeteringReport.as_u8(),
            elapsed_ms,
            delivered,
            received,
            new_product_id: None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("MeteringReport encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// Current TollGate protocol version, carried in [`Announce`] field 1.
pub const PROTOCOL_VERSION: u8 = 1;

/// Capability bit: peer can fund and sign Spilman channels. If unset in an
/// [`Announce`], the peer is bootstrap-only.
pub const CAP_SPILMAN: u32 = 0x01;

/// [`MessageType::Announce`] (0x00): the first message each peer sends. It
/// establishes the sender's identity (pubkey) and declares protocol version,
/// resource unit, and capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Announce {
    #[n(0)]
    pub type_tag: u8,
    #[n(1)]
    pub version: u8,
    #[n(2)]
    pub pubkey: ByteArray<33>,
    #[n(3)]
    pub unit: String,
    #[n(4)]
    pub capabilities: u32,
}

impl Announce {
    pub fn new(version: u8, pubkey: PublicKey, unit: impl Into<String>, capabilities: u32) -> Self {
        Self {
            type_tag: MessageType::Announce.as_u8(),
            version,
            pubkey: ByteArray::from(*pubkey.as_bytes()),
            unit: unit.into(),
            capabilities,
        }
    }

    /// The sender's public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_bytes(*self.pubkey)
    }

    /// Whether the peer advertises Spilman capability.
    pub fn supports_spilman(&self) -> bool {
        self.capabilities & CAP_SPILMAN != 0
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("Announce encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::BootstrapToken`] (0x07): a raw Cashu token, sent when a peer
/// cannot reach a mint and pays to get online.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BootstrapToken {
    #[n(0)]
    pub type_tag: u8,
    #[n(1)]
    pub token: ByteVec,
}

impl BootstrapToken {
    pub fn new(token: Vec<u8>) -> Self {
        Self {
            type_tag: MessageType::BootstrapToken.as_u8(),
            token: ByteVec::from(token),
        }
    }

    /// The raw token bytes (typically the UTF-8 of a `cashuB…` string).
    pub fn token_bytes(&self) -> Vec<u8> {
        self.token.to_vec()
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BootstrapToken encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::BootstrapAck`] (0x08): the provider's response to a
/// [`BootstrapToken`], sent only after verifying it with the mint.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BootstrapAck {
    #[n(0)]
    pub type_tag: u8,
    #[n(1)]
    pub status: u8,
    #[n(2)]
    pub reason: Option<String>,
}

impl BootstrapAck {
    pub const STATUS_ACCEPTED: u8 = 0;
    pub const STATUS_REJECTED: u8 = 1;

    pub fn accepted() -> Self {
        Self {
            type_tag: MessageType::BootstrapAck.as_u8(),
            status: Self::STATUS_ACCEPTED,
            reason: None,
        }
    }

    pub fn rejected(reason: impl Into<String>) -> Self {
        Self {
            type_tag: MessageType::BootstrapAck.as_u8(),
            status: Self::STATUS_REJECTED,
            reason: Some(reason.into()),
        }
    }

    pub fn is_accepted(&self) -> bool {
        self.status == Self::STATUS_ACCEPTED
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BootstrapAck encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// Machine-readable reason codes shared by [`Reject`] and [`Disconnect`]
/// (see `docs/design/core/tollgate-protocol.md`). The discriminant is the value
/// carried on the wire at field 2 (`reason_code`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RejectReason {
    /// The price quoted in a PriceSheet was too high.
    PriceTooHigh = 0x01,
    /// The Cashu mint is not on the local accept list.
    MintNotAccepted = 0x02,
    /// The resource unit is not supported.
    UnitNotAccepted = 0x03,
    /// The proposed metering interval is outside the allowed range.
    MeteringIntervalOutOfRange = 0x04,
    /// The channel funding amount or proofs are invalid.
    ChannelFundingInvalid = 0x05,
    /// Balance verification against the mint failed.
    BalanceVerificationFailed = 0x06,
    /// The observed transit loss exceeds the configured tolerance.
    TransitLossToleranceExceeded = 0x07,
    /// The product changed mid-session; renegotiation is required.
    ProductChangedRenegotiationRequired = 0x08,
    /// The peer's protocol version is not supported.
    ProtocolVersionUnsupported = 0x09,
    /// Any reason not covered above — see the accompanying `reason_text`.
    Other = 0xFF,
}

impl RejectReason {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::PriceTooHigh,
            0x02 => Self::MintNotAccepted,
            0x03 => Self::UnitNotAccepted,
            0x04 => Self::MeteringIntervalOutOfRange,
            0x05 => Self::ChannelFundingInvalid,
            0x06 => Self::BalanceVerificationFailed,
            0x07 => Self::TransitLossToleranceExceeded,
            0x08 => Self::ProductChangedRenegotiationRequired,
            0x09 => Self::ProtocolVersionUnsupported,
            0xFF => Self::Other,
            _ => return None,
        })
    }
}

/// [`MessageType::Reject`] (0x0D): a general-purpose rejection of a previously
/// received message. Carries the type of the rejected message, a
/// machine-readable [`RejectReason`], and an optional human-readable
/// explanation.
///
/// **Spec compliance**: fields match `docs/design/core/tollgate-protocol.md`
/// §0x0D exactly: `0: type, 1: rejected_type, 2: reason_code, 3: reason_text`.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Reject {
    #[n(0)]
    pub type_tag: u8,
    /// The [`MessageType`] discriminant of the message being rejected.
    #[n(1)]
    pub rejected_type: u8,
    /// A [`RejectReason`] discriminant.
    #[n(2)]
    pub reason_code: u8,
    /// Optional human-readable explanation; `None` to omit on the wire.
    #[n(3)]
    pub reason_text: Option<String>,
}

impl Reject {
    /// Balance-exhausted sentinel text used by the convenience methods below.
    /// The spec's reason-code table has no dedicated "balance exhausted" code,
    /// so we signal it via [`RejectReason::Other`] with this text.
    pub const BALANCE_EXHAUSTED_TEXT: &str = "balance exhausted";

    /// Build a [`Reject`] for `rejected_type` with the given reason and no
    /// human-readable text.
    pub fn new(rejected_type: MessageType, reason: RejectReason) -> Self {
        Self {
            type_tag: MessageType::Reject.as_u8(),
            rejected_type: rejected_type.as_u8(),
            reason_code: reason.as_u8(),
            reason_text: None,
        }
    }

    /// Attach a human-readable explanation.
    pub fn with_reason_text(mut self, text: impl Into<String>) -> Self {
        self.reason_text = Some(text.into());
        self
    }

    /// A balance-exhausted rejection — the peer must top up to resume.
    /// Uses [`RejectReason::Other`] with a sentinel text because the spec's
    /// reason-code table does not define a dedicated balance-exhausted code.
    pub fn balance_exhausted() -> Self {
        Self {
            type_tag: MessageType::Reject.as_u8(),
            rejected_type: MessageType::BootstrapToken.as_u8(),
            reason_code: RejectReason::Other.as_u8(),
            reason_text: Some(Self::BALANCE_EXHAUSTED_TEXT.into()),
        }
    }

    /// Whether this rejection is the balance-exhausted signal.
    pub fn is_balance_exhausted(&self) -> bool {
        self.reason_code == RejectReason::Other.as_u8()
            && self.reason_text.as_deref() == Some(Self::BALANCE_EXHAUSTED_TEXT)
    }

    /// The [`RejectReason`], if the code is a known value.
    pub fn reason(&self) -> Option<RejectReason> {
        RejectReason::from_u8(self.reason_code)
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("Reject encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::Disconnect`] (0x0E): an orderly teardown of the peer
/// relationship. Carries the same reason codes as [`Reject`]. On receipt the
/// receiver should release all per-peer state and resources.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Disconnect {
    #[n(0)]
    pub type_tag: u8,
    /// A [`RejectReason`] discriminant.
    #[n(1)]
    pub reason_code: u8,
}

impl Disconnect {
    /// Build a [`Disconnect`] with the given reason.
    pub fn new(reason: RejectReason) -> Self {
        Self {
            type_tag: MessageType::Disconnect.as_u8(),
            reason_code: reason.as_u8(),
        }
    }

    /// The [`RejectReason`], if the code is a known value.
    pub fn reason(&self) -> Option<RejectReason> {
        RejectReason::from_u8(self.reason_code)
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("Disconnect encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::Accept`] (0x02): acceptance of a [`PriceSheet`] offer.
///
/// References the chosen product and mint option, carries the agreed
/// metering interval, and includes the Spilman channel funding proofs
/// (opaque blob for now — full channel setup arrives later).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Accept {
    #[n(0)]
    pub type_tag: u8,
    /// The accepted product's id (from [`PriceSheet::products`]).
    #[n(1)]
    pub product_id: ByteArray<32>,
    /// The chosen mint option's id.
    #[n(2)]
    pub option_id: ByteArray<32>,
    /// The agreed metering interval range `[min_ms, max_ms]`.
    #[n(3)]
    pub interval_ms: (u32, u32),
    /// Spilman channel funding proofs (opaque for now).
    #[n(4)]
    pub channel_funding: ByteVec,
}

impl Accept {
    pub fn new(
        product_id: [u8; 32],
        option_id: [u8; 32],
        min_interval_ms: u32,
        max_interval_ms: u32,
        channel_funding: Vec<u8>,
    ) -> Self {
        Self {
            type_tag: MessageType::Accept.as_u8(),
            product_id: ByteArray::from(product_id),
            option_id: ByteArray::from(option_id),
            interval_ms: (min_interval_ms, max_interval_ms),
            channel_funding: ByteVec::from(channel_funding),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("Accept encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::ChannelReady`] (0x03): both peers send this after verifying
/// the other's Spilman funding proofs. Resource metering begins when both
/// directions are ready.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ChannelReady {
    #[n(0)]
    pub type_tag: u8,
    /// The Spilman channel ID.
    #[n(1)]
    pub channel_id: ByteArray<32>,
    /// Channel direction: `0 = A→B`, `1 = B→A`.
    #[n(2)]
    pub direction: u8,
}

impl ChannelReady {
    pub const DIR_A_TO_B: u8 = 0;
    pub const DIR_B_TO_A: u8 = 1;

    pub fn new(channel_id: [u8; 32], direction: u8) -> Self {
        Self {
            type_tag: MessageType::ChannelReady.as_u8(),
            channel_id: ByteArray::from(channel_id),
            direction,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("ChannelReady encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::BalanceUpdate`] (0x05): sent by the **net debtor** after both
/// [`MeteringReport`] messages have been exchanged. Contains the signed Spilman
/// balance update for only the net amount owed.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BalanceUpdate {
    #[n(0)]
    pub type_tag: u8,
    /// The debtor's Spilman channel ID.
    #[n(1)]
    pub channel_id: ByteArray<32>,
    /// New cumulative balance on this channel.
    #[n(2)]
    pub cumulative_balance: u64,
    /// Schnorr signature over the balance update (64 bytes).
    #[n(3)]
    pub balance_signature: ByteArray<64>,
    /// The net amount being charged this interval.
    #[n(4)]
    pub net_amount: u64,
}

impl BalanceUpdate {
    pub fn new(
        channel_id: [u8; 32],
        cumulative_balance: u64,
        balance_signature: [u8; 64],
        net_amount: u64,
    ) -> Self {
        Self {
            type_tag: MessageType::BalanceUpdate.as_u8(),
            channel_id: ByteArray::from(channel_id),
            cumulative_balance,
            balance_signature: ByteArray::from(balance_signature),
            net_amount,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BalanceUpdate encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::BalanceAck`] (0x06): sent by the creditor to confirm a
/// [`BalanceUpdate`].
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BalanceAck {
    #[n(0)]
    pub type_tag: u8,
    /// The Spilman channel ID.
    #[n(1)]
    pub channel_id: ByteArray<32>,
    /// The cumulative balance we acknowledge.
    #[n(2)]
    pub accepted_balance: u64,
}

impl BalanceAck {
    pub fn new(channel_id: [u8; 32], accepted_balance: u64) -> Self {
        Self {
            type_tag: MessageType::BalanceAck.as_u8(),
            channel_id: ByteArray::from(channel_id),
            accepted_balance,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BalanceAck encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::RolloverInit`] (0x09): sent by the channel funder when its
/// outgoing channel approaches exhaustion. Carries funding proofs for a new
/// channel.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct RolloverInit {
    #[n(0)]
    pub type_tag: u8,
    /// The current (exhausting) channel ID.
    #[n(1)]
    pub old_channel_id: ByteArray<32>,
    /// Spilman funding proofs for the new channel.
    #[n(2)]
    pub new_channel_funding: ByteVec,
}

impl RolloverInit {
    pub fn new(old_channel_id: [u8; 32], new_channel_funding: Vec<u8>) -> Self {
        Self {
            type_tag: MessageType::RolloverInit.as_u8(),
            old_channel_id: ByteArray::from(old_channel_id),
            new_channel_funding: ByteVec::from(new_channel_funding),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("RolloverInit encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::RolloverReady`] (0x0A): sent by the receiver to confirm the
/// new channel is funded and ready. The old channel continues draining to
/// 100%; charges continue on the new channel seamlessly.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct RolloverReady {
    #[n(0)]
    pub type_tag: u8,
    /// The old (exhausting) channel ID.
    #[n(1)]
    pub old_channel_id: ByteArray<32>,
    /// The new Spilman channel ID.
    #[n(2)]
    pub new_channel_id: ByteArray<32>,
}

impl RolloverReady {
    pub fn new(old_channel_id: [u8; 32], new_channel_id: [u8; 32]) -> Self {
        Self {
            type_tag: MessageType::RolloverReady.as_u8(),
            old_channel_id: ByteArray::from(old_channel_id),
            new_channel_id: ByteArray::from(new_channel_id),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("RolloverReady encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::ChannelClose`] (0x0B): request cooperative close of a channel.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ChannelClose {
    #[n(0)]
    pub type_tag: u8,
    /// The channel being closed.
    #[n(1)]
    pub channel_id: ByteArray<32>,
    /// Proposed final balance.
    #[n(2)]
    pub final_balance: u64,
    /// Schnorr signature over the final balance (64 bytes).
    #[n(3)]
    pub final_signature: ByteArray<64>,
    /// Close reason: `0 = normal`, `1 = price_rejected`, `2 = peer_leaving`.
    #[n(4)]
    pub reason: u8,
}

impl ChannelClose {
    pub const REASON_NORMAL: u8 = 0;
    pub const REASON_PRICE_REJECTED: u8 = 1;
    pub const REASON_PEER_LEAVING: u8 = 2;

    pub fn new(
        channel_id: [u8; 32],
        final_balance: u64,
        final_signature: [u8; 64],
        reason: u8,
    ) -> Self {
        Self {
            type_tag: MessageType::ChannelClose.as_u8(),
            channel_id: ByteArray::from(channel_id),
            final_balance,
            final_signature: ByteArray::from(final_signature),
            reason,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("ChannelClose encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::CloseAck`] (0x0C): acknowledge a cooperative close.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct CloseAck {
    #[n(0)]
    pub type_tag: u8,
    /// The channel being closed.
    #[n(1)]
    pub channel_id: ByteArray<32>,
    /// The agreed final balance.
    #[n(2)]
    pub accepted_balance: u64,
}

impl CloseAck {
    pub fn new(channel_id: [u8; 32], accepted_balance: u64) -> Self {
        Self {
            type_tag: MessageType::CloseAck.as_u8(),
            channel_id: ByteArray::from(channel_id),
            accepted_balance,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("CloseAck encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// One mint option inside a [`ProductOffer`]: a mint URL and the price charged
/// when paying through it. `option_id` is the canonical reference an [`Accept`]
/// uses to name the chosen option unambiguously. Nested object — no type tag.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct MintOption {
    #[n(1)]
    pub option_id: ByteArray<32>,
    #[n(2)]
    pub mint_url: String,
    #[n(3)]
    pub price_per_second: i64,
    #[n(4)]
    pub price_per_unit: i64,
    #[n(5)]
    pub mint_unit: String,
}

impl MintOption {
    /// Build a wire option from a [`MintPrice`], computing its `option_id`.
    pub fn from_price(price: &MintPrice) -> Self {
        Self {
            option_id: ByteArray::from(option_id(&price.mint_url, &price.mint_unit)),
            mint_url: price.mint_url.clone(),
            price_per_second: price.price_per_second,
            price_per_unit: price.price_per_unit,
            mint_unit: price.mint_unit.clone(),
        }
    }
}

/// One product offering inside a [`PriceSheet`]: a priced bundle across one or
/// more mints, identified by its canonical [`product_id`]. Nested object — the
/// map starts at field 1 and carries no type tag.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ProductOffer {
    #[n(1)]
    pub product_id: ByteArray<32>,
    /// Opaque, implementation-defined extension bytes, hashed into the id.
    #[n(2)]
    pub extensions: ByteVec,
    #[n(3)]
    pub pricing_scale: u32,
    #[n(4)]
    pub mints: Vec<MintOption>,
}

impl ProductOffer {
    /// Build an offer from per-mint prices, computing the canonical `product_id`
    /// and each mint's `option_id` so the offer is self-describing on the wire.
    pub fn new(pricing_scale: u32, prices: &[MintPrice], extensions: Vec<u8>) -> Self {
        let pid = product_id(pricing_scale, prices, &extensions);
        Self {
            product_id: ByteArray::from(pid.0),
            extensions: ByteVec::from(extensions),
            pricing_scale,
            mints: prices.iter().map(MintOption::from_price).collect(),
        }
    }
}

/// [`MessageType::PriceSheet`] (0x01): a peer's "take it or leave it" offer, sent
/// after [`Announce`]. Carries product offerings and the metering interval range
/// this peer will accept; the other side picks one product + mint option (and,
/// on the Spilman path, replies with an Accept). In bootstrap-only mode the
/// client just reads it to learn the price and which mints are accepted, then
/// sends a [`BootstrapToken`]. See `docs/design/core/tollgate-protocol.md`.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PriceSheet {
    #[n(0)]
    pub type_tag: u8,
    #[n(1)]
    pub products: Vec<ProductOffer>,
    /// `(min_interval_ms, max_interval_ms)` — the acceptable metering interval
    /// range (CBOR array `[min, max]`).
    #[n(2)]
    pub interval_ms: (u32, u32),
}

impl PriceSheet {
    pub fn new(products: Vec<ProductOffer>, min_interval_ms: u32, max_interval_ms: u32) -> Self {
        Self {
            type_tag: MessageType::PriceSheet.as_u8(),
            products,
            interval_ms: (min_interval_ms, max_interval_ms),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("PriceSheet encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_round_trips() {
        for raw in 0x00u8..=0x0E {
            let ty = MessageType::from_u8(raw).expect("known type");
            assert_eq!(ty.as_u8(), raw);
        }
        assert!(MessageType::from_u8(0x0F).is_none());
        assert!(MessageType::from_u8(0xFF).is_none());
    }

    #[test]
    fn metering_report_cbor_round_trips() {
        let report = MeteringReport::new(5000, 100, 40);
        let back = MeteringReport::decode(&report.encode()).expect("decode");
        assert_eq!(report, back);
        assert_eq!(back.type_tag, MessageType::MeteringReport.as_u8());
        assert_eq!(back.elapsed_ms, 5000);
        assert_eq!(back.new_product_id, None);
    }

    #[test]
    fn metering_report_carries_renegotiated_product_id() {
        let mut report = MeteringReport::new(1000, 1, 2);
        report.new_product_id = Some(ByteArray::from([9u8; 32]));
        let back = MeteringReport::decode(&report.encode()).expect("decode");
        assert_eq!(back.new_product_id, Some(ByteArray::from([9u8; 32])));
    }

    #[test]
    fn announce_round_trips_and_exposes_pubkey() {
        let pk = PublicKey::from_bytes([7u8; 33]);
        let announce = Announce::new(1, pk, "bytes", CAP_SPILMAN);
        let bytes = announce.encode();
        let back = Announce::decode(&bytes).expect("decode");
        assert_eq!(announce, back);
        assert_eq!(back.public_key(), pk);
        assert_eq!(back.type_tag, MessageType::Announce.as_u8());
        assert!(back.supports_spilman());
    }

    #[test]
    fn announce_without_spilman_capability() {
        let announce = Announce::new(1, PublicKey::from_bytes([1u8; 33]), "wh", 0);
        assert!(!announce.supports_spilman());
    }

    #[test]
    fn bootstrap_token_round_trips() {
        let token = BootstrapToken::new(b"cashuBsometoken".to_vec());
        let bytes = token.encode();
        let back = BootstrapToken::decode(&bytes).expect("decode");
        assert_eq!(back.token_bytes(), b"cashuBsometoken");
        assert_eq!(back.type_tag, MessageType::BootstrapToken.as_u8());
    }

    #[test]
    fn price_sheet_round_trips_with_products_and_mints() {
        use crate::product::MintPrice;
        use alloc::string::ToString;

        let prices = alloc::vec![
            MintPrice {
                mint_url: "https://mint-a.example".to_string(),
                price_per_second: 0,
                price_per_unit: 1,
                mint_unit: "sat".to_string(),
            },
            MintPrice {
                mint_url: "https://mint-b.example".to_string(),
                price_per_second: 2,
                price_per_unit: 3,
                mint_unit: "sat".to_string(),
            },
        ];
        let offer = ProductOffer::new(1000, &prices, alloc::vec![]);
        let sheet = PriceSheet::new(alloc::vec![offer], 5000, 60000);

        let back = PriceSheet::decode(&sheet.encode()).expect("decode PriceSheet");
        assert_eq!(sheet, back);
        assert_eq!(back.type_tag, MessageType::PriceSheet.as_u8());
        assert_eq!(back.interval_ms, (5000, 60000));
        assert_eq!(back.products.len(), 1);
        assert_eq!(back.products[0].mints.len(), 2);

        // The wire option_id matches the canonical helper, and product_id is the
        // declaration-order-independent fingerprint of the same prices.
        assert_eq!(
            back.products[0].mints[0].option_id.as_slice(),
            &option_id("https://mint-a.example", "sat")
        );
        assert_eq!(
            back.products[0].product_id.as_slice(),
            &product_id(1000, &prices, b"").0
        );

        // peek_type identifies it without a full decode.
        assert_eq!(
            crate::peek_type(&sheet.encode()),
            Some(MessageType::PriceSheet)
        );
    }

    #[test]
    fn price_sheet_round_trips_when_empty_or_mintless() {
        // No products at all (e.g. a pure client that sells nothing).
        let empty = PriceSheet::new(alloc::vec![], 1000, 2000);
        assert_eq!(PriceSheet::decode(&empty.encode()).expect("decode"), empty);
        assert!(empty.products.is_empty());

        // A product with zero mint options (a gateway with no accepted mints).
        let mintless = PriceSheet::new(
            alloc::vec![ProductOffer::new(1000, &[], alloc::vec![])],
            5000,
            60000,
        );
        let back = PriceSheet::decode(&mintless.encode()).expect("decode");
        assert_eq!(back, mintless);
        assert_eq!(back.products.len(), 1);
        assert!(back.products[0].mints.is_empty());
    }

    #[test]
    fn reject_reason_round_trips_through_u8() {
        for code in [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0xFF] {
            let reason = RejectReason::from_u8(code).expect("known code");
            assert_eq!(reason.as_u8(), code);
        }
        assert!(RejectReason::from_u8(0x00).is_none());
        assert!(RejectReason::from_u8(0x10).is_none());
    }

    #[test]
    fn reject_round_trips() {
        let reject = Reject::new(MessageType::PriceSheet, RejectReason::PriceTooHigh)
            .with_reason_text("too expensive");
        let back = Reject::decode(&reject.encode()).expect("decode");
        assert_eq!(reject, back);
        assert_eq!(back.type_tag, MessageType::Reject.as_u8());
        assert_eq!(back.rejected_type, MessageType::PriceSheet.as_u8());
        assert_eq!(back.reason_code, RejectReason::PriceTooHigh.as_u8());
        assert_eq!(back.reason(), Some(RejectReason::PriceTooHigh));
        assert_eq!(back.reason_text.as_deref(), Some("too expensive"));
    }

    #[test]
    fn reject_balance_exhausted_round_trips() {
        let reject = Reject::balance_exhausted();
        assert!(reject.is_balance_exhausted());
        assert_eq!(reject.type_tag, MessageType::Reject.as_u8());

        let back = Reject::decode(&reject.encode()).expect("decode");
        assert_eq!(reject, back);
        assert!(back.is_balance_exhausted());
        assert_eq!(back.reason(), Some(RejectReason::Other));
    }

    #[test]
    fn disconnect_round_trips() {
        let disconnect = Disconnect::new(RejectReason::ProtocolVersionUnsupported);
        let back = Disconnect::decode(&disconnect.encode()).expect("decode");
        assert_eq!(disconnect, back);
        assert_eq!(back.type_tag, MessageType::Disconnect.as_u8());
        assert_eq!(
            back.reason_code,
            RejectReason::ProtocolVersionUnsupported.as_u8()
        );
        assert_eq!(
            back.reason(),
            Some(RejectReason::ProtocolVersionUnsupported)
        );
    }

    #[test]
    fn accept_round_trips() {
        let accept = Accept::new([7u8; 32], [3u8; 32], 2000, 6000, b"funding".to_vec());
        let back = Accept::decode(&accept.encode()).expect("decode");
        assert_eq!(accept, back);
        assert_eq!(back.type_tag, MessageType::Accept.as_u8());
        assert_eq!(back.product_id.as_slice(), &[7u8; 32]);
        assert_eq!(back.option_id.as_slice(), &[3u8; 32]);
        assert_eq!(back.interval_ms, (2000, 6000));
        assert_eq!(back.channel_funding.as_slice(), b"funding");
    }

    #[test]
    fn channel_ready_round_trips() {
        let ready = ChannelReady::new([0xAB; 32], ChannelReady::DIR_A_TO_B);
        let back = ChannelReady::decode(&ready.encode()).expect("decode");
        assert_eq!(ready, back);
        assert_eq!(back.type_tag, MessageType::ChannelReady.as_u8());
        assert_eq!(back.direction, 0);
    }

    #[test]
    fn balance_update_round_trips() {
        let update = BalanceUpdate::new([1u8; 32], 50000, [2u8; 64], 250);
        let back = BalanceUpdate::decode(&update.encode()).expect("decode");
        assert_eq!(update, back);
        assert_eq!(back.type_tag, MessageType::BalanceUpdate.as_u8());
        assert_eq!(back.cumulative_balance, 50000);
        assert_eq!(back.net_amount, 250);
    }

    #[test]
    fn balance_ack_round_trips() {
        let ack = BalanceAck::new([4u8; 32], 49999);
        let back = BalanceAck::decode(&ack.encode()).expect("decode");
        assert_eq!(ack, back);
        assert_eq!(back.type_tag, MessageType::BalanceAck.as_u8());
        assert_eq!(back.accepted_balance, 49999);
    }

    #[test]
    fn rollover_init_round_trips() {
        let init = RolloverInit::new([5u8; 32], b"new-funding".to_vec());
        let back = RolloverInit::decode(&init.encode()).expect("decode");
        assert_eq!(init, back);
        assert_eq!(back.type_tag, MessageType::RolloverInit.as_u8());
        assert_eq!(back.new_channel_funding.as_slice(), b"new-funding");
    }

    #[test]
    fn rollover_ready_round_trips() {
        let ready = RolloverReady::new([5u8; 32], [6u8; 32]);
        let back = RolloverReady::decode(&ready.encode()).expect("decode");
        assert_eq!(ready, back);
        assert_eq!(back.type_tag, MessageType::RolloverReady.as_u8());
        assert_eq!(back.old_channel_id.as_slice(), &[5u8; 32]);
        assert_eq!(back.new_channel_id.as_slice(), &[6u8; 32]);
    }

    #[test]
    fn channel_close_round_trips() {
        let close = ChannelClose::new(
            [8u8; 32],
            42000,
            [9u8; 64],
            ChannelClose::REASON_PRICE_REJECTED,
        );
        let back = ChannelClose::decode(&close.encode()).expect("decode");
        assert_eq!(close, back);
        assert_eq!(back.type_tag, MessageType::ChannelClose.as_u8());
        assert_eq!(back.final_balance, 42000);
        assert_eq!(back.reason, ChannelClose::REASON_PRICE_REJECTED);
    }

    #[test]
    fn close_ack_round_trips() {
        let ack = CloseAck::new([8u8; 32], 42000);
        let back = CloseAck::decode(&ack.encode()).expect("decode");
        assert_eq!(ack, back);
        assert_eq!(back.type_tag, MessageType::CloseAck.as_u8());
        assert_eq!(back.accepted_balance, 42000);
    }

    #[test]
    fn bootstrap_ack_round_trips() {
        let accepted = BootstrapAck::accepted();
        let back = BootstrapAck::decode(&accepted.encode()).expect("decode");
        assert!(back.is_accepted());
        assert_eq!(back.reason, None);

        let rejected = BootstrapAck::rejected("mint unreachable").encode();
        let back = BootstrapAck::decode(&rejected).expect("decode");
        assert!(!back.is_accepted());
        assert_eq!(back.reason.as_deref(), Some("mint unreachable"));
    }
}
