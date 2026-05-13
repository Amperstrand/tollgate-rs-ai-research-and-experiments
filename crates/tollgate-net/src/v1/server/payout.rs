#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]

use std::sync::Arc;
use std::time::Duration;

use crate::cdk_wallet::CdkWallet;

/// A payout recipient with proportional share.
pub struct PayoutTarget {
    pub identity: String,
    pub factor: f64,
    pub lightning_address: String,
}

/// Configuration for the background payout task.
pub struct PayoutConfig {
    pub min_balance: u64,
    pub min_payout_amount: u64,
    pub tolerance_percent: u64,
    pub payout_interval: Duration,
    pub targets: Vec<PayoutTarget>,
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
            if amount > 0 {
                Some((i, amount))
            } else {
                None
            }
        })
        .collect()
}

/// Calculate the maximum cost tolerance for a melt operation.
///
/// The Go v1 implementation allows the melt to cost up to
/// `aimed_amount + (aimed_amount * tolerance_percent / 100)`.
/// CDK handles fee reservation internally, but we expose this
/// for informational logging.
pub fn max_melt_cost(aimed_amount: u64, tolerance_percent: u64) -> u64 {
    aimed_amount + (aimed_amount * tolerance_percent / 100)
}

/// Spawn the background payout task.
///
/// Loops: sleep for interval → check balance → if above min_payout_amount,
/// distribute shares to each target's Lightning address via CDK melt.
/// Errors are logged and the loop continues — the task never crashes.
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

    fn make_targets(factors: &[f64]) -> Vec<PayoutTarget> {
        factors
            .iter()
            .enumerate()
            .map(|(i, &f)| PayoutTarget {
                identity: format!("target_{i}"),
                factor: f,
                lightning_address: format!("target{i}@example.com"),
            })
            .collect()
    }

    #[test]
    fn payout_shares_below_min_payout_amount() {
        let targets = make_targets(&[0.8, 0.2]);
        let shares = calculate_payout_shares(50, 10, 100, &targets);
        assert!(
            shares.is_empty(),
            "no payout when balance < min_payout_amount"
        );
    }

    #[test]
    fn payout_shares_below_min_balance() {
        let targets = make_targets(&[1.0]);
        let shares = calculate_payout_shares(100, 200, 50, &targets);
        assert!(
            shares.is_empty(),
            "no payout when balance - min_balance would be negative"
        );
    }

    #[test]
    fn payout_shares_distributed_correctly() {
        let targets = make_targets(&[0.79, 0.21]);
        // balance=500, min_balance=64, min_payout_amount=128
        // spendable = 500 - 64 = 436
        // target_0: 436 * 0.79 = 344.44 → 344
        // target_1: 436 * 0.21 = 91.56 → 92
        let shares = calculate_payout_shares(500, 64, 128, &targets);
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0], (0, 344));
        assert_eq!(shares[1], (1, 92));
    }

    #[test]
    fn payout_shares_single_target() {
        let targets = make_targets(&[1.0]);
        // balance=1000, min_balance=100, min_payout_amount=128
        // spendable = 900
        // target_0: 900 * 1.0 = 900
        let shares = calculate_payout_shares(1000, 100, 128, &targets);
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0], (0, 900));
    }

    #[test]
    fn payout_shares_zero_factor_skipped() {
        let targets = make_targets(&[0.0, 1.0]);
        let shares = calculate_payout_shares(500, 64, 128, &targets);
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0], (1, 436));
    }

    #[test]
    fn max_cost_calculation() {
        assert_eq!(max_melt_cost(100, 10), 110);
        assert_eq!(max_melt_cost(100, 0), 100);
        assert_eq!(max_melt_cost(100, 50), 150);
    }

    #[test]
    fn payout_shares_exact_boundary() {
        let targets = make_targets(&[1.0]);
        // balance=128, min_balance=64, min_payout_amount=128
        // spendable = 64
        let shares = calculate_payout_shares(128, 64, 128, &targets);
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0], (0, 64));
    }
}
