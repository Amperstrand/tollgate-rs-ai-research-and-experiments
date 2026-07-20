//! Pricing and products.
//!
//! v1 is **static** pricing only. Dynamic (formula-driven) pricing — evaluating
//! an expression against opaque link metrics — is deferred; see
//! `docs/design/core/tollgate-pricing.md`.

use alloc::vec::Vec;
use tollgate_protocol::{MintPrice, ProductId, product_id};

/// The rate a node charges a peer, as scaled integers (in the same scale as the
/// peer's balance — milli-units when `pricing_scale` = 1000). Both are signed:
/// positive = the peer pays, zero = free, negative = we pay (not used in
/// bootstrap-only mode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Price {
    pub per_second: i64,
    pub per_unit: i64,
}

impl Price {
    /// Cost (in scaled units) of `elapsed_ms` of time plus `units` delivered.
    /// **Signed**: a negative rate yields a negative cost — the node *pays* the
    /// peer to attract resources (negative pricing in
    /// `docs/design/core/tollgate-pricing.md`), so billing credits the balance
    /// rather than debiting it.
    pub fn cost_scaled(&self, elapsed_ms: u64, units: u64) -> i64 {
        let time = (elapsed_ms as i128) * (self.per_second as i128) / 1000;
        let unit = (units as i128) * (self.per_unit as i128);
        let total = time + unit;
        total
            .try_into()
            .unwrap_or(if total > 0 { i64::MAX } else { i64::MIN })
    }
}

/// A priced offer across one or more mints.
#[derive(Clone, Debug)]
pub struct Product {
    /// Divisor for sub-unit precision (default [`tollgate_protocol::DEFAULT_PRICING_SCALE`]).
    pub pricing_scale: u32,
    /// Per-mint pricing. A peer picks one mint from this list.
    pub prices: Vec<MintPrice>,
    /// Opaque, implementation-defined extension bytes, hashed into the id.
    pub extensions: Vec<u8>,
}

impl Product {
    /// The canonical fingerprint of this product.
    pub fn id(&self) -> ProductId {
        product_id(self.pricing_scale, &self.prices, &self.extensions)
    }
}

/// Pricing bounds for dynamic price adjustment.
#[derive(Clone, Copy, Debug, Default)]
pub struct PriceBounds {
    pub per_second_floor: Option<i64>,
    pub per_second_ceiling: Option<i64>,
    pub per_unit_floor: Option<i64>,
    pub per_unit_ceiling: Option<i64>,
}

impl Price {
    pub fn clamp(self, bounds: &PriceBounds) -> Self {
        Self {
            per_second: clamp_val(
                self.per_second,
                bounds.per_second_floor,
                bounds.per_second_ceiling,
            ),
            per_unit: clamp_val(
                self.per_unit,
                bounds.per_unit_floor,
                bounds.per_unit_ceiling,
            ),
        }
    }

    pub fn apply_multiplier(self, multiplier: f64) -> Self {
        Self {
            per_second: (self.per_second as f64 * multiplier) as i64,
            per_unit: (self.per_unit as f64 * multiplier) as i64,
        }
    }

    pub fn adjust(
        self,
        bounds: &PriceBounds,
        peer_multiplier: Option<f64>,
        active_peer_factor: Option<f64>,
    ) -> Self {
        let mut price = self;
        if let Some(factor) = active_peer_factor {
            let boost = 1.0 + factor;
            price = Self {
                per_second: (price.per_second as f64 * boost) as i64,
                per_unit: (price.per_unit as f64 * boost) as i64,
            };
        }
        if let Some(mult) = peer_multiplier {
            price = price.apply_multiplier(mult);
        }
        price.clamp(bounds)
    }
}

fn clamp_val(val: i64, floor: Option<i64>, ceiling: Option<i64>) -> i64 {
    let v = floor.map_or(val, |f| val.max(f));
    ceiling.map_or(v, |c| v.min(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_combines_time_and_units() {
        let price = Price {
            per_second: 2,
            per_unit: 3,
        };
        // 2 s × 2 + 10 units × 3 = 4 + 30 = 34
        assert_eq!(price.cost_scaled(2000, 10), 34);
    }

    #[test]
    fn cost_is_zero_for_free_price() {
        assert_eq!(Price::default().cost_scaled(10_000, 1_000), 0);
    }

    #[test]
    fn negative_rate_yields_a_signed_credit() {
        // Negative pricing: the node pays the peer, so the cost is negative and
        // billing will *credit* the balance (no clamp to zero).
        let price = Price {
            per_second: 0,
            per_unit: -2,
        };
        assert_eq!(price.cost_scaled(0, 10), -20); // 10 units × −2 = −20
        // Time and units combine with sign: −5/s for 2 s, +3/unit for 10 units.
        let mixed = Price {
            per_second: -5,
            per_unit: 3,
        };
        assert_eq!(mixed.cost_scaled(2000, 10), 20); // −10 + 30
    }

    #[test]
    fn clamp_enforces_floor_and_ceiling() {
        let bounds = PriceBounds {
            per_second_floor: Some(5),
            per_second_ceiling: Some(100),
            per_unit_floor: Some(1),
            per_unit_ceiling: Some(50),
        };
        let price = Price {
            per_second: 200,
            per_unit: 0,
        };
        let clamped = price.clamp(&bounds);
        assert_eq!(clamped.per_second, 100);
        assert_eq!(clamped.per_unit, 1);
    }

    #[test]
    fn apply_multiplier_scales_both_dimensions() {
        let price = Price {
            per_second: 100,
            per_unit: 200,
        };
        let scaled = price.apply_multiplier(0.5);
        assert_eq!(scaled.per_second, 50);
        assert_eq!(scaled.per_unit, 100);
    }

    #[test]
    fn adjust_applies_multiplier_then_clamps() {
        let bounds = PriceBounds {
            per_second_floor: Some(10),
            per_second_ceiling: Some(200),
            per_unit_floor: None,
            per_unit_ceiling: None,
        };
        let price = Price {
            per_second: 100,
            per_unit: 50,
        };
        let adjusted = price.adjust(&bounds, Some(0.2), None);
        assert_eq!(adjusted.per_second, 20);
        assert_eq!(adjusted.per_unit, 10);
    }

    #[test]
    fn adjust_with_active_peer_factor_boosts_price() {
        let price = Price {
            per_second: 100,
            per_unit: 100,
        };
        let adjusted = price.adjust(&PriceBounds::default(), None, Some(0.5));
        assert_eq!(adjusted.per_second, 150);
        assert_eq!(adjusted.per_unit, 150);
    }

    #[test]
    fn clamp_with_no_bounds_returns_original() {
        let price = Price {
            per_second: 42,
            per_unit: -7,
        };
        let clamped = price.clamp(&PriceBounds::default());
        assert_eq!(clamped, price);
    }
}
