//! Merchant advertisement and session event formatting for the v1
//! compatibility layer.
//!
//! Builds the Nostr events used by the v1 TollGate protocol:
//! - Kind 10021: TollGate advertisement (pricing discovery)
//! - Kind 1022:  Session event (grant after payment)
//! - Kind 21023: Notice event (errors / warnings)
//!
//! Ported from the experimental v1 archive's `merchant.rs` and
//! `merchant_provider.rs`, merged into a single module. All experimental
//! `tollgate_core` imports have been removed; the locally-defined
//! [`AcceptedMint`], [`V1ServerConfig`], and [`CustomerSession`] types
//! replace the experimental equivalents. Nostr types come from the
//! upstream `nostr` crate and [`super::nostr`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;

use super::wallet::CdkWallet;

// ---------------------------------------------------------------------------
// Configuration types (previously from the experimental v1 server module)
// ---------------------------------------------------------------------------

/// An accepted Cashu mint with its pricing terms.
///
/// Mirrors the Go v1 `AcceptedMint` struct.
#[derive(Debug, Clone)]
pub struct AcceptedMint {
    /// Mint URL.
    pub url: String,
    /// Price per step in the given unit.
    pub price_per_step: u64,
    /// Currency unit (e.g. `"sat"`, `"msat"`).
    pub unit: String,
    /// Minimum steps required per purchase.
    pub min_steps: u64,
}

/// Server-side configuration for building v1 Nostr events.
///
/// Contains the metric, step size, accepted mints, and Nostr signing keys
/// needed to produce advertisement and session events.
pub struct V1ServerConfig {
    /// Metric type: `"milliseconds"` or `"bytes"`.
    pub metric: String,
    /// Step size in metric units (e.g. `60000` for one minute).
    pub step_size: u64,
    /// Accepted mints with their pricing.
    pub accepted_mints: Vec<AcceptedMint>,
    /// Nostr keys used to sign events.
    pub nostr_keys: Keys,
    pub trust_proxy_headers: bool,
    pub wallet: Option<Arc<CdkWallet>>,
}

/// A customer session record.
///
/// Tracks the allotment granted to a customer after a successful payment.
#[derive(Debug, Clone)]
pub struct CustomerSession {
    /// Customer MAC address.
    pub mac_address: String,
    /// Session start time (Unix seconds).
    pub start_time: i64,
    /// Metric type: `"milliseconds"` or `"bytes"`.
    pub metric: String,
    /// Allotment remaining in metric units.
    pub allotment: u64,
}

// ---------------------------------------------------------------------------
// Allotment error
// ---------------------------------------------------------------------------

/// Errors from allotment calculation.
#[derive(Debug, thiserror::Error)]
pub enum AllotmentError {
    /// The requested mint URL is not in the server's accepted list.
    #[error("no accepted mint found for: {0}")]
    UnknownMint(String),
    /// The payment amount buys fewer steps than the mint's minimum.
    #[error("insufficient steps: got {steps}, minimum is {min_steps}")]
    InsufficientSteps { steps: u64, min_steps: u64 },
}

// ---------------------------------------------------------------------------
// Kind 10021 — Advertisement builder
// ---------------------------------------------------------------------------

/// Build a TollGate advertisement event (Nostr kind 10021) as JSON.
///
/// Encodes the server's metric, step size, accepted mints, pricing, and
/// implemented TIP numbers into a signed Nostr event. The resulting JSON
/// can be parsed back with
/// [`super::nostr::TollGateAdvertisement::from_json`].
pub fn build_advertisement(
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(
            TagKind::Custom("metric".into()),
            [config.metric.clone()],
        ),
        Tag::custom(
            TagKind::Custom("step_size".into()),
            [config.step_size.to_string()],
        ),
    ];

    for mint in &config.accepted_mints {
        tags.push(Tag::custom(
            TagKind::Custom("price_per_step".into()),
            [
                "cashu".to_owned(),
                mint.price_per_step.to_string(),
                mint.unit.clone(),
                mint.url.clone(),
                mint.min_steps.to_string(),
            ],
        ));
    }

    tags.push(Tag::custom(
        TagKind::Custom("tips".into()),
        ["1", "2", "3", "4"],
    ));

    let event = EventBuilder::new(Kind::Custom(10_021), "")
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

/// Calculate allotment from a payment amount.
///
/// Finds the mint matching `mint_url`, divides `amount_sats` by the
/// mint's `price_per_step`, and multiplies by the server's `step_size`.
/// Returns an error if the mint is unknown or the resulting steps are
/// below the mint's minimum.
pub fn calculate_allotment(
    amount_sats: u64,
    mint_url: &str,
    config: &V1ServerConfig,
) -> Result<u64, AllotmentError> {
    let mint = config
        .accepted_mints
        .iter()
        .find(|m| m.url == mint_url)
        .ok_or_else(|| AllotmentError::UnknownMint(mint_url.to_owned()))?;

    if mint.price_per_step == 0 {
        return Ok(config.step_size);
    }

    let steps = amount_sats / mint.price_per_step;
    if steps < mint.min_steps {
        return Err(AllotmentError::InsufficientSteps {
            steps,
            min_steps: mint.min_steps,
        });
    }

    Ok(steps * config.step_size)
}

// ---------------------------------------------------------------------------
// Kind 1022 — Session event builder
// ---------------------------------------------------------------------------

/// Build a session event (Nostr kind 1022) as JSON.
///
/// Emitted after a successful payment to grant the customer an allotment.
/// The resulting JSON can be parsed back with
/// [`super::nostr::SessionEvent::from_json`].
#[allow(clippy::too_many_arguments)]
pub fn build_session_event(
    session: &CustomerSession,
    config: &V1ServerConfig,
    customer_identifier: &str,
    amount_sat: u64,
    token_type: &str,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(
            TagKind::Custom("p".into()),
            [customer_identifier.to_owned()],
        ),
        Tag::custom(
            TagKind::Custom("allotment".into()),
            [session.allotment.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("metric".into()),
            [session.metric.clone()],
        ),
        Tag::custom(
            TagKind::Custom("device-identifier".into()),
            ["mac".to_owned(), session.mac_address.clone()],
        ),
        Tag::custom(
            TagKind::Custom("start-time".into()),
            [session.start_time.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("amount_sat".into()),
            [amount_sat.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("token_type".into()),
            [token_type.to_owned()],
        ),
    ];

    if amount_sat > 0 {
        let effective_rate = session.allotment / 1000 / amount_sat;
        tags.push(Tag::custom(
            TagKind::Custom("effective_rate".into()),
            [effective_rate.to_string()],
        ));
    }

    let event = EventBuilder::new(Kind::Custom(1022), "")
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

// ---------------------------------------------------------------------------
// Kind 21023 — Notice event builder
// ---------------------------------------------------------------------------

/// Build a notice event (Nostr kind 21023) as JSON.
///
/// Used for errors, warnings, and informational messages to the client.
/// The resulting JSON can be parsed back with
/// [`super::nostr::NoticeEvent::from_json`].
pub fn build_notice_event(
    level: &str,
    code: &str,
    message: &str,
    customer_identifier: Option<&str>,
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(TagKind::Custom("level".into()), [level.to_owned()]),
        Tag::custom(TagKind::Custom("code".into()), [code.to_owned()]),
    ];

    // Go v1 parity: include p tag when customer pubkey/MAC is available
    if let Some(id) = customer_identifier {
        if !id.is_empty() {
            tags.push(Tag::custom(
                TagKind::Custom("p".into()),
                [id.to_owned()],
            ));
        }
    }

    let event = EventBuilder::new(Kind::Custom(21_023), message)
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

// ---------------------------------------------------------------------------
// Session store trait and add_allotment (from merchant_provider.rs)
// ---------------------------------------------------------------------------

/// Alias for a boxed, sendable future returned by store methods.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Minimal session-store interface for the merchant module.
///
/// This is an object-safe subset of the experimental v1 `SessionStore`
/// trait, kept inline so the module avoids depending on the experimental
/// `session_store` module. All methods return boxed futures so the trait
/// can be used as `&dyn SessionStore`.
pub trait SessionStore: Send + Sync {
    /// Get a session by MAC address.
    fn get(&self, mac: &str) -> BoxFuture<'_, Result<Option<CustomerSession>, String>>;
    /// Insert a new session.
    fn insert(&self, session: CustomerSession) -> BoxFuture<'_, Result<(), String>>;
    /// Update an existing session.
    fn update(&self, mac: &str, session: CustomerSession) -> BoxFuture<'_, Result<(), String>>;
}

/// Add allotment to an existing session or create a new one.
///
/// Mirrors Go v1's `Merchant.AddAllotment(macAddress, metric, amount)`.
///
/// If a session for `mac` already exists, its allotment is increased by
/// `allotment` and `start_time` is reset to now. Otherwise a new session
/// is created with the given `metric` and `allotment`.
pub async fn add_allotment(
    sessions: &dyn SessionStore,
    mac: &str,
    metric: &str,
    allotment: u64,
) -> Result<CustomerSession, String> {
    #[allow(clippy::cast_possible_wrap)]
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let existing = sessions.get(mac).await?;

    let session = if let Some(mut s) = existing {
        s.allotment += allotment;
        s.start_time = now;
        let updated = s.clone();
        sessions.update(mac, s).await?;
        updated
    } else {
        let s = CustomerSession {
            mac_address: mac.to_owned(),
            start_time: now,
            metric: metric.to_owned(),
            allotment,
        };
        let cloned = s.clone();
        sessions.insert(s).await?;
        cloned
    };

    Ok(session)
}
