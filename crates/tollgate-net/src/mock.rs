use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use tollgate_core::access::AccessLevel;
use tollgate_core::adapter::ResourceAdapter;
use tollgate_core::error::{AdapterError, WalletError};
use tollgate_core::metering::PeerMetrics;
use tollgate_core::types::Amount;
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
    ) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
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
        Box::pin(async move { result })
    }

    fn create_token(
        &self,
        amount: Amount,
        _mint_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WalletError>> + Send + '_>> {
        let result = {
            let mut balance = self.balance.lock().expect("lock not poisoned");
            if *balance < amount.0 {
                Err(WalletError::Internal("insufficient balance".to_owned()))
            } else {
                *balance -= amount.0;
                Ok(amount.0.to_be_bytes().to_vec())
            }
        };
        Box::pin(async move { result })
    }

    fn mint_reachable(&self, _: &str) -> Pin<Box<dyn Future<Output = Result<bool, WalletError>> + Send + '_>> {
        Box::pin(async { Ok(true) })
    }

    fn balance(&self) -> Pin<Box<dyn Future<Output = Result<Amount, WalletError>> + Send + '_>> {
        let amount = *self.balance.lock().expect("lock not poisoned");
        Box::pin(async move { Ok(Amount(amount)) })
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
