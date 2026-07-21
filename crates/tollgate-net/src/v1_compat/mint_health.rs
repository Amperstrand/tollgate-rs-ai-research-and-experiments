//! Mint health tracking — Phase 3 (#56).
//!
//! Tracks which Cashu mints are reachable via HTTP probes and exposes
//! filters used by [`super::merchant::build_advertisement`] to drop
//! unreachable mints from the advertised price sheet, entering
//! "degraded mode" when all mints are unreachable.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use super::merchant::AcceptedMint;

const PROBE_INTERVAL_SECS: u64 = 60;
const DEFAULT_RECOVERY_THRESHOLD: u8 = 3;

pub struct MintHealthTracker {
    reachable: Arc<RwLock<HashMap<String, bool>>>,
    consecutive_successes: Arc<RwLock<HashMap<String, u8>>>,
    recovery_threshold: u8,
    client: reqwest::Client,
}

impl MintHealthTracker {
    pub fn new(mint_urls: &[String]) -> Self {
        let reachable: HashMap<String, bool> = mint_urls
            .iter()
            .map(|u| (u.clone(), true))
            .collect();
        let consecutive_successes = mint_urls
            .iter()
            .map(|u| (u.clone(), 0))
            .collect();
        Self {
            reachable: Arc::new(RwLock::new(reachable)),
            consecutive_successes: Arc::new(RwLock::new(consecutive_successes)),
            recovery_threshold: DEFAULT_RECOVERY_THRESHOLD,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub async fn run_initial_probe(&self) {
        let urls: Vec<String> = self.reachable.read().await.keys().cloned().collect();
        for url in urls {
            let ok = probe_mint(&self.client, &url).await;
            if ok {
                self.mark_reachable(&url).await;
            } else {
                self.mark_unreachable(&url).await;
            }
        }
    }

    pub fn start_proactive_checks(self: &Arc<Self>) {
        let tracker = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(PROBE_INTERVAL_SECS)).await;
                let urls: Vec<String> =
                    tracker.reachable.read().await.keys().cloned().collect();
                for url in urls {
                    let ok = probe_mint(&tracker.client, &url).await;
                    if ok {
                        tracker.mark_reachable(&url).await;
                    } else {
                        tracker.mark_unreachable(&url).await;
                        tracing::warn!(mint = %url, "proactive probe failed; marking unreachable");
                    }
                }
            }
        });
    }

    pub async fn mark_unreachable(&self, mint_url: &str) {
        let mut reachable = self.reachable.write().await;
        let was_reachable = reachable.get(mint_url).copied().unwrap_or(false);
        reachable.insert(mint_url.to_string(), false);
        drop(reachable);
        if was_reachable {
            tracing::warn!(mint = %mint_url, "mint marked unreachable");
        }
        let mut successes = self.consecutive_successes.write().await;
        successes.insert(mint_url.to_string(), 0);
    }

    pub async fn mark_reachable(&self, mint_url: &str) {
        let currently_reachable = {
            let reachable = self.reachable.read().await;
            reachable.get(mint_url).copied().unwrap_or(false)
        };
        if currently_reachable {
            let mut successes = self.consecutive_successes.write().await;
            successes.insert(mint_url.to_string(), 0);
            return;
        }
        let mut successes = self.consecutive_successes.write().await;
        let count = successes.entry(mint_url.to_string()).or_insert(0);
        *count += 1;
        if *count >= self.recovery_threshold {
            let mut reachable = self.reachable.write().await;
            reachable.insert(mint_url.to_string(), true);
            *count = 0;
            drop(reachable);
            tracing::info!(mint = %mint_url, "mint recovered after {} consecutive successes", self.recovery_threshold);
        }
    }

    pub async fn is_reachable(&self, mint_url: &str) -> bool {
        let reachable = self.reachable.read().await;
        reachable.get(mint_url).copied().unwrap_or(false)
    }

    pub async fn get_reachable_mints(&self, all_mints: &[AcceptedMint]) -> Vec<AcceptedMint> {
        let reachable = self.reachable.read().await;
        all_mints
            .iter()
            .filter(|m| reachable.get(&m.url).copied().unwrap_or(false))
            .cloned()
            .collect()
    }

    pub async fn all_unreachable(&self) -> bool {
        let reachable = self.reachable.read().await;
        if reachable.is_empty() {
            return true;
        }
        !reachable.values().any(|v| *v)
    }
}

async fn probe_mint(client: &reqwest::Client, mint_url: &str) -> bool {
    let url = format!("{}/v1/info", mint_url.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mints() -> Vec<String> {
        vec![
            "http://mint-a.example".to_string(),
            "http://mint-b.example".to_string(),
        ]
    }

    #[tokio::test]
    async fn test_new_tracker_all_reachable() {
        let tracker = MintHealthTracker::new(&mints());
        assert!(tracker.is_reachable("http://mint-a.example").await);
        assert!(tracker.is_reachable("http://mint-b.example").await);
        assert!(!tracker.all_unreachable().await);
    }

    #[tokio::test]
    async fn test_mark_unreachable() {
        let tracker = MintHealthTracker::new(&mints());
        tracker
            .mark_unreachable("http://mint-a.example")
            .await;
        assert!(
            !tracker.is_reachable("http://mint-a.example").await,
            "mint-a must be unreachable after mark"
        );
        assert!(
            tracker.is_reachable("http://mint-b.example").await,
            "mint-b must remain reachable"
        );
        assert!(
            !tracker.all_unreachable().await,
            "not all unreachable while mint-b still up"
        );
    }

    #[tokio::test]
    async fn test_all_unreachable() {
        let tracker = MintHealthTracker::new(&mints());
        tracker
            .mark_unreachable("http://mint-a.example")
            .await;
        tracker
            .mark_unreachable("http://mint-b.example")
            .await;
        assert!(
            tracker.all_unreachable().await,
            "all_unreachable must be true when every mint is down"
        );
    }

    #[tokio::test]
    async fn test_get_reachable_mints_filters() {
        let tracker = MintHealthTracker::new(&mints());
        tracker
            .mark_unreachable("http://mint-a.example")
            .await;
        let all = vec![
            AcceptedMint {
                url: "http://mint-a.example".to_string(),
                price_per_step: 1,
                unit: "sat".to_string(),
                min_steps: 1,
            },
            AcceptedMint {
                url: "http://mint-b.example".to_string(),
                price_per_step: 2,
                unit: "sat".to_string(),
                min_steps: 1,
            },
        ];
        let filtered = tracker.get_reachable_mints(&all).await;
        assert_eq!(filtered.len(), 1, "only mint-b should remain");
        assert_eq!(filtered[0].url, "http://mint-b.example");
    }

    #[tokio::test]
    async fn test_recovery_threshold() {
        let tracker = MintHealthTracker::new(&mints());
        tracker
            .mark_unreachable("http://mint-a.example")
            .await;
        assert!(
            !tracker.is_reachable("http://mint-a.example").await,
            "mint must remain unreachable before recovery threshold"
        );
        tracker.mark_reachable("http://mint-a.example").await;
        assert!(
            !tracker.is_reachable("http://mint-a.example").await,
            "still unreachable after 1 success (< threshold 3)"
        );
        tracker.mark_reachable("http://mint-a.example").await;
        assert!(
            !tracker.is_reachable("http://mint-a.example").await,
            "still unreachable after 2 successes (< threshold 3)"
        );
        tracker.mark_reachable("http://mint-a.example").await;
        assert!(
            tracker.is_reachable("http://mint-a.example").await,
            "mint must be reachable after 3 consecutive successes"
        );
        tracker.mark_reachable("http://mint-a.example").await;
        assert!(
            tracker.is_reachable("http://mint-a.example").await,
            "mint must stay reachable on extra success (no-op)"
        );
    }
}
