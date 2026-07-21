//! Wire messages and the identifiers that appear in them.

use alloc::string::String;
use alloc::vec::Vec;

use minicbor::bytes::{ByteArray, ByteVec};
use minicbor::{Decode, Encode};

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
/// carried on the wire.
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
/// explanation. The receiver should treat any pending negotiation with the
/// sender as rolled back.
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

    /// The [`RejectReason`], if the code is a known value.
    pub fn reason(&self) -> Option<RejectReason> {
        RejectReason::from_u8(self.reason_code)
    }

    /// Whether this rejection is the transit-loss warning (drift over
    /// tolerance). Convenience wrapper around [`Self::reason`].
    pub fn is_transit_loss(&self) -> bool {
        self.reason() == Some(RejectReason::TransitLossToleranceExceeded)
    }

    /// Whether this rejection signals a balance problem (the peer must top up
    /// or the mint could not verify funds). Maps to
    /// [`RejectReason::BalanceVerificationFailed`] — PR #65's negotiation-focused
    /// enum has no dedicated "exhausted" variant, so this is the closest match.
    pub fn is_balance_exhausted(&self) -> bool {
        self.reason() == Some(RejectReason::BalanceVerificationFailed)
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

/// A metering interval range proposed by a peer.
///
/// Encoded as a CBOR array `[min_ms, max_ms]`, not a map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(array)]
pub struct IntervalRange {
    /// Minimum interval in milliseconds.
    #[n(0)]
    pub min_ms: u32,
    /// Maximum interval in milliseconds.
    #[n(1)]
    pub max_ms: u32,
}

impl IntervalRange {
    pub fn new(min_ms: u32, max_ms: u32) -> Self {
        Self { min_ms, max_ms }
    }

    /// Whether `ms` falls within `[min_ms, max_ms]` (inclusive).
    pub fn contains(&self, ms: u32) -> bool {
        ms >= self.min_ms && ms <= self.max_ms
    }
}

/// A per-mint pricing option within a [`ProductEntry`].
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct MintOption {
    /// SHA256(mint_url | mint_unit) — canonical option fingerprint.
    #[n(1)]
    pub option_id: ByteArray<32>,
    /// The Cashu mint URL.
    #[n(2)]
    pub mint_url: String,
    /// Scaled integer price per second (may be negative).
    #[n(3)]
    pub price_per_second: i64,
    /// Scaled integer price per delivered unit (may be negative).
    #[n(4)]
    pub price_per_unit: i64,
    /// The mint's currency unit ("sat", "msat", "usd", …).
    #[n(5)]
    pub mint_unit: String,
}

impl MintOption {
    pub fn new(
        option_id: [u8; 32],
        mint_url: impl Into<String>,
        price_per_second: i64,
        price_per_unit: i64,
        mint_unit: impl Into<String>,
    ) -> Self {
        Self {
            option_id: ByteArray::from(option_id),
            mint_url: mint_url.into(),
            price_per_second,
            price_per_unit,
            mint_unit: mint_unit.into(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("MintOption encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// One priced product entry in a [`PriceSheet`].
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct ProductEntry {
    /// SHA256 of the full product — see `tollgate_protocol::product_id`.
    #[n(1)]
    pub product_id: ByteArray<32>,
    /// CBOR-encoded implementation-specific fields (opaque blob).
    #[n(2)]
    pub extensions: ByteVec,
    /// Pricing scale (default 1000 — prices are in milli-units).
    #[n(3)]
    pub pricing_scale: u32,
    /// The mint options offered for this product.
    #[n(4)]
    pub mint_options: Vec<MintOption>,
}

impl ProductEntry {
    pub fn new(
        product_id: [u8; 32],
        extensions: Vec<u8>,
        pricing_scale: u32,
        mint_options: Vec<MintOption>,
    ) -> Self {
        Self {
            product_id: ByteArray::from(product_id),
            extensions: ByteVec::from(extensions),
            pricing_scale,
            mint_options,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("ProductEntry encodes infallibly")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, minicbor::decode::Error> {
        minicbor::decode(bytes)
    }
}

/// [`MessageType::PriceSheet`] (0x01): the provider's pricing offer.
///
/// Contains an array of products, each with its own mint options, plus the
/// proposed metering interval range. The receiver inspects the offer and
/// responds with [`Accept`] or [`Reject`].
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PriceSheet {
    #[n(0)]
    pub type_tag: u8,
    /// The priced products being offered.
    #[n(1)]
    pub products: Vec<ProductEntry>,
    /// The proposed metering interval range `[min_ms, max_ms]`.
    #[n(2)]
    pub interval: IntervalRange,
}

impl PriceSheet {
    pub fn new(products: Vec<ProductEntry>, interval: IntervalRange) -> Self {
        Self {
            type_tag: MessageType::PriceSheet.as_u8(),
            products,
            interval,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("PriceSheet encodes infallibly")
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
    /// The agreed metering interval range.
    #[n(3)]
    pub interval: IntervalRange,
    /// Spilman channel funding proofs (opaque for now).
    #[n(4)]
    pub channel_funding: ByteVec,
}

impl Accept {
    pub fn new(
        product_id: [u8; 32],
        option_id: [u8; 32],
        interval: IntervalRange,
        channel_funding: Vec<u8>,
    ) -> Self {
        Self {
            type_tag: MessageType::Accept.as_u8(),
            product_id: ByteArray::from(product_id),
            option_id: ByteArray::from(option_id),
            interval,
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

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
    fn reject_with_null_reason() {
        let reject = Reject::new(MessageType::Accept, RejectReason::MintNotAccepted);
        let back = Reject::decode(&reject.encode()).expect("decode");
        assert_eq!(reject, back);
        assert_eq!(back.reason_text, None);
        assert_eq!(back.reason_code, RejectReason::MintNotAccepted.as_u8());
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
    fn interval_range_round_trips() {
        let range = IntervalRange::new(1000, 5000);
        let bytes = minicbor::to_vec(range).expect("encode");
        let back: IntervalRange = minicbor::decode(&bytes).expect("decode");
        assert_eq!(range, back);
        assert_eq!(back.min_ms, 1000);
        assert_eq!(back.max_ms, 5000);
    }

    #[test]
    fn interval_range_contains() {
        let range = IntervalRange::new(1000, 5000);
        assert!(range.contains(1000));
        assert!(range.contains(3000));
        assert!(range.contains(5000));
        assert!(!range.contains(999));
        assert!(!range.contains(5001));
    }

    #[test]
    fn price_sheet_round_trips() {
        let opt1 = MintOption::new([1u8; 32], "https://mint-a.example", 10, 100, "sat");
        let opt2 = MintOption::new([2u8; 32], "https://mint-b.example", 5, 50, "msat");
        let product = ProductEntry::new([9u8; 32], b"ext-data".to_vec(), 1000, vec![opt1, opt2]);
        let sheet = PriceSheet::new(vec![product], IntervalRange::new(1000, 5000));
        let back = PriceSheet::decode(&sheet.encode()).expect("decode");
        assert_eq!(sheet, back);
        assert_eq!(back.type_tag, MessageType::PriceSheet.as_u8());
        assert_eq!(back.products.len(), 1);
        assert_eq!(back.products[0].mint_options.len(), 2);
        assert_eq!(
            back.products[0].mint_options[0].mint_url,
            "https://mint-a.example"
        );
        assert_eq!(back.products[0].mint_options[1].mint_unit, "msat");
        assert_eq!(back.interval, IntervalRange::new(1000, 5000));
    }

    #[test]
    fn price_sheet_empty_products() {
        let sheet = PriceSheet::new(vec![], IntervalRange::new(500, 10000));
        let back = PriceSheet::decode(&sheet.encode()).expect("decode");
        assert_eq!(sheet, back);
        assert!(back.products.is_empty());
        assert_eq!(back.interval.max_ms, 10000);
    }

    #[test]
    fn accept_round_trips() {
        let accept = Accept::new(
            [7u8; 32],
            [3u8; 32],
            IntervalRange::new(2000, 6000),
            b"funding-proofs".to_vec(),
        );
        let back = Accept::decode(&accept.encode()).expect("decode");
        assert_eq!(accept, back);
        assert_eq!(back.type_tag, MessageType::Accept.as_u8());
        assert_eq!(back.product_id.as_slice(), &[7u8; 32]);
        assert_eq!(back.option_id.as_slice(), &[3u8; 32]);
        assert_eq!(back.interval, IntervalRange::new(2000, 6000));
        assert_eq!(back.channel_funding.as_slice(), b"funding-proofs");
    }

    #[test]
    fn accept_empty_funding() {
        let accept = Accept::new([1u8; 32], [2u8; 32], IntervalRange::new(100, 200), vec![]);
        let back = Accept::decode(&accept.encode()).expect("decode");
        assert_eq!(accept, back);
        assert!(back.channel_funding.is_empty());
    }
}
