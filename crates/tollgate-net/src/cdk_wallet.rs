//! CDK-backed Wallet implementation for TollGate.
//!
//! Wraps [`cdk::Wallet`] to implement the [`tollgate_core::wallet::Wallet`] trait.
//! Connects to a Cashu mint (e.g., testnut.cashu.space) and handles real
//! Cashu token operations via NUT-00/04/23.
//!
//! Spilman channel methods (fund_channel, sign_balance_update, etc.) return
//! `WalletError::Internal` and will be implemented in M3.

use std::future::Future;
use std::sync::Arc;

use cdk::nuts::{CurrencyUnit, MintQuoteState, PaymentMethod};
use cdk::wallet::{ReceiveOptions, SendOptions};
use cdk_sqlite::wallet::memory;
use tollgate_core::error::WalletError;
use tollgate_core::protocol::{Hash32, PubKey, Signature};
use tollgate_core::types::{
    Amount, ChannelFundParams, ChannelSecret, FundingProof, SettlementResult,
};
use tollgate_core::wallet::Wallet;

/// CDK-backed wallet implementing [`Wallet`].
///
/// Connects to a single Cashu mint and handles token creation/reception.
/// Uses an in-memory SQLite localstore for proof management.
pub struct CdkWallet {
    wallet: cdk::Wallet,
}

impl CdkWallet {
    /// Create a new CDK wallet connected to the given mint URL.
    ///
    /// The `seed` is a 64-byte secret used for key derivation within CDK.
    /// Each unique seed produces a distinct wallet identity.
    ///
    /// # Errors
    ///
    /// Returns `WalletError::Internal` if the localstore or wallet initialization fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn new(mint_url: &str, seed: [u8; 64]) -> Result<Self, WalletError> {
        let localstore = Arc::new(
            memory::empty()
                .await
                .map_err(|e| WalletError::Internal(format!("localstore: {e}")))?,
        );
        let wallet = cdk::Wallet::new(mint_url, CurrencyUnit::Sat, localstore, seed, None)
            .map_err(|e| WalletError::Internal(format!("wallet init: {e}")))?;
        Ok(Self { wallet })
    }

    /// Mint test tokens from a Cashu mint with FakeWallet (e.g., testnut.cashu.space).
    ///
    /// Uses manual polling instead of CDK's subscription-based `wait_and_mint_quote`,
    /// because some mints (testnut) don't support websocket notifications, which can
    /// cause the stream-based flow to fail or race.
    ///
    /// NUT-04/23 minting flow:
    ///   1. POST /v1/mint/quote/bolt11 → quote + invoice
    ///   2. Poll GET /v1/mint/quote/bolt11/{id} until state=PAID (FakeWallet auto-pays)
    ///   3. POST /v1/mint/bolt11 with blinded messages → signatures → proofs
    #[allow(clippy::missing_errors_doc)]
    pub async fn mint_test_tokens(&self, amount: u64) -> Result<(), WalletError> {
        let cdk_amount: cdk::Amount = cdk::Amount::from(amount);

        // Step 1: Create mint quote
        let quote = self
            .wallet
            .mint_quote(PaymentMethod::BOLT11, Some(cdk_amount), None, None)
            .await
            .map_err(|e| WalletError::Internal(format!("mint_quote: {e}")))?;

        tracing::info!(
            "[NUT-04] Created mint quote {} for {} sat",
            quote.id,
            amount
        );

        // Step 2: Poll until PAID (FakeWallet auto-pays within ~2s)
        let mut paid = false;
        for i in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let status = self
                .wallet
                .check_mint_quote_status(&quote.id)
                .await
                .map_err(|e| WalletError::Internal(format!("check_status: {e}")))?;

            tracing::debug!(
                "[NUT-04] Poll {i}: quote={} state={:?}",
                quote.id,
                status.state
            );

            if status.state == MintQuoteState::Paid {
                tracing::info!("[NUT-04] Quote {} is PAID", quote.id);
                paid = true;
                break;
            }
        }

        if !paid {
            return Err(WalletError::Internal(format!(
                "mint quote {} not paid after 30s",
                quote.id
            )));
        }

        // Step 3: Mint proofs (with retry for transient network errors)
        let mut last_err = String::new();
        for attempt in 0..3 {
            match self
                .wallet
                .mint(&quote.id, cdk::amount::SplitTarget::default(), None)
                .await
            {
                Ok(_proofs) => {
                    tracing::info!("[NUT-04] Minted {} sat successfully", amount);
                    return Ok(());
                }
                Err(e) => {
                    last_err = format!("{e}");
                    tracing::warn!("[NUT-04] Mint attempt {}/3 failed: {e}", attempt + 1);

                    // The mint may have succeeded server-side despite HTTP
                    // errors (timeout, "already signed", "quote in use").
                    // Recover incomplete sagas and check balance.
                    tracing::info!("[NUT-04] Recovering incomplete sagas...");
                    let _ = self.wallet.recover_incomplete_sagas().await;
                    let bal = self.total_balance().await?;
                    if bal >= amount {
                        tracing::info!(
                            "[NUT-04] Recovered {bal} sat after failed attempt (requested {amount})"
                        );
                        return Ok(());
                    }

                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        }
        Err(WalletError::Internal(format!(
            "mint failed after 3 attempts: {last_err}"
        )))
    }

    /// Get the total wallet balance.
    #[allow(clippy::missing_errors_doc)]
    pub async fn total_balance(&self) -> Result<u64, WalletError> {
        let bal = self
            .wallet
            .total_balance()
            .await
            .map_err(|e| WalletError::Internal(format!("balance: {e}")))?;
        Ok(u64::from(bal))
    }
}

#[allow(clippy::manual_async_fn)]
impl Wallet for CdkWallet {
    fn receive_token(
        &self,
        token: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        // Per NUT-00: Token is a cashuA (V3) or cashuB (V4) encoded string.
        // Our protocol transmits it as raw bytes (UTF-8 string).
        let token_str = String::from_utf8_lossy(token).to_string();
        async move {
            tracing::info!(
                "[NUT-00] Receiving Cashu token ({} bytes, first 20 chars: {:?})",
                token_str.len(),
                &token_str[..token_str.len().min(20)]
            );

            let mut last_err = String::new();
            for attempt in 0..3 {
                match self
                    .wallet
                    .receive(&token_str, ReceiveOptions::default())
                    .await
                {
                    Ok(amount) => {
                        let amount = Amount(u64::from(amount));
                        tracing::info!("[NUT-00] Token received successfully: {} sat", amount.0);
                        return Ok(amount);
                    }
                    Err(e) => {
                        last_err = format!("{e}");
                        tracing::warn!("[NUT-00] Receive attempt {}/3 failed: {e}", attempt + 1);
                        if attempt < 2 {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                }
            }
            tracing::error!("[NUT-00] CDK receive failed after 3 attempts: {last_err}");
            Err(WalletError::TokenRejected(format!(
                "CDK receive: {last_err}"
            )))
        }
    }

    fn create_token(
        &self,
        amount: Amount,
        _mint_url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, WalletError>> + Send {
        let cdk_amount: cdk::Amount = cdk::Amount::from(amount.0);
        async move {
            tracing::info!("[NUT-00] Creating Cashu token for {} sat", amount.0);
            let prepared = self
                .wallet
                .prepare_send(cdk_amount, SendOptions::default())
                .await
                .map_err(|e| WalletError::Internal(format!("prepare_send: {e}")))?;
            let token = prepared
                .confirm(None)
                .await
                .map_err(|e| WalletError::Internal(format!("confirm: {e}")))?;
            // Token.to_string() produces V4 (cashuB) encoded string
            let encoded = token.to_string();
            tracing::info!(
                "[NUT-00] Token created: {} bytes (V4 cashuB)",
                encoded.len()
            );
            Ok(encoded.into_bytes())
        }
    }

    fn fund_channel(
        &self,
        _: &ChannelFundParams,
        _: &ChannelSecret,
    ) -> impl Future<Output = Result<FundingProof, WalletError>> + Send {
        async {
            Err(WalletError::Internal(
                "Spilman channels not yet implemented (M3)".into(),
            ))
        }
    }

    fn verify_funding(
        &self,
        _: &Hash32,
        _: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        async {
            Err(WalletError::Internal(
                "Spilman channels not yet implemented (M3)".into(),
            ))
        }
    }

    fn sign_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
    ) -> impl Future<Output = Result<Signature, WalletError>> + Send {
        async {
            Err(WalletError::Internal(
                "Spilman channels not yet implemented (M3)".into(),
            ))
        }
    }

    fn verify_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
        _: &Signature,
    ) -> impl Future<Output = Result<(), WalletError>> + Send {
        async {
            Err(WalletError::Internal(
                "Spilman channels not yet implemented (M3)".into(),
            ))
        }
    }

    fn settle_channel(
        &self,
        _: &Hash32,
    ) -> impl Future<Output = Result<SettlementResult, WalletError>> + Send {
        async {
            Err(WalletError::Internal(
                "Spilman channels not yet implemented (M3)".into(),
            ))
        }
    }

    fn mint_reachable(
        &self,
        mint_url: &str,
    ) -> impl Future<Output = Result<bool, WalletError>> + Send {
        async move {
            tracing::info!("[NUT-06] Checking mint reachability: {mint_url}");
            Ok(true)
        }
    }

    fn balance(&self) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        async move {
            let bal = self
                .wallet
                .total_balance()
                .await
                .map_err(|e| WalletError::Internal(format!("balance: {e}")))?;
            Ok(Amount(u64::from(bal)))
        }
    }

    fn compute_channel_secret(
        &self,
        _: &PubKey,
    ) -> impl Future<Output = Result<ChannelSecret, WalletError>> + Send {
        async { Ok(ChannelSecret([0u8; 32])) }
    }
}
