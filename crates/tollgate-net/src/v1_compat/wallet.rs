//! CDK-backed Wallet implementation for TollGate v1-compat.
//!
//! Wraps [`cdk::Wallet`] to provide Cashu token operations via NUT-00/04/05/23.
//! This is a standalone wallet struct — no external trait dependency.
//!
//! Connects to a Cashu mint (e.g., testnut.cashu.space) and handles real
//! Cashu token operations: token receive (NUT-00), mint quote (NUT-04/05),
//! melt, and multi-mint fallback.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use cdk::nuts::{CurrencyUnit, MintQuoteState, PaymentMethod};
use cdk::wallet::{SendKind, SendOptions};
use cdk_sqlite::wallet::memory;

// ---------------------------------------------------------------------------
// Local types (replace removed experimental trait types)
// ---------------------------------------------------------------------------

/// Information about a mint quote (NUT-04).
#[derive(Debug, Clone)]
pub struct MintQuoteInfo {
    /// Quote ID
    pub quote_id: String,
    /// Payment request (BOLT11 invoice)
    pub invoice: String,
    /// Amount in sats
    pub amount: u64,
    /// Expiry timestamp (unix seconds)
    pub expiry: u64,
}

/// State of a mint quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteState {
    /// Quote has not been paid
    Unpaid,
    /// Quote has been paid and wallet can mint
    Paid,
    /// Ecash has been issued for this quote
    Issued,
}

/// Result of minting tokens from a quote.
#[derive(Debug, Clone)]
pub struct MintResult {
    /// Total amount minted (sats)
    pub amount: u64,
}

// ---------------------------------------------------------------------------
// CdkWallet
// ---------------------------------------------------------------------------

/// CDK-backed wallet.
///
/// Connects to a single Cashu mint and handles token creation/reception.
/// Uses an in-memory SQLite localstore for proof management.
pub struct CdkWallet {
    wallet: cdk::Wallet,
    /// HTTP client for lightweight NUT-07 checkstate calls (verify path).
    client: reqwest::Client,
}

impl CdkWallet {
    /// Create a new CDK wallet connected to the given mint URL.
    ///
    /// The `seed` is a 64-byte secret used for key derivation within CDK.
    /// Each unique seed produces a distinct wallet identity.
    ///
    /// # Errors
    ///
    /// Returns an error if the localstore or wallet initialization fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn new(mint_url: &str, seed: [u8; 64]) -> anyhow::Result<Self> {
        let localstore = Arc::new(
            memory::empty()
                .await
                .map_err(|e| anyhow::anyhow!("localstore: {e}"))?,
        );
        let wallet = cdk::Wallet::new(mint_url, CurrencyUnit::Sat, localstore, seed, None)
            .map_err(|e| anyhow::anyhow!("wallet init: {e}"))?;
        Ok(Self {
            wallet,
            client: reqwest::Client::new(),
        })
    }

    /// Try each mint URL in order, returning the first that initializes successfully.
    ///
    /// Mirrors Go's `TollWallet.New()` — loops through `accepted_mints`, creates a
    /// wallet for each, breaks on first success, returns error if all fail.
    ///
    /// # Errors
    ///
    /// Returns an error if all mints fail to connect.
    pub async fn try_mints(mint_urls: &[String], seed: [u8; 64]) -> anyhow::Result<Self> {
        if mint_urls.is_empty() {
            anyhow::bail!("no mint URLs provided");
        }
        let mut last_err = None;
        for url in mint_urls {
            tracing::info!("Trying mint: {url}");
            match Self::new(url, seed).await {
                Ok(wallet) => {
                    tracing::info!("Connected to mint: {url}");
                    return Ok(wallet);
                }
                Err(e) => {
                    tracing::warn!("Mint {url} failed: {e}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all mints failed")))
    }

    // -----------------------------------------------------------------------
    // NUT-00: Token receive / send
    // -----------------------------------------------------------------------

    /// Receive a Cashu token (NUT-00).
    ///
    /// The token is a `cashuA` (V3) or `cashuB` (V4) encoded string,
    /// transmitted as raw bytes (UTF-8 string).
    ///
    /// Returns the verified amount (in sats).
    ///
    /// # Verification strategy
    ///
    /// This does NOT call `cdk::Wallet::receive()`. Instead it mirrors the
    /// production [`BootstrapWallet::verify`](crate::wallet::BootstrapWallet::verify):
    /// parse with `cashu::Token`, validate the mint, sum proof amounts, and
    /// call NUT-07 checkstate at the mint. This avoids issue #52 where CDK
    /// rejects tokens whose keyset IDs the wallet hasn't pre-loaded
    /// ("Short keyset id does not match any of the provided IDv2s" /
    /// "NUT02: ID length invalid").
    ///
    /// For the v1 compat recovery path this is sufficient: the caller
    /// ([`recovery::recover_failed_token`](super::recovery::recover_failed_token))
    /// only needs to know the token is valid and unspent. Long-term storage
    /// of reclaimed proofs is handled at the gateway/driver layer, not here.
    ///
    /// # Errors
    ///
    /// Returns an error if the token is malformed, from a foreign mint,
    /// or any proof is already spent at the mint.
    #[allow(clippy::missing_errors_doc)]
    pub async fn receive_token(&self, token: &[u8]) -> anyhow::Result<u64> {
        let token_str = String::from_utf8_lossy(token).to_string();
        tracing::info!(
            "[NUT-00] Verifying Cashu token ({} bytes, first 20 chars: {:?})",
            token_str.len(),
            &token_str[..token_str.len().min(20)]
        );

        let amount = self.verify_token(&token_str).await?;
        tracing::info!("[NUT-00] Token verified successfully: {} sat", amount);
        Ok(amount)
    }

    /// Lightweight token verification — mirrors
    /// [`BootstrapWallet::verify`](crate::wallet::BootstrapWallet::verify) but
    /// returns sats (the v1_compat contract) instead of milli-units.
    ///
    /// Parses with `cashu::Token` (no CDK keyset loading), enforces that the
    /// token's mint matches this wallet's configured mint (the same safety
    /// check `cdk::Wallet::receive()` performs), sums proof amounts, and calls
    /// NUT-07 checkstate to ensure every proof is UNSPENT.
    async fn verify_token(&self, token_str: &str) -> anyhow::Result<u64> {
        let token = cashu::nuts::Token::from_str(token_str).context("invalid Cashu token")?;

        let token_mint = token.mint_url().context("token has no mint URL")?;
        let token_mint_str = token_mint.to_string();
        let token_mint_base = token_mint_str.trim_end_matches('/');

        let wallet_mint_str = self.wallet.mint_url.to_string();
        let wallet_mint_base = wallet_mint_str.trim_end_matches('/');
        if token_mint_base != wallet_mint_base {
            anyhow::bail!(
                "token mint {token_mint_str} does not match wallet mint {}",
                self.wallet.mint_url
            );
        }

        let amount_sat: u64 = token
            .value()
            .context("could not sum token value")?
            .into();

        let ys = token_proof_ys(&token);
        if ys.is_empty() {
            anyhow::bail!("token contains no proofs");
        }

        self.check_proofs_unspent(token_mint_base, &ys).await?;

        tracing::info!(
            "[NUT-00] Token verified (lightweight): {amount_sat} sat, {} proofs, no CDK receive()",
            ys.len()
        );
        Ok(amount_sat)
    }

    /// NUT-07: POST `/v1/checkstate` and require every returned state to be UNSPENT.
    async fn check_proofs_unspent(
        &self,
        mint_base_url: &str,
        ys: &[String],
    ) -> anyhow::Result<()> {
        let url = format!("{mint_base_url}/v1/checkstate");
        let body = serde_json::json!({ "Ys": ys });

        let resp: serde_json::Value = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("mint check-state request failed")?
            .error_for_status()
            .context("mint returned error")?
            .json()
            .await
            .context("mint response not JSON")?;

        let states = resp["states"]
            .as_array()
            .context("mint response missing 'states'")?;
        for state in states {
            let s = state["state"].as_str().unwrap_or("");
            if s != "UNSPENT" {
                anyhow::bail!("one or more proofs are already spent (state: {s})");
            }
        }
        Ok(())
    }

    /// Create a Cashu token for the given amount (NUT-00).
    ///
    /// Returns the encoded token (V4 cashuB) as bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if token creation fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_token(&self, amount: u64, _mint_url: &str) -> anyhow::Result<Vec<u8>> {
        let cdk_amount: cdk::Amount = cdk::Amount::from(amount);
        tracing::info!("[NUT-00] Creating Cashu token for {} sat", amount);
        let prepared = self
            .wallet
            .prepare_send(cdk_amount, SendOptions::default())
            .await
            .map_err(|e| anyhow::anyhow!("prepare_send: {e}"))?;
        let token = prepared
            .confirm(None)
            .await
            .map_err(|e| anyhow::anyhow!("confirm: {e}"))?;
        // Token.to_string() produces V4 (cashuB) encoded string
        let encoded = token.to_string();
        tracing::info!(
            "[NUT-00] Token created: {} bytes (V4 cashuB)",
            encoded.len()
        );
        Ok(encoded.into_bytes())
    }

    /// Create a Cashu token with overpayment tolerance (NUT-00).
    ///
    /// `max_overpayment_absolute` is the maximum overpayment allowed (in sats).
    /// Returns the encoded token as bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if token creation fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn create_token_with_overpayment(
        &self,
        amount: u64,
        _mint_url: &str,
        max_overpayment_absolute: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let cdk_amount: cdk::Amount = cdk::Amount::from(amount);
        let tolerance: cdk::Amount = cdk::Amount::from(max_overpayment_absolute);
        tracing::info!(
            "[NUT-00] Creating Cashu token for {} sat (overpayment tolerance: {} sat)",
            amount,
            max_overpayment_absolute
        );
        let opts = SendOptions {
            send_kind: SendKind::OnlineTolerance(tolerance),
            include_fee: true,
            ..SendOptions::default()
        };
        let prepared = self
            .wallet
            .prepare_send(cdk_amount, opts)
            .await
            .map_err(|e| anyhow::anyhow!("prepare_send: {e}"))?;
        let token = prepared
            .confirm(None)
            .await
            .map_err(|e| anyhow::anyhow!("confirm: {e}"))?;
        let encoded = token.to_string();
        tracing::info!(
            "[NUT-00] Token created: {} bytes (V4 cashuB)",
            encoded.len()
        );
        Ok(encoded.into_bytes())
    }

    // -----------------------------------------------------------------------
    // Balance
    // -----------------------------------------------------------------------

    /// Get the total wallet balance (in sats).
    ///
    /// # Errors
    ///
    /// Returns an error if the balance query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn total_balance(&self) -> anyhow::Result<u64> {
        let bal = self
            .wallet
            .total_balance()
            .await
            .map_err(|e| anyhow::anyhow!("balance: {e}"))?;
        Ok(u64::from(bal))
    }

    /// Alias for [`total_balance`](Self::total_balance).
    ///
    /// # Errors
    ///
    /// Returns an error if the balance query fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn balance(&self) -> anyhow::Result<u64> {
        self.total_balance().await
    }

    // -----------------------------------------------------------------------
    // NUT-04/23: Minting
    // -----------------------------------------------------------------------

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
    ///
    /// # Errors
    ///
    /// Returns an error if the mint quote is not paid or minting fails after retries.
    #[allow(clippy::missing_errors_doc)]
    pub async fn mint_test_tokens(&self, amount: u64) -> anyhow::Result<()> {
        let cdk_amount: cdk::Amount = cdk::Amount::from(amount);

        // Step 1: Create mint quote
        let quote = self
            .wallet
            .mint_quote(PaymentMethod::BOLT11, Some(cdk_amount), None, None)
            .await
            .map_err(|e| anyhow::anyhow!("mint_quote: {e}"))?;

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
                .map_err(|e| anyhow::anyhow!("check_status: {e}"))?;

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
            return Err(anyhow::anyhow!(
                "mint quote {} not paid after 30s",
                quote.id
            ));
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
        Err(anyhow::anyhow!("mint failed after 3 attempts: {last_err}"))
    }

    /// Request a mint quote from the mint (NUT-04).
    ///
    /// # Errors
    ///
    /// Returns an error if the mint quote request fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn request_mint_quote(
        &self,
        amount: u64,
        _mint_url: &str,
    ) -> anyhow::Result<MintQuoteInfo> {
        let cdk_amount = cdk::Amount::from(amount);
        let quote = self
            .wallet
            .mint_quote(PaymentMethod::BOLT11, Some(cdk_amount), None, None)
            .await
            .map_err(|e| anyhow::anyhow!("mint_quote: {e}"))?;

        Ok(MintQuoteInfo {
            quote_id: quote.id,
            invoice: quote.request,
            amount,
            expiry: quote.expiry,
        })
    }

    /// Check the state of a mint quote (NUT-04).
    ///
    /// # Errors
    ///
    /// Returns an error if the status check fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn check_mint_quote_status(&self, quote_id: &str) -> anyhow::Result<QuoteState> {
        let status = self
            .wallet
            .check_mint_quote_status(quote_id)
            .await
            .map_err(|e| anyhow::anyhow!("check_mint_quote_status: {e}"))?;

        Ok(match status.state {
            MintQuoteState::Unpaid => QuoteState::Unpaid,
            MintQuoteState::Paid => QuoteState::Paid,
            MintQuoteState::Issued => QuoteState::Issued,
        })
    }

    /// Mint tokens from a paid quote (NUT-04).
    ///
    /// # Errors
    ///
    /// Returns an error if minting fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn mint_tokens(&self, quote_id: &str) -> anyhow::Result<MintResult> {
        let proofs = self
            .wallet
            .mint(quote_id, cdk::amount::SplitTarget::default(), None)
            .await
            .map_err(|e| anyhow::anyhow!("mint: {e}"))?;

        let total: u64 = proofs.iter().map(|p| u64::from(p.amount)).sum();
        Ok(MintResult { amount: total })
    }

    // -----------------------------------------------------------------------
    // NUT-05: Melting
    // -----------------------------------------------------------------------

    /// Melt tokens to pay a BOLT11 Lightning invoice.
    ///
    /// Uses the CDK melt flow: quote → prepare → confirm.
    /// Returns the amount paid (in sats).
    ///
    /// # Errors
    ///
    /// Returns an error if the melt fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn melt_to_invoice(&self, invoice: &str) -> anyhow::Result<u64> {
        let quote = self
            .wallet
            .melt_quote(PaymentMethod::BOLT11, invoice.to_owned(), None, None)
            .await
            .map_err(|e| anyhow::anyhow!("melt_quote: {e}"))?;

        tracing::info!("[melt] Created melt quote {} for invoice", quote.id);

        let prepared = self
            .wallet
            .prepare_melt(&quote.id, HashMap::new())
            .await
            .map_err(|e| anyhow::anyhow!("prepare_melt: {e}"))?;

        let confirmed = prepared
            .confirm()
            .await
            .map_err(|e| anyhow::anyhow!("melt confirm: {e}"))?;

        let amount = u64::from(confirmed.amount());
        let fee = u64::from(confirmed.fee_paid());
        tracing::info!("[melt] Melted {amount} sat (fee: {fee} sat)");
        Ok(amount)
    }

    /// Melt tokens to a Lightning address (user@domain.com).
    ///
    /// CDK resolves the LNURL-pay endpoint and creates a melt quote automatically.
    /// `amount_msat` is in millisatoshis.
    /// Returns the amount paid (in sats).
    ///
    /// # Errors
    ///
    /// Returns an error if the melt fails.
    #[allow(clippy::missing_errors_doc)]
    pub async fn melt_to_lightning_address(
        &self,
        address: &str,
        amount_msat: u64,
    ) -> anyhow::Result<u64> {
        let cdk_amount = cdk::Amount::from(amount_msat);

        let quote = self
            .wallet
            .melt_lightning_address_quote(address, cdk_amount)
            .await
            .map_err(|e| anyhow::anyhow!("melt_lightning_address_quote: {e}"))?;

        tracing::info!(
            "[melt] Created melt quote {} for Lightning address {address}",
            quote.id
        );

        let prepared = self
            .wallet
            .prepare_melt(&quote.id, HashMap::new())
            .await
            .map_err(|e| anyhow::anyhow!("prepare_melt: {e}"))?;

        let confirmed = prepared
            .confirm()
            .await
            .map_err(|e| anyhow::anyhow!("melt confirm: {e}"))?;

        let amount = u64::from(confirmed.amount());
        let fee = u64::from(confirmed.fee_paid());
        tracing::info!("[melt] Melted {amount} sat to {address} (fee: {fee} sat)");
        Ok(amount)
    }

    // -----------------------------------------------------------------------
    // Reachability
    // -----------------------------------------------------------------------

    /// Check if a mint URL is reachable (NUT-06).
    ///
    /// # Errors
    ///
    /// Currently always returns `Ok(true)`.
    #[allow(clippy::missing_errors_doc)]
    pub async fn mint_reachable(&self, mint_url: &str) -> anyhow::Result<bool> {
        tracing::info!("[NUT-06] Checking mint reachability: {mint_url}");
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Proofs (requires `spilman` feature — not available in v1-compat)
    // -----------------------------------------------------------------------

    /// Get unspent proofs serialized as JSON.
    ///
    /// Returns proofs in standard Cashu JSON format.
    #[allow(clippy::missing_errors_doc)]
    pub async fn unspent_proofs_json(&self) -> anyhow::Result<String> {
        let proofs = self
            .wallet
            .get_unspent_proofs()
            .await
            .map_err(|e| anyhow::anyhow!("get_proofs: {e}"))?;
        serde_json::to_string(&proofs).map_err(|e| anyhow::anyhow!("serialize proofs: {e}"))
    }
}

/// Extract the `C` (blinded secret pubkey) hex strings from a token's proofs.
/// These are the Y-values the mint uses for NUT-07 state lookup. Handles both
/// V3 (`cashuA`) and V4 (`cashuB`) token encodings.
fn token_proof_ys(token: &cashu::nuts::Token) -> Vec<String> {
    use cashu::nuts::Token;
    match token {
        Token::TokenV3(t) => t
            .token
            .iter()
            .flat_map(|entry| entry.proofs.iter().map(|p| p.c.to_string()))
            .collect(),
        Token::TokenV4(t) => t
            .token
            .iter()
            .flat_map(|entry| entry.proofs.iter().map(|p| p.c.to_string()))
            .collect(),
    }
}

/// A real cashuB (V4) token for unit tests — 1 sat from testnut.cashu.space.
/// Same token used by the production [`BootstrapWallet`](crate::wallet::BootstrapWallet) tests.
#[cfg(test)]
const SAMPLE_TOKEN: &str = "cashuBo2FteBtodHRwczovL3Rlc3RudXQuY2FzaHUuc3BhY2VhdWNzYXRhdIGiYWlIAYhKdLsvxe5hcIGkYWEBYXN4QDk1NTM1NzQ1YjQ2MzM2OGQ1OTVkMGVhMmQ1M2NmMDU0YjZkY2ZhZTY0NjhlOWU0N2U1MDc1YWU3OWRmNmUyODdhY1ghA03QgEalpQeCViTFYVixs-4tTxGmV0Dl-hKTQ8jLyG1ZYWSjYWVYIKlCWsnyOJRBHT_0xffz67uTQUWhk336QvZbnEQW6OUZYXNYIA88wEUIkwoL1RKs6j41AgtMZLp2e3JrlpZyU1o2M3TJYXJYILoalwd76VtIosztMCjHmQzbNUVKCM4VjvV02fSkG19-";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v4_token_and_reads_amount() {
        let token = cashu::nuts::Token::from_str(SAMPLE_TOKEN).expect("valid cashuB token");
        let amount_sat: u64 = token.value().expect("has value").into();
        assert_eq!(amount_sat, 1);
        let mint = token.mint_url().expect("has mint URL").to_string();
        assert!(mint.contains("testnut.cashu.space"));
    }

    #[test]
    fn extracts_proof_y_values_v4() {
        let token = cashu::nuts::Token::from_str(SAMPLE_TOKEN).expect("valid token");
        let ys = token_proof_ys(&token);
        assert_eq!(ys.len(), 1, "sample token has exactly one proof");
        // Y-values are 33-byte compressed pubkeys in hex (66 chars).
        assert_eq!(ys[0].len(), 66);
    }

    #[test]
    fn rejects_invalid_token_string() {
        let result = cashu::nuts::Token::from_str("not-a-token");
        assert!(result.is_err(), "garbage string must fail to parse");
    }
}
