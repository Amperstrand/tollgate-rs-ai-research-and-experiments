use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::merchant::AcceptedMint;

const PROBE_INTERVAL_SECS: u64 = 60;
const DEFAULT_RECOVERY_THRESHOLD: u8 = 3;

pub struct MintHealthTracker {
    reachable: RwLock<HashMap<String, bool>>,
    consecutive_successes: RwLock<HashMap<String, u8>>,
    recovery_threshold: u8,
    client: reqwest::Client,
}

impl MintHealthTracker {
    pub fn new(mint_urls: &[String]) -> Self {
        Self {
            reachable: RwLock::new(
                mint_urls.iter().map(|u| (u.clone(), true)).collect(),
            ),
            consecutive_successes: RwLock::new(
                mint_urls.iter().map(|u| (u.clone(), 0)).collect(),
            ),
            recovery_threshold: DEFAULT_RECOVERY_THRESHOLD,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub async fn run_initial_probe(&self) {
        let urls: Vec<String> = {
            let r = self.reachable.read().unwrap();
            r.keys().cloned().collect()
        };
        for url in urls {
            if probe_mint(&self.client, &url).await {
                self.mark_reachable(&url);
            } else {
                self.mark_unreachable(&url);
            }
        }
    }

    pub fn start_proactive_checks(self: &Arc<Self>) {
        let tracker = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(PROBE_INTERVAL_SECS)).await;
                let urls: Vec<String> = {
                    let r = tracker.reachable.read().unwrap();
                    r.keys().cloned().collect()
                };
                for url in urls {
                    if probe_mint(&tracker.client, &url).await {
                        tracker.mark_reachable(&url);
                    } else {
                        tracker.mark_unreachable(&url);
                        tracing::warn!(mint = %url, "proactive probe failed");
                    }
                }
            }
        });
    }

    pub fn mark_unreachable(&self, mint_url: &str) {
        let was = {
            let mut r = self.reachable.write().unwrap();
            let prev = r.get(mint_url).copied().unwrap_or(false);
            r.insert(mint_url.to_string(), false);
            prev
        };
        if was {
            tracing::warn!(mint = %mint_url, "mint marked unreachable");
        }
        if let Ok(mut s) = self.consecutive_successes.write() {
            s.insert(mint_url.to_string(), 0);
        }
    }

    pub fn mark_reachable(&self, mint_url: &str) {
        let currently_reachable = self
            .reachable
            .read()
            .unwrap()
            .get(mint_url)
            .copied()
            .unwrap_or(false);
        if currently_reachable {
            return;
        }
        let should_recover = {
            let mut s = self.consecutive_successes.write().unwrap();
            let count = s.entry(mint_url.to_string()).or_insert(0);
            *count += 1;
            *count >= self.recovery_threshold
        };
        if should_recover {
            if let Ok(mut r) = self.reachable.write() {
                r.insert(mint_url.to_string(), true);
            }
            if let Ok(mut s) = self.consecutive_successes.write() {
                s.insert(mint_url.to_string(), 0);
            }
            tracing::info!(mint = %mint_url, "mint recovered");
        }
    }

    pub fn is_reachable(&self, mint_url: &str) -> bool {
        self.reachable
            .read()
            .unwrap()
            .get(mint_url)
            .copied()
            .unwrap_or(false)
    }

    pub fn get_reachable_mints(&self, all_mints: &[AcceptedMint]) -> Vec<AcceptedMint> {
        let r = self.reachable.read().unwrap();
        all_mints
            .iter()
            .filter(|m| r.get(&m.url).copied().unwrap_or(false))
            .cloned()
            .collect()
    }

    pub fn all_unreachable(&self) -> bool {
        let r = self.reachable.read().unwrap();
        r.is_empty() || !r.values().any(|v| *v)
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
        vec!["http://a.test".into(), "http://b.test".into()]
    }

    #[test]
    fn new_tracker_all_reachable() {
        let t = MintHealthTracker::new(&mints());
        assert!(t.is_reachable("http://a.test"));
        assert!(t.is_reachable("http://b.test"));
        assert!(!t.all_unreachable());
    }

    #[test]
    fn mark_unreachable_works() {
        let t = MintHealthTracker::new(&mints());
        t.mark_unreachable("http://a.test");
        assert!(!t.is_reachable("http://a.test"));
        assert!(t.is_reachable("http://b.test"));
    }

    #[test]
    fn all_unreachable_when_all_down() {
        let t = MintHealthTracker::new(&mints());
        t.mark_unreachable("http://a.test");
        t.mark_unreachable("http://b.test");
        assert!(t.all_unreachable());
    }

    #[test]
    fn get_reachable_filters() {
        let t = MintHealthTracker::new(&mints());
        t.mark_unreachable("http://a.test");
        let all = vec![
            AcceptedMint { url: "http://a.test".into(), price_per_step: 1, unit: "sat".into(), min_steps: 1 },
            AcceptedMint { url: "http://b.test".into(), price_per_step: 2, unit: "sat".into(), min_steps: 1 },
        ];
        let filtered = t.get_reachable_mints(&all);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].url, "http://b.test");
    }

    #[test]
    fn recovery_needs_threshold() {
        let t = MintHealthTracker::new(&mints());
        t.mark_unreachable("http://a.test");
        assert!(!t.is_reachable("http://a.test"));
        t.mark_reachable("http://a.test");
        assert!(!t.is_reachable("http://a.test"));
        t.mark_reachable("http://a.test");
        assert!(!t.is_reachable("http://a.test"));
        t.mark_reachable("http://a.test");
        assert!(t.is_reachable("http://a.test"));
    }
}
