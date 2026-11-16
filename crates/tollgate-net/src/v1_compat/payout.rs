//! Background payout task — distributes wallet profits to Lightning addresses.
//!
//! Ported from the m4 branch's v1/server/payout.rs, adapted for the
//! experimental branch's v1_compat CdkWallet. When the wallet balance
//! exceeds `min_payout`, each configured target receives a proportional
//! share via `melt_to_lightning_address` (LNURL-pay → BOLT11).
//!
//! Targets without a Lightning address are skipped (their share stays
//! in the wallet). Errors are logged; the task never crashes.

use std::sync::Arc;
use std::time::Duration;

use super::wallet::CdkWallet;
use crate::config::ProfitShareEntry;

/// A payout recipient resolved from config.
pub struct PayoutTarget {
    pub identity: String,
    pub factor: f64,
    pub lightning_address: String,
}

/// Resolved payout configuration for the background task.
pub struct PayoutConfig {
    pub min_balance: u64,
    pub min_payout_amount: u64,
    pub tolerance_percent: u64,
    pub payout_interval: Duration,
    pub targets: Vec<PayoutTarget>,
}

/// Resolve ProfitShareEntry list from config into PayoutTargets.
/// Entries with no lightning_address are filtered out (their share
/// stays in the wallet — they're logged at spawn time).
pub fn resolve_targets(entries: &[ProfitShareEntry]) -> Vec<PayoutTarget> {
    entries
        .iter()
        .filter_map(|e| {
            let addr = e.lightning_address.clone().unwrap_or_default();
            if addr.is_empty() {
                tracing::info!(
                    "[payout] target '{}' has no lightning_address — share stays in wallet",
                    e.identity,
                );
                None
            } else {
                Some(PayoutTarget {
                    identity: e.identity.clone(),
                    factor: e.factor,
                    lightning_address: addr,
                })
            }
        })
        .collect()
}

/// Calculate the payout amount for each target given the current balance.
///
/// Returns a list of `(target_index, amount_sats)` pairs.
/// Targets whose calculated share rounds to zero are excluded.
pub fn calculate_payout_shares(
    balance: u64,
    min_balance: u64,
    min_payout_amount: u64,
    targets: &[PayoutTarget],
) -> Vec<(usize, u64)> {
    if balance < min_payout_amount {
        return Vec::new();
    }

    let spendable = balance.saturating_sub(min_balance);
    if spendable == 0 {
        return Vec::new();
    }

    targets
        .iter()
        .enumerate()
        .filter_map(|(i, target)| {
            let amount = (spendable as f64 * target.factor).round() as u64;
            if amount > 0 { Some((i, amount)) } else { None }
        })
        .collect()
}

/// Maximum melt cost tolerance (informational; CDK handles fee reserve).
pub fn max_melt_cost(aimed_amount: u64, tolerance_percent: u64) -> u64 {
    aimed_amount + (aimed_amount * tolerance_percent / 100)
}

/// Spawn the background payout task.
///
/// Loops: sleep for interval → check balance → if above min_payout,
/// distribute shares via `melt_to_lightning_address`. Never crashes.
pub fn spawn_payout_task(
    wallet: Arc<CdkWallet>,
    config: PayoutConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(config.payout_interval).await;

            let balance = match wallet.total_balance().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("[payout] failed to get balance: {e}");
                    continue;
                }
            };

            if balance < config.min_payout_amount {
                tracing::debug!(
                    "[payout] balance {balance} below min_payout_amount {}",
                    config.min_payout_amount
                );
                continue;
            }

            let shares = calculate_payout_shares(
                balance,
                config.min_balance,
                config.min_payout_amount,
                &config.targets,
            );

            if shares.is_empty() {
                continue;
            }

            tracing::info!(
                "[payout] balance={balance}, distributing {} shares",
                shares.len()
            );

            for (idx, amount_sats) in &shares {
                let target = &config.targets[*idx];
                let amount_msat = amount_sats * 1000;
                let max_cost = max_melt_cost(*amount_sats, config.tolerance_percent);

                tracing::info!(
                    "[payout] sending {amount_sats} sat (max_cost={max_cost}) to {} ({})",
                    target.identity,
                    target.lightning_address,
                );

                match wallet
                    .melt_to_lightning_address(&target.lightning_address, amount_msat)
                    .await
                {
                    Ok(paid) => {
                        tracing::info!(
                            "[payout] paid {paid} sat to {} ({})",
                            target.identity,
                            target.lightning_address,
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[payout] failed to send to {} ({}): {e}",
                            target.identity,
                            target.lightning_address,
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(identity: &str, factor: f64, addr: &str) -> PayoutTarget {
        PayoutTarget {
            identity: identity.to_owned(),
            factor,
            lightning_address: addr.to_owned(),
        }
    }

    #[test]
    fn shares_are_proportional_and_exclude_zero_rounds() {
        let targets = vec![
            target("owner", 0.7, "owner@wallet.example"),
            target("dev", 0.25, "dev@wallet.example"),
            target("tiny", 0.05, "tiny@wallet.example"),
        ];
        // 1000 balance, 100 min_balance → 900 spendable
        let shares = calculate_payout_shares(1000, 100, 500, &targets);
        assert_eq!(shares.len(), 3);
        assert_eq!(shares[0], (0, 630)); // 900 * 0.7
        assert_eq!(shares[1], (1, 225)); // 900 * 0.25
        assert_eq!(shares[2], (2, 45)); // 900 * 0.05
    }

    #[test]
    fn below_min_payout_no_shares() {
        let targets = vec![target("a", 1.0, "a@x.example")];
        assert!(calculate_payout_shares(100, 50, 500, &targets).is_empty());
    }

    #[test]
    fn zero_round_share_excluded() {
        let targets = vec![
            target("big", 0.99, "big@x.example"),
            target("small", 0.01, "small@x.example"),
        ];
        // 100 spendable, 0.01 share rounds to 1 → included
        // 10 spendable, 0.01 share rounds to 0 → excluded
        let shares = calculate_payout_shares(150, 50, 100, &targets);
        assert_eq!(shares.len(), 2);
        let shares2 = calculate_payout_shares(60, 50, 50, &targets);
        assert_eq!(shares2.len(), 1); // only "big" gets a share
    }

    #[test]
    fn max_melt_cost_is_additive() {
        assert_eq!(max_melt_cost(100, 10), 110);
        assert_eq!(max_melt_cost(100, 0), 100);
    }
}
