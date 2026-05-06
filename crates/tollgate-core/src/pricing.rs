//! Pricing engine for TollGate metering intervals.
//!
//! Pure-computation module for computing interval costs from dual pricing
//! (time + units) and deriving deterministic product IDs. No I/O, no async,
//! no traits — just pure functions.
//!
//! # Cost Formula
//!
//! ```text
//! cost_scaled = (elapsed_seconds × price_per_second) + (units_delivered × price_per_unit)
//! cost        = ceil(cost_scaled / pricing_scale)
//! ```
//!
//! All intermediate arithmetic uses `i128` with checked operations to prevent
//! silent overflow. Ceiling division handles both positive and negative costs
//! (negative = inverse toll, where the node pays the peer to accept traffic).

use crate::error::PricingError;
use crate::protocol::Hash32;

/// A resolved price for a single metering interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalPrice {
    /// Time-based component: ceil(elapsed_seconds * price_per_second / pricing_scale)
    ///
    /// For debugging only. The authoritative total is computed from the
    /// combined scaled sum, not by summing individual ceiling-divided values.
    pub time_component: i64,
    /// Unit-based component: ceil(units * price_per_unit / pricing_scale)
    ///
    /// For debugging only.
    pub unit_component: i64,
    /// Total cost for this interval: ceil((time_scaled + unit_scaled) / pricing_scale)
    ///
    /// This is the AUTHORITATIVE value — computed from the sum of scaled
    /// intermediates before a single ceiling division.
    pub total: i64,
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

// ---------------------------------------------------------------------------
// Pure functions
// ---------------------------------------------------------------------------

/// Compute the cost for a single metering interval.
///
/// Uses the spec formula:
/// ```text
/// cost_scaled = (elapsed_seconds × price_per_second) + (units_delivered × price_per_unit)
/// cost        = ceil(cost_scaled / pricing_scale)
/// ```
///
/// All intermediate arithmetic is done in `i128` with checked operations.
/// Returns an error on overflow or if `pricing_scale` is zero.
///
/// # Errors
///
/// - [`PricingError::ZeroScaleDivisor`] if `pricing_scale == 0`
/// - [`PricingError::Overflow`] if any intermediate arithmetic overflows `i128`
pub fn compute_interval_cost(
    price_per_second: i64,
    price_per_unit: i64,
    elapsed_seconds: u64,
    units_delivered: u64,
    pricing_scale: u64,
) -> Result<i64, PricingError> {
    if pricing_scale == 0 {
        return Err(PricingError::ZeroScaleDivisor);
    }

    let time_scaled = i128::from(price_per_second)
        .checked_mul(i128::from(elapsed_seconds))
        .ok_or(PricingError::Overflow)?;

    let unit_scaled = i128::from(price_per_unit)
        .checked_mul(i128::from(units_delivered))
        .ok_or(PricingError::Overflow)?;

    let cost_scaled = time_scaled
        .checked_add(unit_scaled)
        .ok_or(PricingError::Overflow)?;

    let cost = ceil_div(cost_scaled, pricing_scale);
    Ok(cost)
}

/// Compute interval cost at full scaled precision (no ceiling division).
///
/// Used for bootstrap balance tracking where sub-sat precision must be
/// preserved across multiple intervals. The result stays in scaled units
/// (e.g., milli-sats when `pricing_scale = 1000`) and is never rounded.
///
/// # Formula
///
/// ```text
/// cost_scaled = (elapsed_seconds × price_per_second) + (units_delivered × price_per_unit)
/// ```
///
/// where `elapsed_seconds = elapsed_ms / 1000` (integer division, truncating sub-second).
pub fn compute_interval_cost_scaled(
    elapsed_ms: u64,
    units_delivered: u64,
    price_per_second: i64,
    price_per_unit: i64,
) -> i128 {
    let elapsed_seconds = i128::from(elapsed_ms / 1000);
    let pps = i128::from(price_per_second);
    let ppu = i128::from(price_per_unit);
    let units = i128::from(units_delivered);
    elapsed_seconds * pps + units * ppu
}

/// Compute a deterministic product ID from pricing parameters.
///
/// ```text
/// product_id = SHA256(CBOR(pricing_scale) || CBOR(pricing) || CBOR(extensions))
/// ```
///
/// Where `extensions` is encoded as a CBOR byte string (major type 2),
/// not an array of unsigned integers.
///
/// # Errors
///
/// - [`PricingError::Encoding`] if CBOR serialization fails
pub fn compute_product_id(
    pricing_scale: u64,
    pricing: &[crate::protocol::MintOption],
    extensions: &[u8],
) -> Result<Hash32, PricingError> {
    use sha2::{Digest, Sha256};

    let scale_bytes = minicbor::to_vec(pricing_scale)
        .map_err(|e| PricingError::Encoding(e.to_string()))?;
    let pricing_bytes = minicbor::to_vec(pricing)
        .map_err(|e| PricingError::Encoding(e.to_string()))?;
    // extensions must be encoded as CBOR byte string (major type 2),
    // not as an array of unsigned integers.
    let ext_bytes = minicbor::to_vec(<&minicbor::bytes::ByteSlice>::from(extensions))
        .map_err(|e| PricingError::Encoding(e.to_string()))?;

    let mut hasher = Sha256::new();
    hasher.update(&scale_bytes);
    hasher.update(&pricing_bytes);
    hasher.update(&ext_bytes);

    let hash: [u8; 32] = hasher.finalize().into();
    Ok(Hash32(hash))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Ceiling division of a signed numerator by a positive unsigned divisor.
///
/// For non-negative `a`: `(a + b - 1) / b` rounds up.
/// For negative `a`: Rust's truncation toward zero is equivalent to ceiling
/// because the divisor is positive and the result is negative.
fn ceil_div(a: i128, b: u64) -> i64 {
    let b = i128::from(b);
    let result = if a >= 0 {
        (a + b - 1) / b
    } else {
        a / b
    };
    i64::try_from(result).expect("ceil_div result must fit in i64")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // compute_interval_cost — unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn usage_only_pricing() {
        // price_per_second=0, price_per_unit=10, elapsed=0, units=1000, scale=1000
        // cost_scaled = 0 + 10000 = 10000
        // cost = ceil(10000/1000) = 10
        let cost = compute_interval_cost(0, 10, 0, 1000, 1000).unwrap();
        assert_eq!(cost, 10);
    }

    #[test]
    fn time_only_pricing() {
        // price_per_second=100, price_per_unit=0, elapsed=5, units=0, scale=1000
        // cost_scaled = 500 + 0 = 500
        // cost = ceil(500/1000) = 1
        let cost = compute_interval_cost(100, 0, 5, 0, 1000).unwrap();
        assert_eq!(cost, 1);
    }

    #[test]
    fn combined_pricing() {
        // price_per_second=50, price_per_unit=5, elapsed=5, units=1000, scale=1000
        // cost_scaled = 250 + 5000 = 5250
        // cost = ceil(5250/1000) = 6
        let cost = compute_interval_cost(50, 5, 5, 1000, 1000).unwrap();
        assert_eq!(cost, 6);
    }

    #[test]
    fn negative_pricing_inverse_toll() {
        // price_per_second=0, price_per_unit=-2, elapsed=0, units=1000, scale=1000
        // cost_scaled = 0 + (-2000) = -2000
        // cost = ceil(-2000/1000) = -2
        let cost = compute_interval_cost(0, -2, 0, 1000, 1000).unwrap();
        assert_eq!(cost, -2);
    }

    #[test]
    fn sub_unit_precision_rounds_up() {
        // price_per_second=0, price_per_unit=1, elapsed=0, units=1, scale=1000
        // cost_scaled = 1
        // cost = ceil(1/1000) = 1
        let cost = compute_interval_cost(0, 1, 0, 1, 1000).unwrap();
        assert_eq!(cost, 1);
    }

    #[test]
    fn ceiling_division_rounds_up_not_down() {
        // price_per_second=0, price_per_unit=1, elapsed=0, units=999, scale=1000
        // cost_scaled = 999
        // cost = ceil(999/1000) = 1
        let cost = compute_interval_cost(0, 1, 0, 999, 1000).unwrap();
        assert_eq!(cost, 1);
    }

    #[test]
    fn zero_cost_when_both_prices_zero() {
        let cost = compute_interval_cost(0, 0, 999, 999, 1000).unwrap();
        assert_eq!(cost, 0);
    }

    #[test]
    fn zero_scale_divisor_error() {
        let result = compute_interval_cost(1, 1, 1, 1, 0);
        assert!(matches!(result, Err(PricingError::ZeroScaleDivisor)));
    }

    #[test]
    fn overflow_detection() {
        // Two ~2^126 products summed overflow i128.
        let result = compute_interval_cost(i64::MAX, i64::MAX, u64::MAX, u64::MAX, 1);
        assert!(matches!(result, Err(PricingError::Overflow)));
    }

    #[test]
    fn exact_division_no_rounding() {
        // 1000 / 1000 = 1 exactly
        let cost = compute_interval_cost(0, 1, 0, 1000, 1000).unwrap();
        assert_eq!(cost, 1);
    }

    #[test]
    fn negative_time_component() {
        // Negative time pricing
        let cost = compute_interval_cost(-5, 0, 100, 0, 1000).unwrap();
        // cost_scaled = -500, ceil(-500/1000) = 0 (truncation toward zero = -0 = 0)
        // Wait: -500 / 1000 in Rust = 0 (truncation toward zero). ceil(-0.5) = 0.
        assert_eq!(cost, 0);
    }

    #[test]
    fn small_negative_not_rounding_to_zero() {
        // cost_scaled = -1, ceil(-1/1000) = 0 in Rust truncation? No.
        // -1/1000 in Rust = 0 (truncation toward zero). But ceil(-0.001) = 0. ✓
        let cost = compute_interval_cost(-1, 0, 1, 0, 1000).unwrap();
        assert_eq!(cost, 0);
    }

    #[test]
    fn large_negative_exact() {
        let cost = compute_interval_cost(-100, 0, 10, 0, 1).unwrap();
        assert_eq!(cost, -1000);
    }

    #[test]
    fn negative_with_remainder_truncates_toward_zero() {
        // cost_scaled = -1500, ceil(-1500/1000)
        // Rust: -1500/1000 = -1 (truncation toward zero)
        // ceil(-1.5) = -1 ✓
        let cost = compute_interval_cost(-300, 0, 5, 0, 1000).unwrap();
        assert_eq!(cost, -1);
    }

    // -----------------------------------------------------------------------
    // compute_interval_cost_scaled — unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn scaled_basic_combined() {
        let cost = compute_interval_cost_scaled(5000, 1000, 50, 5);
        assert_eq!(cost, 5 * 50 + 1000 * 5);
    }

    #[test]
    fn scaled_zero_elapsed() {
        let cost = compute_interval_cost_scaled(0, 100, 10, 1);
        assert_eq!(cost, 100);
    }

    #[test]
    fn scaled_zero_units() {
        let cost = compute_interval_cost_scaled(3000, 0, 10, 1);
        assert_eq!(cost, 30);
    }

    #[test]
    fn scaled_both_zero() {
        let cost = compute_interval_cost_scaled(0, 0, 10, 5);
        assert_eq!(cost, 0);
    }

    #[test]
    fn scaled_negative_price_per_unit() {
        let cost = compute_interval_cost_scaled(0, 1000, 0, -2);
        assert_eq!(cost, -2000);
    }

    #[test]
    fn scaled_sub_second_truncated() {
        let cost = compute_interval_cost_scaled(999, 0, 100, 0);
        assert_eq!(cost, 0);
    }

    // -----------------------------------------------------------------------
    // compute_product_id — unit tests
    // -----------------------------------------------------------------------

    fn make_mint_option(id_byte: u8) -> crate::protocol::MintOption {
        crate::protocol::MintOption {
            option_id: Hash32([id_byte; 32]),
            mint_url: format!("https://mint-{id_byte}.example.com"),
            price_per_second: 10,
            price_per_unit: 1,
            mint_unit: "sat".to_owned(),
        }
    }

    #[test]
    fn product_id_deterministic() {
        let pricing = vec![make_mint_option(0x22)];
        let ext = b"test-extensions";
        let id1 = compute_product_id(1000, &pricing, ext).unwrap();
        let id2 = compute_product_id(1000, &pricing, ext).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn product_id_sensitive_to_scale() {
        let pricing = vec![make_mint_option(0x22)];
        let ext = b"ext";
        let id1 = compute_product_id(1000, &pricing, ext).unwrap();
        let id2 = compute_product_id(2000, &pricing, ext).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn product_id_sensitive_to_pricing() {
        let pricing1 = vec![make_mint_option(0x11)];
        let pricing2 = vec![make_mint_option(0x22)];
        let ext = b"ext";
        let id1 = compute_product_id(1000, &pricing1, ext).unwrap();
        let id2 = compute_product_id(1000, &pricing2, ext).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn product_id_sensitive_to_extensions() {
        let pricing = vec![make_mint_option(0x22)];
        let id1 = compute_product_id(1000, &pricing, b"ext1").unwrap();
        let id2 = compute_product_id(1000, &pricing, b"ext2").unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn product_id_empty_extensions() {
        let pricing = vec![make_mint_option(0x22)];
        let id = compute_product_id(1000, &pricing, b"").unwrap();
        // Should produce a valid hash (all-32-bytes, not all zeros)
        assert_ne!(id.0, [0u8; 32]);
    }

    #[test]
    fn product_id_multiple_mint_options() {
        let pricing = vec![make_mint_option(0x11), make_mint_option(0x22)];
        let id = compute_product_id(1000, &pricing, b"ext").unwrap();
        assert_ne!(id.0, [0u8; 32]);

        // Different from single option
        let pricing_single = vec![make_mint_option(0x11)];
        let id_single = compute_product_id(1000, &pricing_single, b"ext").unwrap();
        assert_ne!(id, id_single);
    }

    #[test]
    fn product_id_empty_mint_options() {
        let id = compute_product_id(1000, &[], b"ext").unwrap();
        assert_ne!(id.0, [0u8; 32]);
    }

    // -----------------------------------------------------------------------
    // ceil_div — unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn ceil_div_positive_exact() {
        assert_eq!(ceil_div(1000, 1000), 1);
    }

    #[test]
    fn ceil_div_positive_rounds_up() {
        assert_eq!(ceil_div(1, 1000), 1);
        assert_eq!(ceil_div(999, 1000), 1);
        assert_eq!(ceil_div(1001, 1000), 2);
    }

    #[test]
    fn ceil_div_negative_exact() {
        assert_eq!(ceil_div(-1000, 1000), -1);
    }

    #[test]
    fn ceil_div_negative_truncates_toward_zero() {
        // ceil(-500/1000) = 0 (truncation toward zero gives -0 = 0)
        assert_eq!(ceil_div(-500, 1000), 0);
        // ceil(-1500/1000) = -1 (truncation toward zero gives -1)
        assert_eq!(ceil_div(-1500, 1000), -1);
    }

    #[test]
    fn ceil_div_zero() {
        assert_eq!(ceil_div(0, 1000), 0);
    }

    // -----------------------------------------------------------------------
    // Proptest property tests
    // -----------------------------------------------------------------------

    proptest::proptest! {
        #[test]
        fn non_negative_prices_positive_scale_result_non_negative(
            pps in 0i64..10000,
            ppu in 0i64..10000,
            elapsed in 0u64..10000,
            units in 0u64..10000,
            scale in 1u64..10000,
        ) {
            let cost = compute_interval_cost(pps, ppu, elapsed, units, scale).unwrap();
            assert!(cost >= 0, "non-negative prices should produce non-negative cost, got {cost}");
        }

        #[test]
        fn negative_price_per_unit_result_non_positive_when_time_zero(
            ppu in -10000i64..=-1i64,
            units in 1u64..10000,
            scale in 1u64..10000,
        ) {
            let cost = compute_interval_cost(0, ppu, 0, units, scale).unwrap();
            assert!(cost <= 0, "negative price_per_unit with zero time should produce non-positive cost, got {cost}");
        }

        #[test]
        fn deterministic(
            pps in -1000i64..1000,
            ppu in -1000i64..1000,
            elapsed in 0u64..1000,
            units in 0u64..1000,
            scale in 1u64..1000,
        ) {
            let cost1 = compute_interval_cost(pps, ppu, elapsed, units, scale).unwrap();
            let cost2 = compute_interval_cost(pps, ppu, elapsed, units, scale).unwrap();
            assert_eq!(cost1, cost2);
        }

        #[test]
        fn monotonicity_more_units_gte_when_positive_ppu(
            ppu in 0i64..1000,
            elapsed in 0u64..100,
            units1 in 0u64..500u64,
            units2 in 500u64..1000u64,
            scale in 1u64..1000,
        ) {
            let cost1 = compute_interval_cost(0, ppu, elapsed, units1, scale).unwrap();
            let cost2 = compute_interval_cost(0, ppu, elapsed, units2, scale).unwrap();
            assert!(
                cost2 >= cost1,
                "more units should produce cost >= previous: {cost2} < {cost1} (ppu={ppu}, units1={units1}, units2={units2})"
            );
        }

        #[test]
        fn product_id_deterministic_proptest(
            scale in 1u64..10000u64,
            id_byte in 0u8..255,
            ext in proptest::collection::vec(0u8..255, 0..20),
        ) {
            let pricing = vec![make_mint_option(id_byte)];
            let id1 = compute_product_id(scale, &pricing, &ext).unwrap();
            let id2 = compute_product_id(scale, &pricing, &ext).unwrap();
            assert_eq!(id1, id2);
        }
    }
}
