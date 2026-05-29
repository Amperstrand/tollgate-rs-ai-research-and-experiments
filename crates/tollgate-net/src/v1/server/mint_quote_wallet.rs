//! Lightning mint quote wallet operations for NUT-04 flow.
//!
//! This trait is separate from the `Wallet` trait in `tollgate-core` because
//! mint quote operations are CDK-specific and involve stateful quote tracking.
//! The `Wallet` trait handles stateless bootstrap token operations.

use std::collections::HashMap;
use std::fmt;
use tokio::sync::Mutex as TokioMutex;

// Types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteState {
    Unpaid,
    Paid,
    Issued,
}

impl fmt::Display for QuoteState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unpaid => write!(f, "UNPAID"),
            Self::Paid => write!(f, "PAID"),
            Self::Issued => write!(f, "ISSUED"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MintQuoteInfo {
    pub quote_id: String,
    pub invoice: String, // BOLT11 invoice string (lnbc...)
    pub amount: u64,
    pub expiry: u64, // unix timestamp
}

#[derive(Debug, Clone, PartialEq)]
pub struct MintResult {
    pub amount: u64, // amount actually minted (in sats)
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MintQuoteError {
    #[error("quote not found: {0}")]
    NotFound(String),
    #[error("mint error: {0}")]
    Mint(String),
    #[error("quote expired: {0}")]
    Expired(String),
    #[error("{0}")]
    Other(String),
}

#[async_trait::async_trait]
pub trait MintQuoteWallet: Send + Sync {
    async fn request_mint_quote(
        &self,
        amount: u64,
        mint_url: &str,
    ) -> Result<MintQuoteInfo, MintQuoteError>;
    async fn check_mint_quote_status(&self, quote_id: &str) -> Result<QuoteState, MintQuoteError>;
    async fn mint_tokens(&self, quote_id: &str) -> Result<MintResult, MintQuoteError>;
}

#[derive(Clone)]
#[allow(dead_code)]
struct MockQuote {
    quote_id: String,
    invoice: String,
    amount: u64,
    expiry: u64,
    state: QuoteState,
}

pub struct MockMintQuoteWallet {
    quotes: TokioMutex<HashMap<String, MockQuote>>,
    counter: TokioMutex<usize>,
}

impl MockMintQuoteWallet {
    pub fn new() -> Self {
        Self {
            quotes: TokioMutex::new(HashMap::new()),
            counter: TokioMutex::new(0),
        }
    }

    pub async fn set_quote_state(&self, quote_id: &str, state: QuoteState) {
        let mut quotes = self.quotes.lock().await;
        if let Some(quote) = quotes.get_mut(quote_id) {
            quote.state = state;
        }
    }
}

impl Default for MockMintQuoteWallet {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MintQuoteWallet for MockMintQuoteWallet {
    async fn request_mint_quote(
        &self,
        amount: u64,
        _mint_url: &str,
    ) -> Result<MintQuoteInfo, MintQuoteError> {
        let mut counter = self.counter.lock().await;
        let id = *counter;
        *counter += 1;
        drop(counter);

        let quote_id = format!("mock-quote-{id}");

        let quote = MockQuote {
            quote_id: quote_id.clone(),
            invoice: format!("lnbc{amount}mock{id}"),
            amount,
            expiry: 1_700_000_000 + 3600,
            state: QuoteState::Unpaid,
        };

        let mut quotes = self.quotes.lock().await;
        quotes.insert(quote_id.clone(), quote);

        Ok(MintQuoteInfo {
            quote_id,
            invoice: format!("lnbc{amount}mock{id}"),
            amount,
            expiry: 1_700_000_000 + 3600,
        })
    }

    async fn check_mint_quote_status(&self, quote_id: &str) -> Result<QuoteState, MintQuoteError> {
        let quotes = self.quotes.lock().await;
        quotes
            .get(quote_id)
            .map(|q| q.state)
            .ok_or_else(|| MintQuoteError::NotFound(quote_id.to_owned()))
    }

    async fn mint_tokens(&self, quote_id: &str) -> Result<MintResult, MintQuoteError> {
        let mut quotes = self.quotes.lock().await;
        let quote = quotes
            .get_mut(quote_id)
            .ok_or_else(|| MintQuoteError::NotFound(quote_id.to_owned()))?;

        match quote.state {
            QuoteState::Paid => {
                quote.state = QuoteState::Issued;
                Ok(MintResult {
                    amount: quote.amount,
                })
            }
            QuoteState::Issued => Ok(MintResult {
                amount: quote.amount,
            }),
            QuoteState::Unpaid => Err(MintQuoteError::Mint(
                "quote state is Unpaid, expected Paid".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_request_returns_unpaid() {
        let wallet = MockMintQuoteWallet::new();
        let info = wallet
            .request_mint_quote(100, "https://example.com/mint")
            .await
            .unwrap();

        let state = wallet
            .check_mint_quote_status(&info.quote_id)
            .await
            .unwrap();
        assert_eq!(state, QuoteState::Unpaid);
    }

    #[tokio::test]
    async fn mock_check_unknown_returns_not_found() {
        let wallet = MockMintQuoteWallet::new();
        let result = wallet
            .check_mint_quote_status("nonexistent-quote")
            .await
            .unwrap_err();
        assert_eq!(
            result,
            MintQuoteError::NotFound("nonexistent-quote".to_owned())
        );
    }

    #[tokio::test]
    async fn mock_set_paid_then_check() {
        let wallet = MockMintQuoteWallet::new();
        let info = wallet
            .request_mint_quote(50, "https://example.com/mint")
            .await
            .unwrap();

        wallet
            .set_quote_state(&info.quote_id, QuoteState::Paid)
            .await;

        let state = wallet
            .check_mint_quote_status(&info.quote_id)
            .await
            .unwrap();
        assert_eq!(state, QuoteState::Paid);
    }

    #[tokio::test]
    async fn mock_mint_transitions_to_issued() {
        let wallet = MockMintQuoteWallet::new();
        let info = wallet
            .request_mint_quote(75, "https://example.com/mint")
            .await
            .unwrap();

        wallet
            .set_quote_state(&info.quote_id, QuoteState::Paid)
            .await;
        let result = wallet.mint_tokens(&info.quote_id).await.unwrap();

        let state = wallet
            .check_mint_quote_status(&info.quote_id)
            .await
            .unwrap();
        assert_eq!(state, QuoteState::Issued);
        assert_eq!(result.amount, 75);
    }

    #[tokio::test]
    async fn mock_mint_returns_correct_amount() {
        let wallet = MockMintQuoteWallet::new();
        let info = wallet
            .request_mint_quote(123, "https://example.com/mint")
            .await
            .unwrap();

        wallet
            .set_quote_state(&info.quote_id, QuoteState::Paid)
            .await;
        let result = wallet.mint_tokens(&info.quote_id).await.unwrap();

        assert_eq!(result.amount, 123);
    }

    #[tokio::test]
    async fn mock_mint_unpaid_fails() {
        let wallet = MockMintQuoteWallet::new();
        let info = wallet
            .request_mint_quote(200, "https://example.com/mint")
            .await
            .unwrap();

        let result = wallet.mint_tokens(&info.quote_id).await.unwrap_err();
        assert!(matches!(result, MintQuoteError::Mint(_)));
    }
}
