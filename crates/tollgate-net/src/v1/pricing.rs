//! Pricing selection and budget validation for v1 client mode.
//!
//! Mirrors the Go Chandler `selectCompatiblePricingOption` and
//! `ValidateBudgetConstraints` logic.

use super::nostr_events::PricingOption;

#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("no compatible pricing found: our mints don't overlap with upstream")]
    NoCompatibleMint,
    #[error("price too high: {price_per_unit:.6} per {unit_name} exceeds max {max_price:.6}")]
    PriceTooHigh {
        price_per_unit: f64,
        unit_name: String,
        max_price: f64,
    },
    #[error("unsupported metric: {0}")]
    UnsupportedMetric(String),
    #[error("insufficient funds: need {required} sats, have {available}")]
    InsufficientFunds { required: u64, available: u64 },
}

/// Find the cheapest compatible pricing option from the upstream advertisement
/// that matches one of our accepted mints.
pub fn select_cheapest_compatible<'a>(
    upstream_options: &'a [PricingOption],
    our_mint_urls: &[String],
    our_unit: &str,
) -> Result<&'a PricingOption, PricingError> {
    let compatible: Vec<&PricingOption> = upstream_options
        .iter()
        .filter(|opt| our_mint_urls.contains(&opt.mint_url) && opt.unit == our_unit)
        .collect();

    if compatible.is_empty() {
        return Err(PricingError::NoCompatibleMint);
    }

    compatible
        .into_iter()
        .min_by_key(|opt| opt.price_per_step)
        .ok_or(PricingError::NoCompatibleMint)
}

/// Validate that the effective price per unit is within budget.
pub fn validate_budget(
    pricing: &PricingOption,
    step_size: u64,
    metric: &str,
    max_price_per_ms: f64,
    max_price_per_byte: f64,
) -> Result<(), PricingError> {
    let price_per_step = pricing.price_per_step as f64;
    let price_per_unit = price_per_step / step_size as f64;

    let (max_price, unit_name) = match metric {
        "milliseconds" => (max_price_per_ms, "millisecond"),
        "bytes" => (max_price_per_byte, "byte"),
        other => return Err(PricingError::UnsupportedMetric(other.into())),
    };

    if price_per_unit > max_price {
        return Err(PricingError::PriceTooHigh {
            price_per_unit,
            unit_name: unit_name.into(),
            max_price,
        });
    }

    Ok(())
}

/// Calculate allotment from steps and step size.
pub fn calculate_allotment(steps: u64, step_size: u64) -> u64 {
    steps * step_size
}

/// Calculate how many steps we can afford with the given balance.
pub fn affordable_steps(balance: u64, price_per_step: u64, min_steps: u64) -> u64 {
    if price_per_step == 0 {
        return 0;
    }
    let steps = balance / price_per_step;
    if steps < min_steps {
        0
    } else {
        steps
    }
}
