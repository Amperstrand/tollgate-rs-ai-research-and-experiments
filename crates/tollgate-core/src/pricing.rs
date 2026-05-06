use crate::types::Amount;
use crate::protocol::Hash32;

/// A resolved price for a single metering interval.
///
/// Computed from the product's pricing_scale, mint option's price_per_second/price_per_unit,
/// and the elapsed metering interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalPrice {
    /// Time-based component: (price_per_second * elapsed_ms) / 1000
    pub time_component: Amount,
    /// Unit-based component: (price_per_unit * delivered_units) / pricing_scale
    pub unit_component: Amount,
    /// Total price for this interval.
    pub total: Amount,
}

/// Domain-level product definition with resolved pricing.
///
/// Whereas `protocol::Product` is the wire format, this is the domain model
/// used by the pricing engine after negotiation is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainProduct {
    pub product_id: Hash32,
    pub pricing_scale: u64,
    pub resolved_mint: ResolvedMint,
}

/// A mint option that has been selected during negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMint {
    pub option_id: Hash32,
    pub mint_url: String,
    pub price_per_second: i64,
    pub price_per_unit: i64,
    pub mint_unit: String,
}

/// Pricing mode for a peer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMode {
    /// Normal paid mode — peer pays per interval.
    Paid,
    /// Zero-price mode — peer gets free access (whitelisted, promo, etc).
    ZeroPrice,
    /// Negative pricing — peer is being paid to accept traffic (inverse toll).
    NegativePrice,
}
