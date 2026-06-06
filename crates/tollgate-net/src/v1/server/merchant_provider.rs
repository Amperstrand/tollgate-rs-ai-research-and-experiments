use std::sync::{Arc, RwLock};
use tollgate_core::wallet::Wallet;

/// Thread-safe wrapper allowing atomic swap of the wallet/merchant.
/// Matches Go's `MerchantProvider` pattern — RWMutex protecting a merchant interface.
/// Swaps are rare (once per mint recovery); reads happen on every HTTP request.
pub struct MerchantProvider {
    wallet: RwLock<Arc<dyn Wallet>>,
}

impl MerchantProvider {
    pub fn new(wallet: Arc<dyn Wallet>) -> Self {
        Self {
            wallet: RwLock::new(wallet),
        }
    }

    /// Returns a clone of the current wallet Arc.
    /// In-flight requests keep their reference alive after swap.
    pub fn get(&self) -> Arc<dyn Wallet> {
        self.wallet
            .read()
            .expect("merchant provider lock not poisoned")
            .clone()
    }

    /// Atomically swaps the wallet. In-flight requests on the old wallet continue uninterrupted.
    pub fn swap(&self, new_wallet: Arc<dyn Wallet>) {
        let mut guard = self
            .wallet
            .write()
            .expect("merchant provider lock not poisoned");
        *guard = new_wallet;
    }
}
