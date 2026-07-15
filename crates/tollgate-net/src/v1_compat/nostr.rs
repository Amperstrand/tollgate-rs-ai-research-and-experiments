//! TIP (TollGate Implementation Possibility) Nostr event types.
//!
//! Parses and constructs the Nostr event kinds used by the v1 TollGate
//! protocol. Ported from the experimental v1 archive into the v1-compat
//! layer; uses only the upstream `nostr` crate — no experimental
//! `tollgate-core` types.
//!
//! # Kinds
//!
//! | Kind  | Struct                | Purpose                                  |
//! |-------|-----------------------|------------------------------------------|
//! | 10021 | [`TollGateAdvertisement`] | TollGate discovery (advertisement)  |
//! | 1022  | [`SessionEvent`]      | Session grant after successful payment    |
//! | 21000 | [`build_payment_event`] | Payment event carrying Cashu tokens     |
//! | 21023 | [`NoticeEvent`]       | Error / warning / info notice             |
//!
//! See TIP-01 through TIP-10 in the Go v1 reference implementation.

use nostr::event::tag::TagKind;
use nostr::prelude::*;

// ---------------------------------------------------------------------------
// Kind 10021 — TollGate Advertisement
// ---------------------------------------------------------------------------

/// TollGate Discovery event (Nostr kind 10021).
///
/// Advertises a TollGate's pricing, metric, and accepted mints.
/// See TIP-01 and TIP-02.
#[derive(Debug, Clone)]
pub struct TollGateAdvertisement {
    /// The signed Nostr event.
    pub event: Event,
}

impl TollGateAdvertisement {
    /// TollGate advertisement kind.
    pub const KIND: Kind = Kind::Custom(10_021);

    /// Parse from raw JSON.
    pub fn from_json(json: &str) -> Result<Self, V1NostrError> {
        let event = Event::from_json(json)?;
        if event.kind != Self::KIND {
            return Err(V1NostrError::WrongKind {
                expected: Self::KIND,
                got: event.kind,
            });
        }
        Ok(Self { event })
    }

    /// Metric type: "milliseconds" or "bytes".
    pub fn metric(&self) -> Option<String> {
        self.tag_value("metric")
    }

    /// Step size (e.g., `60000` for 1 minute when metric is milliseconds).
    pub fn step_size(&self) -> Option<u64> {
        self.tag_value("step_size").and_then(|s| s.parse().ok())
    }

    /// All pricing options from `price_per_step` tags.
    ///
    /// Each tag:
    /// `["price_per_step", "cashu", "<price>", "<unit>", "<mint_url>", "<min_steps>"]`
    pub fn pricing_options(&self) -> Vec<PricingOption> {
        let mut options = Vec::new();
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("price_per_step") && items.len() >= 5 {
                options.push(PricingOption {
                    asset_type: items[1].clone(),
                    price_per_step: items[2].parse().unwrap_or(0),
                    unit: items[3].clone(),
                    mint_url: items[4].clone(),
                    min_steps: items
                        .get(5)
                        .and_then(|s: &String| s.parse().ok())
                        .unwrap_or(0),
                });
            }
        }
        options
    }

    /// Implemented TIP numbers from `tips` tag.
    pub fn tips(&self) -> Vec<u32> {
        let mut tips = Vec::new();
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("tips") {
                for item in items.iter().skip(1) {
                    if let Ok(n) = item.parse() {
                        tips.push(n);
                    }
                }
            }
        }
        tips
    }

    /// Public key of the TollGate (from event).
    pub fn pubkey(&self) -> PublicKey {
        self.event.pubkey
    }

    fn tag_value(&self, name: &str) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some(name) {
                return items.get(1).cloned();
            }
        }
        None
    }
}

/// A single pricing option from a TollGate advertisement.
#[derive(Debug, Clone)]
pub struct PricingOption {
    /// Asset type (always "cashu" for now).
    pub asset_type: String,
    /// Price per step in the given unit.
    pub price_per_step: u64,
    /// Currency unit (e.g., "sat", "eur").
    pub unit: String,
    /// Mint URL for this pricing option.
    pub mint_url: String,
    /// Minimum steps required per purchase.
    pub min_steps: u64,
}

// ---------------------------------------------------------------------------
// Kind 1022 — Session Event
// ---------------------------------------------------------------------------

/// Session event (Nostr kind 1022).
///
/// Returned by a TollGate after successful payment. Contains the allotment
/// granted to the customer.
#[derive(Debug, Clone)]
pub struct SessionEvent {
    /// The signed Nostr event.
    pub event: Event,
}

impl SessionEvent {
    /// Session event kind.
    pub const KIND: Kind = Kind::Custom(1022);

    /// Parse from raw JSON.
    pub fn from_json(json: &str) -> Result<Self, V1NostrError> {
        let event = Event::from_json(json)?;
        if event.kind != Self::KIND {
            return Err(V1NostrError::WrongKind {
                expected: Self::KIND,
                got: event.kind,
            });
        }
        Ok(Self { event })
    }

    /// Allotment amount (in the metric units: milliseconds or bytes).
    pub fn allotment(&self) -> Option<u64> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("allotment") {
                return items.get(1).and_then(|s: &String| s.parse().ok());
            }
        }
        None
    }

    /// Metric type for this session ("milliseconds" or "bytes").
    pub fn metric(&self) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("metric") {
                return items.get(1).cloned();
            }
        }
        None
    }

    /// Device identifier from the session (MAC address).
    pub fn device_identifier(&self) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("device-identifier") {
                return items.get(2).cloned();
            }
        }
        None
    }

    /// Start time as Unix timestamp (seconds), from the `start-time` tag.
    pub fn start_time(&self) -> Option<i64> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("start-time") {
                return items.get(1).and_then(|s: &String| s.parse().ok());
            }
        }
        None
    }

    /// Customer pubkey or identifier from the `p` tag.
    ///
    /// This is NOT the event author — it identifies the customer
    /// (MAC address when no Nostr key is known).
    pub fn customer_pubkey(&self) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("p") {
                return items.get(1).cloned();
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Kind 21023 — Notice Event
// ---------------------------------------------------------------------------

/// Notice event (Nostr kind 21023).
///
/// Used by TollGates to communicate errors and warnings to clients.
#[derive(Debug, Clone)]
pub struct NoticeEvent {
    /// The signed Nostr event.
    pub event: Event,
}

impl NoticeEvent {
    /// Notice event kind.
    pub const KIND: Kind = Kind::Custom(21_023);

    /// Parse from raw JSON. Also accepts kind 1022 responses that are actually
    /// errors — the v1 server returns various error kinds.
    pub fn from_json(json: &str) -> Result<Self, V1NostrError> {
        let event = Event::from_json(json)?;
        if event.kind != Self::KIND {
            return Err(V1NostrError::WrongKind {
                expected: Self::KIND,
                got: event.kind,
            });
        }
        Ok(Self { event })
    }

    /// Severity level: "error", "warning", "info", "debug".
    pub fn level(&self) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("level") {
                return items.get(1).cloned();
            }
        }
        None
    }

    /// Machine-readable error code.
    pub fn code(&self) -> Option<String> {
        for tag in self.event.tags.iter() {
            let items = tag.as_slice();
            if items.first().map(String::as_str) == Some("code") {
                return items.get(1).cloned();
            }
        }
        None
    }

    /// Human-readable message.
    pub fn message(&self) -> &str {
        &self.event.content
    }
}

// ---------------------------------------------------------------------------
// Kind 21000 — Payment Event builder
// ---------------------------------------------------------------------------

/// Build a payment event (kind 21000) for sending Cashu tokens.
///
/// The v1 Go server accepts both plain Cashu tokens and Nostr payment events.
/// Using plain tokens is simpler, but we support both for completeness.
pub fn build_payment_event(
    keys: &Keys,
    tollgate_pubkey: PublicKey,
    mac_address: &str,
    cashu_token: &str,
) -> Result<Event, V1NostrError> {
    let tags = Tags::from_list(vec![
        Tag::custom(TagKind::Custom("p".into()), [tollgate_pubkey.to_hex()]),
        Tag::custom(
            TagKind::Custom("device-identifier".into()),
            ["mac", mac_address],
        ),
        Tag::custom(TagKind::Custom("payment".into()), [cashu_token]),
    ]);

    let event = EventBuilder::new(Kind::Custom(21_000), "")
        .tags(tags)
        .sign_with_keys(keys)?;
    Ok(event)
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from v1 Nostr event operations.
#[derive(Debug, thiserror::Error)]
pub enum V1NostrError {
    /// Event has unexpected kind.
    #[error("wrong event kind: expected {expected:?}, got {got:?}")]
    WrongKind { expected: Kind, got: Kind },
    /// Failed to parse event JSON.
    #[error("event parse error: {0}")]
    EventParse(#[from] nostr::event::Error),
    /// Failed to sign event.
    #[error("event builder error: {0}")]
    EventBuild(#[from] nostr::event::builder::Error),
}
