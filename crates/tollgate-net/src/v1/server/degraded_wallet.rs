//! Degraded-mode wallet returned when all mints are unreachable.
//!
//! Port of Go's `MerchantDegraded`. Every payment operation fails with a
//! descriptive error so that HTTP handlers return appropriate error responses.
//! `balance()` returns `Ok(0)` to avoid crashing balance-dependent paths.
//! `mint_reachable()` returns `Ok(false)`.

use std::future::Future;
use std::pin::Pin;

use tollgate_core::error::WalletError;
use tollgate_core::types::Amount;
use tollgate_core::wallet::Wallet;

const DEGRADED_MSG: &str = "service degraded: wallet unavailable, mints unreachable";

/// A `Wallet` impl that represents degraded state — all mints unreachable.
///
/// Payment operations return errors. Balance returns zero. Mint reachability
/// returns false. This matches Go's `MerchantDegraded` behaviour so that
/// the server can start and serve non-payment endpoints (advertisement,
/// whoami) even when mints are down, then upgrade to a real wallet once
/// the health tracker detects recovery.
pub struct DegradedWallet;

#[allow(clippy::manual_async_fn)]
impl Wallet for DegradedWallet {
    fn receive_token(
        &self,
        _token: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
        Box::pin(async {
            Err(WalletError::TokenRejected(DEGRADED_MSG.to_owned()))
        })
    }

    fn create_token(
        &self,
        _amount: Amount,
        _mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WalletError>> + Send + '_>> {
        Box::pin(async {
            Err(WalletError::Internal(DEGRADED_MSG.to_owned()))
        })
    }

    fn mint_reachable(
        &self,
        _mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, WalletError>> + Send + '_>> {
        Box::pin(async { Ok(false) })
    }

    fn balance(&self) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
        Box::pin(async {
            tracing::warn!("balance called in degraded mode — wallet unavailable, returning 0");
            Ok(Amount(0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_degraded_wallet_receive_rejects() {
        let w = DegradedWallet;
        let result = w.receive_token(&[1, 2, 3, 4, 5, 6, 7, 8]).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("service degraded"),
            "expected degraded message, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_degraded_wallet_create_rejects() {
        let w = DegradedWallet;
        let result = w.create_token(Amount(100), "https://example.com").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("service degraded"),
            "expected degraded message, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_degraded_wallet_mint_unreachable() {
        let w = DegradedWallet;
        let result = w.mint_reachable("https://example.com").await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_degraded_wallet_balance_zero() {
        let w = DegradedWallet;
        let result = w.balance().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Amount(0));
    }
}
