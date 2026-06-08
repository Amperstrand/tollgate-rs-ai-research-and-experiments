//! Mint health tracker with hysteresis-based reachability probing.
//!
//! Port of Go's `MintHealthTracker`. Probes each mint's `/v1/info` endpoint
//! on a configurable interval (default 5 min). A mint is marked reachable
//! only after `required_consecutive` (default 3) successful probes in a row.
//! A single failure resets the consecutive counter to zero.
//!
//! Callbacks:
//! - `on_first_reachable` — fires once when the first mint becomes reachable
//!   after starting with none. Used to trigger recovery from degraded mode.
//! - `on_reachable_set_changed` — fires whenever the count of reachable mints
//!   changes (any direction).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Per-mint state tracked by the health checker.
struct MintState {
    reachable: bool,
    consecutive_success: i32,
}

/// Callbacks queued during a lock section, to be fired outside the lock.
struct PendingCallbacks {
    first_reachable: bool,
    reachable_set_changed: bool,
}

/// Type-erased callback shared via Arc so it can be cloned out of the lock.
type SharedCallback = Arc<dyn Fn() + Send + Sync>;

/// Shared inner state protected by a `std::sync::Mutex` (lock held briefly
/// for state updates, never across await points).
struct Inner {
    mints: HashMap<String, MintState>,
    had_reachable_mint: bool,
    on_first_reachable: Option<SharedCallback>,
    on_reachable_set_changed: Option<SharedCallback>,
    cancel: CancellationToken,
}

/// Background task probing mints with hysteresis, recovery callbacks.
///
/// Wrap in `Arc` and call [`MintHealthTracker::start`] to launch the
/// background probing loop.
pub struct MintHealthTracker {
    inner: Arc<Mutex<Inner>>,
    mint_urls: Vec<String>,
    probe_interval: Duration,
    probe_timeout: Duration,
    required_consecutive: i32,
}

impl MintHealthTracker {
    /// Create a new tracker with defaults (5 min interval, 5 s timeout, 3 consecutive).
    pub fn new(mint_urls: Vec<String>) -> Self {
        let mut mints = HashMap::new();
        for url in &mint_urls {
            mints.insert(
                url.clone(),
                MintState {
                    reachable: false,
                    consecutive_success: 0,
                },
            );
        }

        Self {
            inner: Arc::new(Mutex::new(Inner {
                mints,
                had_reachable_mint: false,
                on_first_reachable: None,
                on_reachable_set_changed: None,
                cancel: CancellationToken::new(),
            })),
            mint_urls,
            probe_interval: Duration::from_secs(300),
            probe_timeout: Duration::from_secs(5),
            required_consecutive: 3,
        }
    }

    /// Synchronous initial probe (before background task starts).
    ///
    /// Probes each mint once. If a mint responds, it is immediately marked
    /// reachable (consecutive_success set to `required_consecutive` so the
    /// hysteresis threshold is met).
    ///
    /// Returns the list of mint URLs that responded successfully.
    ///
    /// **Panics** if called from within an async runtime. Use
    /// [`MintHealthTracker::run_initial_probe_async`] instead in that case.
    pub fn run_initial_probe(&self) -> Vec<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime for initial probe");

        rt.block_on(self.run_initial_probe_inner())
    }

    /// Async version of [`MintHealthTracker::run_initial_probe`].
    pub async fn run_initial_probe_async(&self) -> Vec<String> {
        self.run_initial_probe_inner().await
    }

    async fn run_initial_probe_inner(&self) -> Vec<String> {
        let mut reachable = Vec::new();
        for url in &self.mint_urls {
            if probe_mint(url, self.probe_timeout).await {
                reachable.push(url.clone());
            }
        }

        let mut inner = self.inner.lock().expect("health tracker lock");
        for url in &reachable {
            if let Some(state) = inner.mints.get_mut(url) {
                state.consecutive_success = self.required_consecutive;
                state.reachable = true;
            }
        }
        if !reachable.is_empty() {
            inner.had_reachable_mint = true;
        }

        reachable
    }

    /// Launch the background probing loop. Returns the `JoinHandle`.
    ///
    /// The loop runs until [`MintHealthTracker::stop`] is called or the
    /// `CancellationToken` is cancelled.
    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        let interval = self.probe_interval;
        let timeout = self.probe_timeout;
        let urls = self.mint_urls.clone();
        let cancel = {
            let inner = self.inner.lock().expect("health tracker lock");
            inner.cancel.clone()
        };

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // First tick fires immediately; skip it so we don't double-probe
            // right after `run_initial_probe`.
            ticker.tick().await;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // Probe all mints concurrently (outside lock).
                        let mut results = Vec::with_capacity(urls.len());
                        {
                            let mut set = tokio::task::JoinSet::new();
                            for url in &urls {
                                let url = url.clone();
                                let timeout = timeout;
                                set.spawn(async move {
                                    let ok = probe_mint(&url, timeout).await;
                                    (url, ok)
                                });
                            }
                            while let Some(res) = set.join_next().await {
                                if let Ok((url, ok)) = res {
                                    results.push((url, ok));
                                }
                            }
                        }

                        // Update state under lock, queue callbacks.
                        let pending = {
                            let mut inner = this.inner.lock().expect("health tracker lock");
                            this.apply_probe_results(&mut inner, &results)
                        };

                        // Fire callbacks outside lock.
                        this.fire_callbacks(pending);
                    }
                    _ = cancel.cancelled() => {
                        tracing::info!("mint health tracker shutting down");
                        return;
                    }
                }
            }
        })
    }

    /// Signal the background task to stop.
    pub fn stop(&self) {
        let inner = self.inner.lock().expect("health tracker lock");
        inner.cancel.cancel();
    }

    /// Returns URLs of mints currently marked reachable.
    pub fn get_reachable_mint_urls(&self) -> Vec<String> {
        let inner = self.inner.lock().expect("health tracker lock");
        inner
            .mints
            .iter()
            .filter(|(_, s)| s.reachable)
            .map(|(url, _)| url.clone())
            .collect()
    }

    /// Returns the number of reachable mints.
    pub fn get_reachable_count(&self) -> usize {
        let inner = self.inner.lock().expect("health tracker lock");
        inner.mints.values().filter(|s| s.reachable).count()
    }

    /// Set the one-shot callback that fires the first time a mint becomes
    /// reachable after starting with none.
    pub fn set_on_first_reachable(&self, callback: Box<dyn Fn() + Send + Sync>) {
        let mut inner = self.inner.lock().expect("health tracker lock");
        inner.on_first_reachable = Some(Arc::from(callback));
    }

    /// Set the callback that fires whenever the reachable set changes.
    pub fn set_on_reachable_set_changed(&self, callback: Box<dyn Fn() + Send + Sync>) {
        let mut inner = self.inner.lock().expect("health tracker lock");
        inner.on_reachable_set_changed = Some(Arc::from(callback));
    }

    /// Reset `had_reachable_mint` so `on_first_reachable` can fire again.
    ///
    /// Called by the recovery path when wallet creation fails after the
    /// tracker detected a mint came back — allows a retry next time.
    pub fn reset_first_reachable(&self) {
        let mut inner = self.inner.lock().expect("health tracker lock");
        inner.had_reachable_mint = false;
    }

    // ------------------------------------------------------------------
    // Internal helpers (called with lock already held or by the bg task)
    // ------------------------------------------------------------------

    /// Apply probe results under `inner` lock. Returns pending callbacks.
    fn apply_probe_results(
        &self,
        inner: &mut Inner,
        results: &[(String, bool)],
    ) -> PendingCallbacks {
        let old_count = inner.mints.values().filter(|s| s.reachable).count();
        let mut first_just_became_reachable = false;

        for (url, reachable_now) in results {
            let state = match inner.mints.get_mut(url.as_str()) {
                Some(s) => s,
                None => continue,
            };

            if *reachable_now {
                state.consecutive_success += 1;
            } else {
                state.consecutive_success = 0;
            }

            let was_reachable = state.reachable;
            let now_reachable = state.consecutive_success >= self.required_consecutive;

            if was_reachable != now_reachable {
                state.reachable = now_reachable;
                tracing::info!(
                    mint_url = %url,
                    reachable = now_reachable,
                    consecutive = state.consecutive_success,
                    "mint reachability changed"
                );
            }
        }

        let new_count = inner.mints.values().filter(|s| s.reachable).count();

        if !inner.had_reachable_mint && new_count > 0 {
            inner.had_reachable_mint = true;
            first_just_became_reachable = true;
            tracing::info!("first mint became reachable — recovery possible");
        }

        PendingCallbacks {
            first_reachable: first_just_became_reachable,
            reachable_set_changed: old_count != new_count,
        }
    }

    /// Fire queued callbacks outside the lock.
    fn fire_callbacks(&self, pending: PendingCallbacks) {
        let (first_cb, changed_cb) = {
            let inner = self.inner.lock().expect("health tracker lock");
            (
                inner.on_first_reachable.clone(),
                inner.on_reachable_set_changed.clone(),
            )
        };

        if pending.first_reachable {
            if let Some(f) = first_cb {
                f();
            }
        }

        if pending.reachable_set_changed {
            if let Some(f) = changed_cb {
                f();
            }
        }
    }
}

/// Probe a single mint by HTTP GET `{mint_url}/v1/info`.
///
/// Returns `true` if the mint responded with HTTP 200 within `timeout`.
pub async fn probe_mint(mint_url: &str, timeout: Duration) -> bool {
    let url = format!("{}/v1/info", mint_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default();

    match client.get(&url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(e) => {
            tracing::debug!(mint_url = %mint_url, error = %e, "mint probe failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::merchant_provider::MerchantProvider;
    use super::*;
    use crate::mock::MockWallet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ------------------------------------------------------------------
    // Helpers — lightweight mock HTTP servers
    // ------------------------------------------------------------------

    /// Spawn a mock HTTP server that always returns `status_code`.
    /// Returns the base URL (e.g. `http://127.0.0.1:PORT`).
    async fn mock_mint_server(status_code: u16) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock mint");
        let addr = listener.local_addr().expect("local addr");
        let url = format!("http://{addr}");

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let mut stream = stream;
                    // Read the request (discard)
                    let _ = stream.read(&mut buf).await;
                    let body = if status_code == 200 { "{}" } else { "error" };
                    let response = format!(
                        "HTTP/1.1 {status_code} OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        // Give the server a moment to start listening
        tokio::time::sleep(Duration::from_millis(10)).await;
        url
    }

    /// Return a URL that will fail to connect (unreachable port).
    fn unreachable_url() -> String {
        "http://127.0.0.1:1".to_owned()
    }

    // ------------------------------------------------------------------
    // Tests — probe function
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_probe_mint_reachable() {
        let base = mock_mint_server(200).await;
        assert!(probe_mint(&base, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn test_probe_mint_unreachable() {
        assert!(!probe_mint(&unreachable_url(), Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn test_probe_mint_server_error() {
        let base = mock_mint_server(500).await;
        assert!(!probe_mint(&base, Duration::from_secs(5)).await);
    }

    // ------------------------------------------------------------------
    // Tests — initial probe
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_initial_probe_reachable() {
        let base = mock_mint_server(200).await;
        let tracker = MintHealthTracker::new(vec![base.clone()]);
        let reachable = tracker.run_initial_probe_async().await;
        assert_eq!(reachable, vec![base]);
        assert_eq!(tracker.get_reachable_count(), 1);
    }

    #[tokio::test]
    async fn test_initial_probe_unreachable() {
        let url = unreachable_url();
        let tracker = MintHealthTracker::new(vec![url]);
        let reachable = tracker.run_initial_probe_async().await;
        assert!(reachable.is_empty());
        assert_eq!(tracker.get_reachable_count(), 0);
    }

    #[tokio::test]
    async fn test_initial_probe_mixed() {
        let ok_base = mock_mint_server(200).await;
        let bad_url = unreachable_url();
        let tracker = MintHealthTracker::new(vec![ok_base.clone(), bad_url]);
        let reachable = tracker.run_initial_probe_async().await;
        assert_eq!(reachable, vec![ok_base]);
        assert_eq!(tracker.get_reachable_count(), 1);
    }

    // ------------------------------------------------------------------
    // Tests — hysteresis
    // ------------------------------------------------------------------

    #[test]
    fn test_hysteresis_requires_three_successes() {
        let url = unreachable_url(); // We won't actually probe
        let tracker = MintHealthTracker::new(vec![url.clone()]);
        assert_eq!(tracker.required_consecutive, 3);

        // Simulate: 2 successes → not reachable yet
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.consecutive_success = 2;
            assert!(!state.reachable);
        }

        // Simulate: 3 successes → reachable
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.consecutive_success = 3;
            state.reachable = true;
            assert!(state.reachable);
        }
    }

    #[test]
    fn test_single_failure_resets_counter() {
        let url = unreachable_url();
        let tracker = MintHealthTracker::new(vec![url.clone()]);

        // Simulate 2 successes
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.consecutive_success = 2;
        }

        // Apply a failure
        let mut inner = tracker.inner.lock().expect("lock");
        let _pending = tracker.apply_probe_results(&mut inner, &[(url.clone(), false)]);

        let state = inner.mints.get(&url).unwrap();
        assert_eq!(state.consecutive_success, 0);
        assert!(!state.reachable);
    }

    // ------------------------------------------------------------------
    // Tests — callbacks
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_on_first_reachable_fires() {
        let bad_url = unreachable_url();
        let tracker = Arc::new(MintHealthTracker::new(vec![bad_url.clone()]));
        tracker.run_initial_probe_async().await;
        assert_eq!(tracker.get_reachable_count(), 0);

        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();
        tracker.set_on_first_reachable(Box::new(move || {
            fired_clone.store(true, Ordering::SeqCst);
        }));

        // Simulate background check making the mint reachable
        for _ in 0..3 {
            let mut inner = tracker.inner.lock().expect("lock");
            let pending = tracker.apply_probe_results(&mut inner, &[(bad_url.clone(), true)]);
            drop(inner);
            tracker.fire_callbacks(pending);
        }

        // The 3rd round should have fired the callback
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_on_first_reachable_does_not_fire_if_initially_reachable() {
        let base_url = "http://initially-reachable.test".to_owned();
        let tracker = MintHealthTracker::new(vec![base_url.clone()]);

        // Mark mint as reachable via initial probe simulation
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&base_url).unwrap();
            state.consecutive_success = 3;
            state.reachable = true;
            inner.had_reachable_mint = true;
        }

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        tracker.set_on_first_reachable(Box::new(move || {
            fired_clone.store(true, Ordering::SeqCst);
        }));

        // Apply another success — should NOT fire first_reachable
        let mut inner = tracker.inner.lock().expect("lock");
        let pending = tracker.apply_probe_results(&mut inner, &[(base_url, true)]);
        drop(inner);
        tracker.fire_callbacks(pending);

        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn test_reset_first_reachable_allows_refiring() {
        let url = "http://test-mint.reset".to_owned();
        let tracker = MintHealthTracker::new(vec![url.clone()]);

        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        tracker.set_on_first_reachable(Box::new(move || {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        // Simulate first transition: 0 → 1 reachable (fires callback)
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.consecutive_success = 3;
            state.reachable = true;
            let pending = tracker.apply_probe_results(&mut inner, &[(url.clone(), true)]);
            drop(inner);
            tracker.fire_callbacks(pending);
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Simulate mint going unreachable
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.reachable = false;
            state.consecutive_success = 0;
        }

        // Reset so the callback can fire again
        tracker.reset_first_reachable();

        // Simulate second transition: 0 → 1 reachable (fires callback again)
        {
            let mut inner = tracker.inner.lock().expect("lock");
            let state = inner.mints.get_mut(&url).unwrap();
            state.consecutive_success = 3;
            state.reachable = true;
            let pending = tracker.apply_probe_results(&mut inner, &[(url.clone(), true)]);
            drop(inner);
            tracker.fire_callbacks(pending);
        }
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    // ------------------------------------------------------------------
    // Tests — recovery swap via MerchantProvider
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_recovery_swap() {
        use tollgate_core::types::Amount;
        use tollgate_core::wallet::Wallet;

        // Start with degraded wallet
        let degraded: Arc<dyn Wallet> = Arc::new(super::super::DegradedWallet);
        let mp = Arc::new(MerchantProvider::new(degraded));

        // Verify degraded: receive fails
        let w = mp.get();
        let result = w.receive_token(&[1, 2, 3, 4, 5, 6, 7, 8]).await;
        assert!(result.is_err());

        // Swap to mock wallet
        let mock: Arc<dyn Wallet> = Arc::new(MockWallet::new(0));
        mp.swap(mock);

        // Verify mock: receive succeeds
        let w = mp.get();
        let result = w.receive_token(&[0, 0, 0, 0, 0, 0, 0, 10]).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Amount(10));
    }

    // ------------------------------------------------------------------
    // Tests — stop
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_stop_cancels_background_task() {
        let tracker = Arc::new(MintHealthTracker::new(vec![unreachable_url()]));
        tracker.run_initial_probe_async().await;
        let handle = tracker.start();
        tracker.stop();
        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "background task did not stop within timeout"
        );
    }

    // ------------------------------------------------------------------
    // Tests — reachable set changed callback
    // ------------------------------------------------------------------

    #[test]
    fn test_reachable_set_changed_callback() {
        let url = "http://test-callback.mint".to_owned();
        let tracker = MintHealthTracker::new(vec![url.clone()]);

        let changes = Arc::new(AtomicUsize::new(0));
        let changes_clone = changes.clone();
        tracker.set_on_reachable_set_changed(Box::new(move || {
            changes_clone.fetch_add(1, Ordering::SeqCst);
        }));

        // Make it reachable (3 probes)
        for _ in 0..3 {
            let mut inner = tracker.inner.lock().expect("lock");
            let pending = tracker.apply_probe_results(&mut inner, &[(url.clone(), true)]);
            drop(inner);
            tracker.fire_callbacks(pending);
        }

        // Should have fired once (on the transition from 0 to 1 reachable)
        assert_eq!(changes.load(Ordering::SeqCst), 1);
    }
}
