#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use nostr::prelude::Keys;

use super::{AcceptedMint, V1ServerConfig};

fn default_config_version() -> String {
    "v0.0.7".to_owned()
}
fn default_log_level() -> String {
    "info".to_owned()
}
fn default_metric() -> String {
    "bytes".to_owned()
}
const fn default_step_size() -> u64 {
    22_020_096 // 21 MiB
}
const fn default_margin() -> f64 {
    0.1
}
fn default_accepted_mints() -> Vec<MintConfig> {
    vec![
        MintConfig {
            url: "https://mint.coinos.io".to_owned(),
            min_balance: 64,
            balance_tolerance_percent: 10,
            payout_interval_seconds: 60,
            min_payout_amount: 128,
            price_per_step: 1,
            price_unit: "sat".to_owned(),
            purchase_min_steps: 0,
        },
        MintConfig {
            url: "https://mint.minibits.cash/Bitcoin".to_owned(),
            min_balance: 64,
            balance_tolerance_percent: 10,
            payout_interval_seconds: 60,
            min_payout_amount: 128,
            price_per_step: 1,
            price_unit: "sat".to_owned(),
            purchase_min_steps: 0,
        },
    ]
}
fn default_profit_share() -> Vec<ProfitShareConfig> {
    vec![
        ProfitShareConfig {
            factor: 0.79,
            identity: "owner".to_owned(),
        },
        ProfitShareConfig {
            factor: 0.21,
            identity: "developer".to_owned(),
        },
    ]
}
const fn default_zero_u64() -> u64 {
    0
}

/// Top-level server configuration, matching Go v1's `/etc/tollgate/config.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_config_version")]
    pub config_version: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default = "default_step_size")]
    pub step_size: u64,
    #[serde(default = "default_margin")]
    pub margin: f64,
    #[serde(default = "default_accepted_mints")]
    pub accepted_mints: Vec<MintConfig>,
    #[serde(default = "default_profit_share")]
    pub profit_share: Vec<ProfitShareConfig>,
}

/// Per-mint configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MintConfig {
    pub url: String,
    #[serde(default = "default_u64::<64>")]
    pub min_balance: u64,
    #[serde(default = "default_u64::<10>")]
    pub balance_tolerance_percent: u64,
    #[serde(default = "default_u64::<60>")]
    pub payout_interval_seconds: u64,
    #[serde(default = "default_u64::<128>")]
    pub min_payout_amount: u64,
    pub price_per_step: u64,
    #[serde(default = "default_sat")]
    pub price_unit: String,
    #[serde(default = "default_zero_u64")]
    pub purchase_min_steps: u64,
}

/// Profit-share entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfitShareConfig {
    pub factor: f64,
    #[serde(alias = "pubkey")]
    pub identity: String,
}

fn default_u64<const V: u64>() -> u64 {
    V
}
fn default_sat() -> String {
    "sat".to_owned()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct NostrKeyFile {
    config_version: String,
    private_key: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid key: {0}")]
    InvalidKey(String),
}

/// Load Nostr keys from a JSON file, or generate and save new ones.
///
/// File format matches Go v1's `identities.json` (single-identity subset):
/// ```json
/// { "config_version": "v0.0.1", "private_key": "<hex>" }
/// ```
pub fn load_or_generate_keys(path: &str) -> Result<Keys, KeyError> {
    match std::fs::read_to_string(path) {
        Ok(data) => {
            let kf: NostrKeyFile = serde_json::from_str(&data)?;
            let keys = Keys::parse(&kf.private_key).map_err(|e| {
                KeyError::InvalidKey(format!("failed to parse private key: {e}"))
            })?;
            tracing::info!(public_key = %keys.public_key(), "Loaded Nostr keys from {path}");
            Ok(keys)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let keys = Keys::generate();
            let kf = NostrKeyFile {
                config_version: "v0.0.1".to_owned(),
                private_key: keys.secret_key().to_secret_hex(),
            };
            let json = serde_json::to_string_pretty(&kf)?;
            std::fs::write(path, &json)?;
            tracing::info!(public_key = %keys.public_key(), "Generated new Nostr keys, saved to {path}");
            Ok(keys)
        }
        Err(e) => Err(KeyError::Io(e)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            config_version: default_config_version(),
            log_level: default_log_level(),
            metric: default_metric(),
            step_size: default_step_size(),
            margin: default_margin(),
            accepted_mints: default_accepted_mints(),
            profit_share: default_profit_share(),
        }
    }
}

impl ServerConfig {
    /// Load configuration from a JSON file.
    ///
    /// If the file does not exist, returns defaults.
    /// Partial JSON is fine — missing fields fall back to defaults via serde.
    pub fn load_from_file(path: &str) -> Result<Self, ConfigError> {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("config file {path} not found, using defaults");
                return Ok(Self::default());
            }
            Err(e) => return Err(ConfigError::Io(e)),
        };
        let config: ServerConfig = serde_json::from_str(&data)?;
        Ok(config)
    }

    /// Convert to the runtime `V1ServerConfig`.
    pub fn to_server_config(&self, keys: Keys, port: u16) -> V1ServerConfig {
        let accepted_mints = self
            .accepted_mints
            .iter()
            .map(|m| AcceptedMint {
                url: m.url.clone(),
                price_per_step: m.price_per_step,
                unit: m.price_unit.clone(),
                min_steps: m.purchase_min_steps,
            })
            .collect();

        V1ServerConfig {
            metric: self.metric.clone(),
            step_size: self.step_size,
            accepted_mints,
            nostr_keys: keys,
            port,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_values() {
        let c = ServerConfig::default();
        assert_eq!(c.config_version, "v0.0.7");
        assert_eq!(c.log_level, "info");
        assert_eq!(c.metric, "bytes");
        assert_eq!(c.step_size, 22_020_096);
        assert!((c.margin - 0.1).abs() < f64::EPSILON);
        assert_eq!(c.accepted_mints.len(), 2);
        assert_eq!(c.profit_share.len(), 2);

        let m0 = &c.accepted_mints[0];
        assert_eq!(m0.url, "https://mint.coinos.io");
        assert_eq!(m0.min_balance, 64);
        assert_eq!(m0.balance_tolerance_percent, 10);
        assert_eq!(m0.payout_interval_seconds, 60);
        assert_eq!(m0.min_payout_amount, 128);
        assert_eq!(m0.price_per_step, 1);
        assert_eq!(m0.price_unit, "sat");
        assert_eq!(m0.purchase_min_steps, 0);

        let m1 = &c.accepted_mints[1];
        assert_eq!(m1.url, "https://mint.minibits.cash/Bitcoin");

        assert!((c.profit_share[0].factor - 0.79).abs() < f64::EPSILON);
        assert_eq!(c.profit_share[0].identity, "owner");
        assert!((c.profit_share[1].factor - 0.21).abs() < f64::EPSILON);
        assert_eq!(c.profit_share[1].identity, "developer");
    }

    #[test]
    fn config_load_from_file() {
        let dir = std::env::temp_dir().join("tollgate_config_test_load");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let json = r#"{
            "config_version": "v0.1.0",
            "log_level": "debug",
            "metric": "milliseconds",
            "step_size": 60000,
            "margin": 0.2,
            "accepted_mints": [
                {
                    "url": "https://example.com/mint",
                    "price_per_step": 5
                }
            ],
            "profit_share": [
                { "factor": 0.9, "identity": "alice" },
                { "factor": 0.1, "identity": "bob" }
            ]
        }"#;
        std::fs::write(&path, json).unwrap();

        let c = ServerConfig::load_from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.config_version, "v0.1.0");
        assert_eq!(c.log_level, "debug");
        assert_eq!(c.metric, "milliseconds");
        assert_eq!(c.step_size, 60000);
        assert!((c.margin - 0.2).abs() < f64::EPSILON);
        assert_eq!(c.accepted_mints.len(), 1);
        assert_eq!(c.accepted_mints[0].url, "https://example.com/mint");
        assert_eq!(c.accepted_mints[0].price_per_step, 5);
        assert_eq!(c.accepted_mints[0].min_balance, 64);
        assert_eq!(c.accepted_mints[0].balance_tolerance_percent, 10);
        assert_eq!(c.accepted_mints[0].payout_interval_seconds, 60);
        assert_eq!(c.accepted_mints[0].min_payout_amount, 128);
        assert_eq!(c.accepted_mints[0].price_unit, "sat");
        assert_eq!(c.accepted_mints[0].purchase_min_steps, 0);
        assert_eq!(c.profit_share.len(), 2);
        assert!((c.profit_share[0].factor - 0.9).abs() < f64::EPSILON);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_missing_file_returns_defaults() {
        let c = ServerConfig::load_from_file("/tmp/tollgate_nonexistent_config_test_99999.json")
            .unwrap();
        assert_eq!(c.metric, "bytes");
        assert_eq!(c.step_size, 22_020_096);
        assert_eq!(c.accepted_mints.len(), 2);
    }

    #[test]
    fn config_partial_json_uses_defaults() {
        let dir = std::env::temp_dir().join("tollgate_config_test_partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.json");
        let json = r#"{ "metric": "milliseconds" }"#;
        std::fs::write(&path, json).unwrap();

        let c = ServerConfig::load_from_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.metric, "milliseconds");
        assert_eq!(c.config_version, "v0.0.7");
        assert_eq!(c.log_level, "info");
        assert_eq!(c.step_size, 22_020_096);
        assert!((c.margin - 0.1).abs() < f64::EPSILON);
        assert_eq!(c.accepted_mints.len(), 2);
        assert_eq!(c.profit_share.len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_to_server_config() {
        let sc = ServerConfig {
            config_version: "v0.1.0".to_owned(),
            log_level: "debug".to_owned(),
            metric: "bytes".to_owned(),
            step_size: 1000,
            margin: 0.15,
            accepted_mints: vec![MintConfig {
                url: "https://mint.example.com".to_owned(),
                min_balance: 32,
                balance_tolerance_percent: 5,
                payout_interval_seconds: 30,
                min_payout_amount: 64,
                price_per_step: 2,
                price_unit: "sat".to_owned(),
                purchase_min_steps: 3,
            }],
            profit_share: vec![],
        };
        let keys = Keys::generate();
        let port = 4242;

        let v1 = sc.to_server_config(keys.clone(), port);
        assert_eq!(v1.metric, "bytes");
        assert_eq!(v1.step_size, 1000);
        assert_eq!(v1.port, 4242);
        assert_eq!(v1.accepted_mints.len(), 1);
        assert_eq!(v1.accepted_mints[0].url, "https://mint.example.com");
        assert_eq!(v1.accepted_mints[0].price_per_step, 2);
        assert_eq!(v1.accepted_mints[0].unit, "sat");
        assert_eq!(v1.accepted_mints[0].min_steps, 3);
        assert_eq!(v1.nostr_keys.public_key(), keys.public_key());
    }

    #[test]
    fn config_roundtrip_serialize() {
        let c = ServerConfig::default();
        let json = serde_json::to_string_pretty(&c).unwrap();
        let c2: ServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c.config_version, c2.config_version);
        assert_eq!(c.metric, c2.metric);
        assert_eq!(c.step_size, c2.step_size);
        assert_eq!(c.accepted_mints.len(), c2.accepted_mints.len());
        assert_eq!(c.profit_share.len(), c2.profit_share.len());
    }

    #[test]
    fn key_load_or_generate_creates_new_file() {
        let dir = std::env::temp_dir().join("tollgate_key_test_new");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");

        let keys = super::super::config::load_or_generate_keys(path.to_str().unwrap()).unwrap();
        assert!(!keys.public_key().to_hex().is_empty());

        let data = std::fs::read_to_string(&path).unwrap();
        assert!(data.contains("private_key"));
        assert!(data.contains("v0.0.1"));

        let loaded = super::super::config::load_or_generate_keys(path.to_str().unwrap()).unwrap();
        assert_eq!(keys.public_key(), loaded.public_key());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn key_load_or_generate_loads_existing() {
        let dir = std::env::temp_dir().join("tollgate_key_test_existing");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("existing_keys.json");

        let original = Keys::generate();
        let kf = super::super::config::NostrKeyFile {
            config_version: "v0.0.1".to_owned(),
            private_key: original.secret_key().to_secret_hex(),
        };
        let json = serde_json::to_string_pretty(&kf).unwrap();
        std::fs::write(&path, &json).unwrap();

        let loaded = super::super::config::load_or_generate_keys(path.to_str().unwrap()).unwrap();
        assert_eq!(original.public_key(), loaded.public_key());

        std::fs::remove_dir_all(&dir).ok();
    }
}
