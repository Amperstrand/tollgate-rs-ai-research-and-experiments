//! Granting access when a Lightning quote is paid.
//!
//! The v1 client polls `GET /ln-invoice` after paying an invoice. When the
//! mint reports the quote PAID, the server mints the proofs, re-serializes
//! them as a bootstrap token, and admits the peer through the Driver — the
//! exact verification path a Cashu `POST /` payment takes (no separate
//! trust assumption). A claim registry guarantees exactly-once granting
//! across concurrent pollers: two overlapping polls cannot double-mint or
//! double-grant.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Mutex;

use super::adapter::V1PaymentResult;
use super::merchant::{self, V1ServerConfig};
use super::wallet::{CdkWallet, QuoteState};

/// Per-quote grant state. `Processing` marks an in-flight claim;
/// `Granted(allotment)` is terminal for the quote's lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantState {
    Processing,
    Granted(u64),
}

/// Exactly-once claim registry for Lightning quote grants.
#[derive(Default)]
pub struct LnGrantRegistry {
    states: Mutex<HashMap<String, GrantState>>,
}

impl LnGrantRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Atomically claim the quote for granting. Returns `true` exactly once
    /// per quote until [`Self::reset`] is called; a concurrent poller (or a
    /// later poll of an already-granted quote) gets `false`.
    pub fn claim(&self, quote_id: &str) -> bool {
        let mut states = self.states.lock().expect("lock not poisoned");
        if states.contains_key(quote_id) {
            return false;
        }
        states.insert(quote_id.to_owned(), GrantState::Processing);
        true
    }

    /// Record the completed grant (terminal).
    pub fn mark_granted(&self, quote_id: &str, allotment: u64) {
        self.states
            .lock()
            .expect("lock not poisoned")
            .insert(quote_id.to_owned(), GrantState::Granted(allotment));
    }

    /// Release a failed claim so a later poll can retry the grant.
    pub fn reset(&self, quote_id: &str) {
        self.states
            .lock()
            .expect("lock not poisoned")
            .remove(quote_id);
    }

    /// Current state of a quote's grant, if any.
    pub fn get(&self, quote_id: &str) -> Option<GrantState> {
        self.states
            .lock()
            .expect("lock not poisoned")
            .get(quote_id)
            .cloned()
    }
}

/// The mint-and-send step of the grant. Seam for offline tests; production
/// is [`CdkWallet`].
///
/// `mint_tokens` must be idempotent: the grant retries quotes the mint
/// already reports ISSUED (a prior attempt minted but failed downstream),
/// so it returns the already-minted amount instead of erroring.
pub trait LnMinter {
    /// Mint the paid quote; returns the minted amount in sats.
    fn mint_tokens(&self, quote_id: &str) -> impl Future<Output = anyhow::Result<u64>> + Send;
    /// Serialize `amount` sats from the wallet as a bootstrap token.
    fn create_token(&self, amount: u64) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send;
}

#[allow(clippy::manual_async_fn)]
impl LnMinter for CdkWallet {
    fn mint_tokens(&self, quote_id: &str) -> impl Future<Output = anyhow::Result<u64>> + Send {
        async move {
            let result = CdkWallet::mint_tokens(self, quote_id).await?;
            Ok(result.amount)
        }
    }

    fn create_token(&self, amount: u64) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
        // Self-referential swap: we mint proofs from a paid quote, then
        // immediately re-serialize them as a token for our own Driver to
        // verify and receive. The Driver's receive path charges the keyset
        // input fee — using include_fee=true HERE would require
        // amount+fee in proofs (which we don't have; we only minted
        // `amount` from the paid quote).
        //
        // SendKind::OfflineExact + include_fee=false produces a token
        // whose outputs sum to exactly `amount` from the proofs we hold.
        // The Driver's receive will deduct the fee, and the customer's
        // allotment is calculated from the nominal amount (the fee is
        // the server's cost of doing business, not the customer's).
        //
        // Found against testnut.cashu.space input_fee_ppk=100
        // (2026-11-15): 10-sat quote → 10 sats minted →
        // create_token(10, include_fee=true) failed with
        // "Insufficient funds".
        async move { CdkWallet::create_token_grant(self, amount).await }
    }
}

/// Admitting a peer with a token. Seam for offline tests; production is
/// the v2 [`crate::driver::Driver`] via
/// [`super::adapter::admit_peer_with_token`].
pub trait PeerAdmitter: Send + Sync {
    fn admit(
        &self,
        peer_hex: &str,
        ip: Option<IpAddr>,
        token: &str,
    ) -> impl Future<Output = V1PaymentResult> + Send;
}

#[allow(clippy::manual_async_fn)]
impl PeerAdmitter for crate::driver::Driver {
    fn admit(
        &self,
        peer_hex: &str,
        ip: Option<IpAddr>,
        token: &str,
    ) -> impl Future<Output = V1PaymentResult> + Send {
        async move { super::adapter::admit_peer_with_token(self, peer_hex, ip, token).await }
    }
}

/// Outcome of a grant attempt for one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOutcome {
    /// Access granted now (this poll performed the grant).
    Issued { allotment: u64 },
    /// Access was granted by an earlier poll.
    AlreadyIssued { allotment: u64 },
    /// Another poller is mid-grant; the client should poll again.
    Processing,
    /// The grant attempt failed; the claim was released for a retry.
    ///
    /// A failure after minting (token serialization, admission) leaves the
    /// quote ISSUED at the mint with the proofs in our wallet; later polls
    /// retry with that state and skip re-minting.
    Failed(String),
}

/// Everything one grant attempt needs, besides the claim registry.
pub struct GrantContext<'a, M: LnMinter, A: PeerAdmitter> {
    pub minter: &'a M,
    pub admitter: &'a A,
    pub peer_hex: &'a str,
    pub ip: Option<IpAddr>,
    pub mint_url: &'a str,
    pub config: &'a V1ServerConfig,
}

/// Attempt to grant access for a paid quote. Exactly-once per quote across
/// concurrent callers: the registry claim decides who mints; everyone else
/// sees [`GrantOutcome::Processing`] or [`GrantOutcome::AlreadyIssued`].
///
/// Both PAID and ISSUED quotes attempt the grant: a prior attempt whose
/// mint succeeded but later steps failed leaves the quote ISSUED with the
/// proofs in the wallet, and treating that as "still processing" would
/// dead-end the quote (found live against testnut 2026-11-16: a failed
/// admission polled `PAID` forever with the claim released but unreachable).
pub async fn grant_if_paid<M: LnMinter, A: PeerAdmitter>(
    registry: &LnGrantRegistry,
    ctx: &GrantContext<'_, M, A>,
    quote_id: &str,
    quote_state: QuoteState,
) -> GrantOutcome {
    if let Some(GrantState::Granted(allotment)) = registry.get(quote_id) {
        return GrantOutcome::AlreadyIssued { allotment };
    }
    if quote_state != QuoteState::Paid && quote_state != QuoteState::Issued {
        return GrantOutcome::Processing;
    }
    if !registry.claim(quote_id) {
        return GrantOutcome::Processing;
    }

    let granted = async {
        let amount = ctx
            .minter
            .mint_tokens(quote_id)
            .await
            .map_err(|e| format!("mint failed: {e}"))?;

        let allotment = merchant::calculate_allotment(amount, ctx.mint_url, ctx.config)
            .map_err(|e| format!("allotment calc failed: {e}"))?;

        let token = ctx
            .minter
            .create_token(amount)
            .await
            .map_err(|e| format!("token serialization failed: {e}"))?;
        let token_str = String::from_utf8_lossy(&token).to_string();

        let result = ctx.admitter.admit(ctx.peer_hex, ctx.ip, &token_str).await;
        if !result.accepted {
            return Err(result
                .reason
                .unwrap_or_else(|| "peer admission rejected".to_owned()));
        }

        Ok(allotment)
    }
    .await;

    match granted {
        Ok(allotment) => {
            registry.mark_granted(quote_id, allotment);
            GrantOutcome::Issued { allotment }
        }
        Err(reason) => {
            tracing::warn!(quote = quote_id, %reason, "Lightning grant failed; claim released");
            registry.reset(quote_id);
            GrantOutcome::Failed(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1_compat::merchant::AcceptedMint;
    use nostr::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockMinter {
        mint_calls: AtomicUsize,
        fail_mint: bool,
    }

    impl LnMinter for MockMinter {
        fn mint_tokens(&self, _quote_id: &str) -> impl Future<Output = anyhow::Result<u64>> + Send {
            self.mint_calls.fetch_add(1, Ordering::SeqCst);
            let fail = self.fail_mint;
            async move {
                if fail {
                    Err(anyhow::anyhow!("mint exploded"))
                } else {
                    Ok(10)
                }
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn create_token(
            &self,
            _amount: u64,
        ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
            async move { Ok(b"cashu-token".to_vec()) }
        }
    }

    struct MockAdmitter {
        accept: bool,
        /// Fail the first N admit calls (simulates a transient post-mint
        /// failure: token verified later once the fault clears).
        fail_first: usize,
        admits: AtomicUsize,
    }

    impl PeerAdmitter for MockAdmitter {
        fn admit(
            &self,
            peer_hex: &str,
            _ip: Option<IpAddr>,
            _token: &str,
        ) -> impl Future<Output = V1PaymentResult> + Send {
            let attempt = self.admits.fetch_add(1, Ordering::SeqCst);
            let accept = self.accept && attempt >= self.fail_first;
            let peer_hex = peer_hex.to_owned();
            async move {
                if accept {
                    V1PaymentResult {
                        accepted: true,
                        reason: None,
                        peer_hex,
                    }
                } else {
                    V1PaymentResult {
                        accepted: false,
                        reason: Some("spent token".to_owned()),
                        peer_hex,
                    }
                }
            }
        }
    }

    fn test_config() -> V1ServerConfig {
        V1ServerConfig {
            metric: "milliseconds".to_owned(),
            step_size: 60_000,
            accepted_mints: vec![AcceptedMint {
                url: "https://mint.example".to_owned(),
                price_per_step: 1,
                unit: "sat".to_owned(),
                min_steps: 1,
            }],
            nostr_keys: Keys::generate(),
            trust_proxy_headers: false,
            wallet: None,
            mint_health: None,
        }
    }

    #[test]
    fn registry_claims_exactly_once_until_reset() {
        let registry = LnGrantRegistry::new();
        assert!(registry.claim("q1"));
        assert!(!registry.claim("q1"), "second claimant is rejected");
        registry.mark_granted("q1", 600);
        assert_eq!(registry.get("q1"), Some(GrantState::Granted(600)));
        assert!(
            !registry.claim("q1"),
            "granted quotes can never be re-claimed"
        );
    }

    #[test]
    fn registry_reset_releases_failed_claim() {
        let registry = LnGrantRegistry::new();
        assert!(registry.claim("q1"));
        registry.reset("q1");
        assert!(registry.claim("q1"), "a failed claim must be retryable");
    }

    #[tokio::test]
    async fn paid_quote_grants_once_and_reports_allotment() {
        let registry = LnGrantRegistry::new();
        let minter = MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: false,
        };
        let admitter = MockAdmitter {
            accept: true,
            fail_first: 0,
            admits: AtomicUsize::new(0),
        };

        let outcome = grant_if_paid(
            &registry,
            &GrantContext {
                minter: &minter,
                admitter: &admitter,
                peer_hex: "aabb",
                ip: None,
                mint_url: "https://mint.example",
                config: &test_config(),
            },
            "q1",
            QuoteState::Paid,
        )
        .await;

        assert_eq!(
            outcome,
            GrantOutcome::Issued { allotment: 600_000 },
            "10 sats at 1 sat/step x 60_000 step = 600_000 ms"
        );
        assert_eq!(minter.mint_calls.load(Ordering::SeqCst), 1);
        assert_eq!(admitter.admits.load(Ordering::SeqCst), 1);

        // Second poll of the same quote: already granted, no re-mint.
        let again = grant_if_paid(
            &registry,
            &GrantContext {
                minter: &minter,
                admitter: &admitter,
                peer_hex: "aabb",
                ip: None,
                mint_url: "https://mint.example",
                config: &test_config(),
            },
            "q1",
            QuoteState::Issued,
        )
        .await;
        assert_eq!(again, GrantOutcome::AlreadyIssued { allotment: 600_000 });
        assert_eq!(
            minter.mint_calls.load(Ordering::SeqCst),
            1,
            "mint exactly once"
        );
    }

    #[tokio::test]
    async fn unpaid_quote_does_not_grant() {
        let registry = LnGrantRegistry::new();
        let minter = MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: false,
        };
        let admitter = MockAdmitter {
            accept: true,
            fail_first: 0,
            admits: AtomicUsize::new(0),
        };

        let outcome = grant_if_paid(
            &registry,
            &GrantContext {
                minter: &minter,
                admitter: &admitter,
                peer_hex: "aabb",
                ip: None,
                mint_url: "https://mint.example",
                config: &test_config(),
            },
            "q1",
            QuoteState::Unpaid,
        )
        .await;

        assert_eq!(outcome, GrantOutcome::Processing);
        assert_eq!(minter.mint_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_grant_releases_claim_for_retry() {
        let registry = LnGrantRegistry::new();
        let minter = MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: true,
        };
        let admitter = MockAdmitter {
            accept: true,
            fail_first: 0,
            admits: AtomicUsize::new(0),
        };

        let outcome = grant_if_paid(
            &registry,
            &GrantContext {
                minter: &minter,
                admitter: &admitter,
                peer_hex: "aabb",
                ip: None,
                mint_url: "https://mint.example",
                config: &test_config(),
            },
            "q1",
            QuoteState::Paid,
        )
        .await;
        assert!(matches!(outcome, GrantOutcome::Failed(_)));

        // After the transient failure, a retry (mint now healthy) succeeds.
        let minter2 = MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: false,
        };
        let outcome2 = grant_if_paid(
            &registry,
            &GrantContext {
                minter: &minter2,
                admitter: &admitter,
                peer_hex: "aabb",
                ip: None,
                mint_url: "https://mint.example",
                config: &test_config(),
            },
            "q1",
            QuoteState::Paid,
        )
        .await;
        assert_eq!(outcome2, GrantOutcome::Issued { allotment: 600_000 });
    }

    #[tokio::test]
    async fn issued_quote_retries_grant_after_post_mint_failure() {
        // Live-found dead-end (testnut, 2026-11-16): a grant whose mint
        // succeeded but admission failed releases the claim "for retry",
        // yet the mint then reports the quote ISSUED — which the Paid-only
        // gate turned into an eternal Processing, stranding the quote.
        let registry = LnGrantRegistry::new();
        let minter = MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: false,
        };
        let admitter = MockAdmitter {
            accept: true,
            fail_first: 1,
            admits: AtomicUsize::new(0),
        };

        let ctx = GrantContext {
            minter: &minter,
            admitter: &admitter,
            peer_hex: "aabb",
            ip: None,
            mint_url: "https://mint.example",
            config: &test_config(),
        };

        let first = grant_if_paid(&registry, &ctx, "q1", QuoteState::Paid).await;
        assert!(matches!(first, GrantOutcome::Failed(_)));

        // The mint now reports ISSUED (tokens were minted by the failed
        // attempt); the retry must still perform the grant.
        let second = grant_if_paid(&registry, &ctx, "q1", QuoteState::Issued).await;
        assert_eq!(
            second,
            GrantOutcome::Issued { allotment: 600_000 },
            "an ISSUED quote must be grantable, not eternally Processing"
        );
        assert_eq!(admitter.admits.load(Ordering::SeqCst), 2);
        assert_eq!(
            registry.get("q1"),
            Some(GrantState::Granted(600_000)),
            "the retry's grant is the terminal registry state"
        );
    }

    #[tokio::test]
    async fn concurrent_pollers_grant_exactly_once() {
        let registry = std::sync::Arc::new(LnGrantRegistry::new());
        let minter = std::sync::Arc::new(MockMinter {
            mint_calls: AtomicUsize::new(0),
            fail_mint: false,
        });
        let admitter = std::sync::Arc::new(MockAdmitter {
            accept: true,
            fail_first: 0,
            admits: AtomicUsize::new(0),
        });
        let config = std::sync::Arc::new(test_config());

        let mut joins = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            let minter = minter.clone();
            let admitter = admitter.clone();
            let config = config.clone();
            joins.push(tokio::spawn(async move {
                grant_if_paid(
                    &registry,
                    &GrantContext {
                        minter: &*minter,
                        admitter: &*admitter,
                        peer_hex: "aabb",
                        ip: None,
                        mint_url: "https://mint.example",
                        config: &config,
                    },
                    "q1",
                    QuoteState::Paid,
                )
                .await
            }));
        }
        let mut results = Vec::new();
        for j in joins {
            results.push(j.await.expect("poller panicked"));
        }

        let issued = results
            .iter()
            .filter(|r| matches!(r, GrantOutcome::Issued { .. }))
            .count();
        let already = results
            .iter()
            .filter(|r| matches!(r, GrantOutcome::AlreadyIssued { .. }))
            .count();
        assert_eq!(
            issued, 1,
            "exactly one of the racing pollers performs the grant"
        );
        assert!(already + issued <= 8);
        assert_eq!(
            minter.mint_calls.load(Ordering::SeqCst),
            1,
            "the quote is minted exactly once"
        );
        assert_eq!(admitter.admits.load(Ordering::SeqCst), 1);
    }
}
