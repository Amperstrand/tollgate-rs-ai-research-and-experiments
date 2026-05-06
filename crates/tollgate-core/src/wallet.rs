//! Wallet trait — abstracts Cashu/Spilman wallet operations.
//!
//! Implementations connect to a Cashu mint and manage Spilman payment channels.
//! The trait uses async methods (via RPITIT) to allow for network calls to the mint.
//!
//! See `docs/design/core/tollgate-payment-channels.md` for the full Wallet specification.

use crate::error::WalletError;
use crate::protocol::{Hash32, PubKey, Signature};
use crate::types::{Amount, ChannelFundParams, ChannelSecret, FundingProof, SettlementResult};

use std::future::Future;

/// Wallet trait — abstracts Cashu/Spilman wallet operations.
///
/// Implementations connect to a Cashu mint and manage Spilman payment channels.
/// The trait is async to allow for network calls to the mint.
///
/// See `docs/design/core/tollgate-payment-channels.md` for the full Wallet specification.
pub trait Wallet: Send + Sync {
    /// Receive and validate a regular Cashu token (for bootstrap payment).
    fn receive_token(
        &self,
        token: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send;

    /// Create a regular Cashu token of the given amount (for paying bootstrap).
    fn create_token(
        &self,
        amount: Amount,
        mint_url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, WalletError>> + Send;

    /// Fund a new Spilman channel.
    fn fund_channel(
        &self,
        params: &ChannelFundParams,
        secret: &ChannelSecret,
    ) -> impl Future<Output = Result<FundingProof, WalletError>> + Send;

    /// Verify that channel funding from the peer is valid.
    fn verify_funding(
        &self,
        channel_id: &Hash32,
        funding_data: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send;

    /// Sign a balance update (debtor side).
    fn sign_balance_update(
        &self,
        channel_id: &Hash32,
        cumulative_balance: Amount,
    ) -> impl Future<Output = Result<Signature, WalletError>> + Send;

    /// Verify a balance update signature (creditor side).
    fn verify_balance_update(
        &self,
        channel_id: &Hash32,
        cumulative_balance: Amount,
        signature: &Signature,
    ) -> impl Future<Output = Result<(), WalletError>> + Send;

    /// Cooperatively settle (close) a channel.
    fn settle_channel(
        &self,
        channel_id: &Hash32,
    ) -> impl Future<Output = Result<SettlementResult, WalletError>> + Send;

    /// Check if a mint is reachable and trusted.
    fn mint_reachable(
        &self,
        mint_url: &str,
    ) -> impl Future<Output = Result<bool, WalletError>> + Send;

    /// Get current wallet balance across all channels.
    fn balance(&self) -> impl Future<Output = Result<Amount, WalletError>> + Send;

    /// Compute the channel secret for a new channel (derived from session params).
    fn compute_channel_secret(
        &self,
        peer_pubkey: &PubKey,
    ) -> impl Future<Output = Result<ChannelSecret, WalletError>> + Send;
}
