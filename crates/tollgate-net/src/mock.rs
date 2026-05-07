use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use tollgate_core::access::AccessLevel;
use tollgate_core::adapter::ResourceAdapter;
use tollgate_core::error::{AdapterError, WalletError};
use tollgate_core::metering::PeerMetrics;
use tollgate_core::protocol::{Hash32, PubKey, Signature};
use tollgate_core::types::{
    Amount, ChannelFundParams, ChannelSecret, FundingProof, SettlementResult,
};
use tollgate_core::wallet::Wallet;

pub struct MockWallet {
    balance: Mutex<u64>,
}

impl MockWallet {
    pub fn new(initial_balance: u64) -> Self {
        Self {
            balance: Mutex::new(initial_balance),
        }
    }
}

#[allow(clippy::manual_async_fn)]
impl Wallet for MockWallet {
    fn receive_token(
        &self,
        token: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        let result = if token.len() < 8 {
            Err(WalletError::TokenRejected("token too short".to_owned()))
        } else {
            let amount = u64::from_be_bytes(token[..8].try_into().expect("checked length above"));
            if amount == 0 {
                Err(WalletError::TokenRejected("zero amount".to_owned()))
            } else {
                let mut balance = self.balance.lock().expect("lock not poisoned");
                *balance += amount;
                Ok(Amount(amount))
            }
        };
        async move { result }
    }

    fn create_token(
        &self,
        amount: Amount,
        _mint_url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, WalletError>> + Send {
        let result = {
            let mut balance = self.balance.lock().expect("lock not poisoned");
            if *balance < amount.0 {
                Err(WalletError::Internal("insufficient balance".to_owned()))
            } else {
                *balance -= amount.0;
                Ok(amount.0.to_be_bytes().to_vec())
            }
        };
        async move { result }
    }

    fn fund_channel(
        &self,
        _: &ChannelFundParams,
        _: &ChannelSecret,
    ) -> impl Future<Output = Result<FundingProof, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn verify_funding(
        &self,
        _: &Hash32,
        _: &[u8],
    ) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn sign_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
    ) -> impl Future<Output = Result<Signature, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn verify_balance_update(
        &self,
        _: &Hash32,
        _: Amount,
        _: &Signature,
    ) -> impl Future<Output = Result<(), WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn settle_channel(
        &self,
        _: &Hash32,
    ) -> impl Future<Output = Result<SettlementResult, WalletError>> + Send {
        async { Err(WalletError::Internal("not implemented".to_owned())) }
    }

    fn mint_reachable(&self, _: &str) -> impl Future<Output = Result<bool, WalletError>> + Send {
        async { Ok(true) }
    }

    fn balance(&self) -> impl Future<Output = Result<Amount, WalletError>> + Send {
        let amount = *self.balance.lock().expect("lock not poisoned");
        async move { Ok(Amount(amount)) }
    }

    fn compute_channel_secret(
        &self,
        _: &PubKey,
    ) -> impl Future<Output = Result<ChannelSecret, WalletError>> + Send {
        async { Ok(ChannelSecret([0u8; 32])) }
    }
}

pub struct MockAdapter {
    access_levels: Mutex<HashMap<Vec<u8>, AccessLevel>>,
    metrics: Mutex<PeerMetrics>,
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAdapter {
    pub fn new() -> Self {
        Self {
            access_levels: Mutex::new(HashMap::new()),
            metrics: Mutex::new(PeerMetrics::zero()),
        }
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn set_metrics(&self, m: PeerMetrics) {
        *self.metrics.lock().expect("lock not poisoned") = m;
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn get_access_level(&self, peer_id: &[u8]) -> Option<AccessLevel> {
        self.access_levels
            .lock()
            .expect("lock not poisoned")
            .get(peer_id)
            .copied()
    }
}

impl ResourceAdapter for MockAdapter {
    fn set_peer_access(
        &self,
        peer_id: &[u8],
        level: AccessLevel,
    ) -> impl Future<Output = Result<(), AdapterError>> + Send {
        let peer_id = peer_id.to_owned();
        async move {
            tracing::info!(?level, "Access level changed for peer");
            self.access_levels
                .lock()
                .expect("lock not poisoned")
                .insert(peer_id, level);
            Ok(())
        }
    }

    fn peer_metrics(
        &self,
        _: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send {
        let metrics = self.metrics.lock().expect("lock not poisoned").clone();
        async move { Ok(metrics) }
    }

    fn subscribe_meter(
        &self,
        peer_id: &[u8],
    ) -> impl Future<Output = Result<PeerMetrics, AdapterError>> + Send {
        self.peer_metrics(peer_id)
    }
}
