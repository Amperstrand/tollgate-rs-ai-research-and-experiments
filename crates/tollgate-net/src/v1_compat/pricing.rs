//! Pricing selection and budget validation for v1 client mode.
//!
//! Mirrors the Go Chandler `selectCompatiblePricingOption` and
//! `ValidateBudgetConstraints` logic.

/// A single pricing option from a Nostr kind 10021 advertisement.
///
/// Carries the Cashu mint URL, unit, per-step price, and minimum step
/// count advertised by an upstream TollGate node.
#[derive(Debug, Clone, PartialEq)]
pub struct PricingOption {
    /// Ecash asset type, e.g. `"cashu"`.
    pub asset_type: String,
    /// Price in the advertised unit (e.g. sats) per step.
    pub price_per_step: u64,
    /// Metered unit name, e.g. `"sat"`, `"ms"`, `"byte"`.
    pub unit: String,
    /// Mint URL that backs this pricing option.
    pub mint_url: String,
    /// Minimum number of steps the upstream requires per purchase.
    pub min_steps: u64,
}

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
    #[error("pubkey blocked: {pubkey}")]
    PubkeyBlocked { pubkey: String },
    #[error("pubkey not in allowlist: {pubkey}")]
    PubkeyNotAllowed { pubkey: String },
    #[error("trust denied: {message}")]
    TrustDenied { message: String },
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
    if steps < min_steps { 0 } else { steps }
}

/// Validate a pubkey against blocklist, allowlist, and default trust policy.
///
/// Evaluation order:
/// 1. Blocklist takes priority — if the pubkey is listed, it is always rejected.
/// 2. If the allowlist is non-empty, the pubkey must appear in it.
/// 3. If the allowlist is empty, the `default_policy` decides:
///    - `"trust_all"` → accepted
///    - `"trust_none"` → rejected
pub fn validate_trust_policy(
    pubkey: &str,
    allowlist: &[String],
    blocklist: &[String],
    default_policy: &str,
) -> Result<(), PricingError> {
    if blocklist.iter().any(|p| p == pubkey) {
        return Err(PricingError::PubkeyBlocked {
            pubkey: pubkey.to_owned(),
        });
    }

    if !allowlist.is_empty() {
        if allowlist.iter().any(|p| p == pubkey) {
            return Ok(());
        }
        return Err(PricingError::PubkeyNotAllowed {
            pubkey: pubkey.to_owned(),
        });
    }

    match default_policy {
        "trust_all" => Ok(()),
        "trust_none" => Err(PricingError::TrustDenied {
            message: format!("pubkey {pubkey} rejected by default trust_none policy"),
        }),
        other => Err(PricingError::TrustDenied {
            message: format!("unknown default policy: {other}"),
        }),
    }
}

/// Select the first compatible pricing option we can afford.
///
/// Filters to options matching our mint URLs and unit, then checks whether
/// our balance covers at least `preferred_allotment / step_size` steps
/// (clamped to each option's `min_steps`).
pub fn select_compatible_with_funds<'a>(
    upstream_options: &'a [PricingOption],
    our_mint_urls: &[String],
    our_unit: &str,
    balance_sats: u64,
    preferred_allotment: u64,
    step_size: u64,
) -> Result<&'a PricingOption, PricingError> {
    let compatible: Vec<&PricingOption> = upstream_options
        .iter()
        .filter(|opt| our_mint_urls.contains(&opt.mint_url) && opt.unit == our_unit)
        .collect();

    if compatible.is_empty() {
        return Err(PricingError::NoCompatibleMint);
    }

    let effective_step_size = if step_size == 0 { 1 } else { step_size };

    for opt in &compatible {
        let preferred_steps = (preferred_allotment / effective_step_size).max(1);
        let steps = preferred_steps.max(opt.min_steps);
        let cost = steps * opt.price_per_step;
        if balance_sats >= cost {
            return Ok(opt);
        }
    }

    let cheapest = compatible
        .into_iter()
        .min_by_key(|opt| opt.price_per_step)
        .expect("compatible is non-empty");
    let preferred_steps = (preferred_allotment / effective_step_size).max(1);
    let steps = preferred_steps.max(cheapest.min_steps);
    let required = steps * cheapest.price_per_step;

    Err(PricingError::InsufficientFunds {
        required,
        available: balance_sats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(mint_url: &str, price_per_step: u64, min_steps: u64) -> PricingOption {
        PricingOption {
            asset_type: "cashu".to_owned(),
            price_per_step,
            unit: "sat".to_owned(),
            mint_url: mint_url.to_owned(),
            min_steps,
        }
    }

    // --- validate_trust_policy tests ---

    #[test]
    fn trust_policy_blocks_blocked_pubkey() {
        let err =
            validate_trust_policy("badkey", &[], &["badkey".to_owned()], "trust_all").unwrap_err();
        assert!(
            matches!(err, PricingError::PubkeyBlocked { ref pubkey } if pubkey == "badkey"),
            "{err:?}"
        );
    }

    #[test]
    fn trust_policy_allows_listed_pubkey() {
        let result = validate_trust_policy("goodkey", &["goodkey".to_owned()], &[], "trust_none");
        assert!(result.is_ok());
    }

    #[test]
    fn trust_policy_rejects_unlisted_pubkey() {
        let err = validate_trust_policy("unknown", &["goodkey".to_owned()], &[], "trust_all")
            .unwrap_err();
        assert!(
            matches!(err, PricingError::PubkeyNotAllowed { ref pubkey } if pubkey == "unknown"),
            "{err:?}"
        );
    }

    #[test]
    fn trust_policy_trust_all_default() {
        assert!(validate_trust_policy("anyone", &[], &[], "trust_all").is_ok());
    }

    #[test]
    fn trust_policy_trust_none_default() {
        let err = validate_trust_policy("anyone", &[], &[], "trust_none").unwrap_err();
        assert!(matches!(err, PricingError::TrustDenied { .. }), "{err:?}");
    }

    #[test]
    fn trust_policy_blocklist_overrides_allowlist() {
        let key = "suspicious";
        let err = validate_trust_policy(key, &[key.to_owned()], &[key.to_owned()], "trust_all")
            .unwrap_err();
        assert!(matches!(err, PricingError::PubkeyBlocked { .. }), "{err:?}");
    }

    #[test]
    fn trust_policy_rejects_unknown_default() {
        let err = validate_trust_policy("anyone", &[], &[], "trust_expired").unwrap_err();
        assert!(
            matches!(err, PricingError::TrustDenied { ref message } if message.contains("trust_expired")),
            "{err:?}"
        );
    }

    // --- select_compatible_with_funds tests ---

    #[test]
    fn select_with_funds_returns_affordable_option() {
        let options = vec![opt("https://mint.a", 1, 1), opt("https://mint.b", 2, 1)];
        let mints = vec!["https://mint.a".to_owned()];

        let result = select_compatible_with_funds(&options, &mints, "sat", 100, 60, 60);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().price_per_step, 1);
    }

    #[test]
    fn select_with_funds_rejects_no_compatible_mint() {
        let options = vec![opt("https://mint.x", 1, 1)];
        let mints = vec!["https://mint.a".to_owned()];

        let err = select_compatible_with_funds(&options, &mints, "sat", 100, 60, 60).unwrap_err();
        assert!(matches!(err, PricingError::NoCompatibleMint), "{err:?}");
    }

    #[test]
    fn select_with_funds_rejects_insufficient_balance() {
        let options = vec![opt("https://mint.a", 10, 5)];
        let mints = vec!["https://mint.a".to_owned()];

        // preferred_allotment=60, step_size=60 → preferred_steps=1, clamped to min_steps=5
        // cost = 5 * 10 = 50. balance=49 < 50
        let err = select_compatible_with_funds(&options, &mints, "sat", 49, 60, 60).unwrap_err();
        assert!(
            matches!(
                err,
                PricingError::InsufficientFunds {
                    required: 50,
                    available: 49
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn select_with_funds_skips_unaffordable_and_returns_first_cheap() {
        let options = vec![opt("https://mint.a", 100, 1), opt("https://mint.a", 1, 1)];
        let mints = vec!["https://mint.a".to_owned()];

        // Both are compatible. First needs 100 sats (1 step × 100), second needs 1 sat.
        // Balance=50 → first is too expensive, second is affordable.
        let result = select_compatible_with_funds(&options, &mints, "sat", 50, 60, 60);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().price_per_step, 1);
    }

    #[test]
    fn select_with_funds_respects_min_steps() {
        let options = vec![opt("https://mint.a", 5, 10)];
        let mints = vec!["https://mint.a".to_owned()];

        // preferred_allotment=60, step_size=60 → preferred_steps=1, clamped to min_steps=10
        // cost = 10 * 5 = 50. balance=49 < 50
        let err = select_compatible_with_funds(&options, &mints, "sat", 49, 60, 60).unwrap_err();
        assert!(
            matches!(err, PricingError::InsufficientFunds { required: 50, .. }),
            "{err:?}"
        );

        // balance=50 should succeed
        let result = select_compatible_with_funds(&options, &mints, "sat", 50, 60, 60);
        assert!(result.is_ok());
    }
}
