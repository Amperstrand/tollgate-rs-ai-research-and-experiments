//! Wallet trait — abstracts Cashu wallet operations for bootstrap token payments.
//!
//! Implementations connect to a Cashu mint and handle token creation/reception.
//! Spilman payment channel operations are handled separately by `SpilmanService`
//! in `tollgate-net`, which wraps cdk-spilman's bridge API directly.
//!
//! See `docs/design/core/tollgate-bootstrap.md` for bootstrap token specification.
//! See `docs/design/core/tollgate-payment-channels.md` for Spilman channel design.

use crate::error::WalletError;
use crate::types::Amount;

use std::future::Future;
use std::pin::Pin;

/// Wallet trait — abstracts Cashu wallet operations for bootstrap token payments.
///
/// This trait covers only stateless bootstrap token operations (receive, create,
/// balance, mint reachability). Spilman payment channel operations — which are
/// stateful, involve async networking to the mint, and have fundamentally different
/// buyer/seller roles — are handled by `SpilmanService` in `tollgate-net`.
///
/// Splitting these concerns keeps `tollgate-core` dependency-free and accurately
/// reflects the architectural boundary: bootstrap tokens are simple Cashu transfers,
/// while Spilman channels are stateful multi-step protocols with dedicated bridge
/// infrastructure in cdk-spilman.
pub trait Wallet: Send + Sync {
    /// Receive and validate a Cashu token (for bootstrap payment).
    fn receive_token(
        &self,
        token: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>>;

    /// Create a Cashu token of the given amount (for paying bootstrap).
    fn create_token(
        &self,
        amount: Amount,
        mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WalletError>> + Send + '_>>;

    /// Check if a mint is reachable and trusted.
    fn mint_reachable(
        &self,
        mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, WalletError>> + Send + '_>>;

    /// Get current wallet balance.
    fn balance(&self) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>>;
}
