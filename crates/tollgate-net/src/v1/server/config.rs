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
            url: "https://testnut.cashu.exchange".to_owned(),
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
const fn default_true() -> bool {
    true
}
const fn default_false() -> bool {
    false
}

fn mint_hostname_has_test(url: &str) -> bool {
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .split(':')
        .next()
        .unwrap_or(url);
    host.to_ascii_lowercase().contains("test")
}

pub const CONFIG_SCHEMA_VERSION: &str = "v0.0.7";

/// Top-level server configuration, matching Go v1's `/etc/tollgate/config.json`.
///
/// Unknown / not-yet-modeled fields (e.g. `upstream_detector`,
/// `upstream_session_manager`, `upstream_wifi`) are captured in `extra` so that
/// a round-trip load → save preserves them. This is required for a drop-in
/// replacement that may rewrite the operator's config file.
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
    #[serde(default = "default_true")]
    pub show_setup: bool,
    #[serde(default = "default_false")]
    pub reseller_mode: bool,
    #[serde(default = "default_accepted_mints")]
    pub accepted_mints: Vec<MintConfig>,
    #[serde(default = "default_profit_share")]
    pub profit_share: Vec<ProfitShareConfig>,
    /// Fields present in the on-disk JSON that this binary does not yet model
    /// natively (preserved verbatim across load/save).
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
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
            show_setup: default_true(),
            reseller_mode: default_false(),
            accepted_mints: default_accepted_mints(),
            profit_share: default_profit_share(),
            extra: std::collections::BTreeMap::new(),
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

    /// Validate the configuration, returning a list of human-readable problems.
    ///
    /// Mirrors the Go `config_manager` validation: metric enum, non-empty mints,
    /// and profit-share factors summing to ~1.0.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.metric != "bytes" && self.metric != "milliseconds" {
            errors.push(format!(
                "metric must be \"bytes\" or \"milliseconds\", got {:?}",
                self.metric
            ));
        }
        if self.step_size == 0 {
            errors.push("step_size must be greater than 0".to_owned());
        }
        if self.accepted_mints.is_empty() {
            errors.push("accepted_mints must not be empty".to_owned());
        }
        for (i, m) in self.accepted_mints.iter().enumerate() {
            if m.url.is_empty() {
                errors.push(format!("accepted_mints[{i}].url must not be empty"));
            } else if !mint_hostname_has_test(&m.url) {
                errors.push(format!(
                    "accepted_mints[{i}].url hostname must contain \"test\" \
                     (got {url:?} — non-test mints will cause real Bitcoin loss)",
                    url = m.url
                ));
            }
        }
        if !self.profit_share.is_empty() {
            let sum: f64 = self.profit_share.iter().map(|p| p.factor).sum();
            if (sum - 1.0).abs() > 0.001 {
                errors.push(format!(
                    "profit_share factors must sum to 1.0, got {sum:.6}"
                ));
            }
        }
        errors
    }

    /// Migrate an older config to the current schema version in-place.
    ///
    /// Currently this only stamps the `config_version`; field back-fill is
    /// handled by serde defaults on load. Returns `true` if anything changed.
    pub fn migrate(&mut self) -> bool {
        if self.config_version == CONFIG_SCHEMA_VERSION {
            return false;
        }
        tracing::info!(
            from = %self.config_version,
            to = CONFIG_SCHEMA_VERSION,
            "migrating config schema"
        );
        self.config_version = CONFIG_SCHEMA_VERSION.to_owned();
        true
    }

    /// Write the config to `path`, backing up any existing file to `<path>.bak`
    /// first (matching the Go `config_manager` backup behavior).
    pub fn save_to_file(&self, path: &str) -> Result<(), ConfigError> {
        if std::path::Path::new(path).exists() {
            let backup = format!("{path}.bak");
            std::fs::copy(path, &backup)?;
            tracing::debug!("backed up {path} to {backup}");
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
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

// ---------------------------------------------------------------------------
// identities.json (Go schema v0.0.1)
// ---------------------------------------------------------------------------

fn default_identities_version() -> String {
    "v0.0.1".to_owned()
}

/// An identity owned by this device (has a private key). Used for signing
/// advertisements/sessions and as the source key for payouts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedIdentity {
    pub name: String,
    /// Nostr private key (hex). Sensitive.
    pub privatekey: String,
}

/// A public identity referenced by `profit_share`. The `lightning_address` is
/// what payouts actually melt to.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublicIdentity {
    pub name: String,
    #[serde(default)]
    pub pubkey: String,
    #[serde(default)]
    pub lightning_address: String,
}

/// Contents of `/etc/tollgate/identities.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Identities {
    #[serde(default = "default_identities_version")]
    pub config_version: String,
    #[serde(default)]
    pub owned_identities: Vec<OwnedIdentity>,
    #[serde(default)]
    pub public_identities: Vec<PublicIdentity>,
}

impl Default for Identities {
    fn default() -> Self {
        Self {
            config_version: default_identities_version(),
            owned_identities: Vec::new(),
            public_identities: Vec::new(),
        }
    }
}

impl Identities {
    /// Load identities from `path`, or generate a fresh file containing a single
    /// `owner` owned-identity if it does not exist.
    pub fn load_or_generate(path: &str) -> Result<Self, KeyError> {
        match std::fs::read_to_string(path) {
            Ok(data) => {
                let ids: Self = serde_json::from_str(&data)?;
                Ok(ids)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let keys = Keys::generate();
                let ids = Self {
                    config_version: default_identities_version(),
                    owned_identities: vec![OwnedIdentity {
                        name: "owner".to_owned(),
                        privatekey: keys.secret_key().to_secret_hex(),
                    }],
                    public_identities: Vec::new(),
                };
                let json = serde_json::to_string_pretty(&ids)?;
                std::fs::write(path, &json)?;
                tracing::info!("generated new identities.json at {path}");
                Ok(ids)
            }
            Err(e) => Err(KeyError::Io(e)),
        }
    }

    /// Resolve the signing keys for an owned identity by name.
    #[must_use]
    pub fn owned_keys(&self, name: &str) -> Option<Keys> {
        self.owned_identities
            .iter()
            .find(|i| i.name == name)
            .and_then(|i| Keys::parse(&i.privatekey).ok())
    }

    /// Resolve the Lightning address for a (public or owned) identity by name.
    #[must_use]
    pub fn lightning_address(&self, name: &str) -> Option<String> {
        self.public_identities
            .iter()
            .find(|i| i.name == name && !i.lightning_address.is_empty())
            .map(|i| i.lightning_address.clone())
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
        assert_eq!(c.accepted_mints.len(), 1);
        assert_eq!(c.profit_share.len(), 2);

        let m0 = &c.accepted_mints[0];
        assert_eq!(m0.url, "https://testnut.cashu.exchange");
        assert_eq!(m0.min_balance, 64);
        assert_eq!(m0.balance_tolerance_percent, 10);
        assert_eq!(m0.payout_interval_seconds, 60);
        assert_eq!(m0.min_payout_amount, 128);
        assert_eq!(m0.price_per_step, 1);
        assert_eq!(m0.price_unit, "sat");
        assert_eq!(m0.purchase_min_steps, 0);

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
                    "url": "https://testmint.example.com/mint",
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
        assert_eq!(c.accepted_mints[0].url, "https://testmint.example.com/mint");
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
        assert_eq!(c.accepted_mints.len(), 1);
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
        assert_eq!(c.accepted_mints.len(), 1);
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
            show_setup: true,
            reseller_mode: false,
            accepted_mints: vec![MintConfig {
                url: "https://testmint.example.com".to_owned(),
                min_balance: 32,
                balance_tolerance_percent: 5,
                payout_interval_seconds: 30,
                min_payout_amount: 64,
                price_per_step: 2,
                price_unit: "sat".to_owned(),
                purchase_min_steps: 3,
            }],
            profit_share: vec![],
            extra: std::collections::BTreeMap::new(),
        };
        let keys = Keys::generate();
        let port = 4242;

        let v1 = sc.to_server_config(keys.clone(), port);
        assert_eq!(v1.metric, "bytes");
        assert_eq!(v1.step_size, 1000);
        assert_eq!(v1.port, 4242);
        assert_eq!(v1.accepted_mints.len(), 1);
        assert_eq!(v1.accepted_mints[0].url, "https://testmint.example.com");
        assert_eq!(v1.accepted_mints[0].price_per_step, 2);
        assert_eq!(v1.accepted_mints[0].unit, "sat");
        assert_eq!(v1.accepted_mints[0].min_steps, 3);
        assert_eq!(v1.nostr_keys.public_key(), keys.public_key());
    }

    #[test]
    fn config_preserves_unknown_fields_roundtrip() {
        // A full Go v0.0.7 config carries upstream_* objects we don't model yet;
        // they must survive a load → save round-trip unchanged.
        let json = r#"{
            "config_version": "v0.0.7",
            "metric": "bytes",
            "step_size": 22020096,
            "reseller_mode": true,
            "show_setup": false,
            "accepted_mints": [{ "url": "https://m", "price_per_step": 1 }],
            "profit_share": [{ "factor": 1.0, "identity": "owner" }],
            "upstream_wifi": { "scan_interval_seconds": 300, "signal_floor": -85 },
            "upstream_detector": { "require_valid_signature": true }
        }"#;
        let c: ServerConfig = serde_json::from_str(json).unwrap();
        assert!(c.reseller_mode);
        assert!(!c.show_setup);
        assert!(c.extra.contains_key("upstream_wifi"));
        assert!(c.extra.contains_key("upstream_detector"));

        let out = serde_json::to_value(&c).unwrap();
        assert_eq!(out["upstream_wifi"]["signal_floor"], -85);
        assert_eq!(out["upstream_detector"]["require_valid_signature"], true);
        // Modeled fields must NOT leak into `extra`.
        assert!(!c.extra.contains_key("metric"));
        assert!(!c.extra.contains_key("reseller_mode"));
    }

    #[test]
    fn config_validate_catches_bad_metric_and_profit_share() {
        let mut c = ServerConfig::default();
        c.metric = "seconds".to_owned();
        c.profit_share = vec![ProfitShareConfig {
            factor: 0.5,
            identity: "owner".to_owned(),
        }];
        let errs = c.validate();
        assert!(errs.iter().any(|e| e.contains("metric")));
        assert!(errs.iter().any(|e| e.contains("profit_share")));

        let good = ServerConfig::default();
        assert!(good.validate().is_empty(), "default config should validate");
    }

    #[test]
    fn config_migrate_stamps_version() {
        let mut c = ServerConfig {
            config_version: "v0.0.5".to_owned(),
            ..ServerConfig::default()
        };
        assert!(c.migrate());
        assert_eq!(c.config_version, CONFIG_SCHEMA_VERSION);
        assert!(!c.migrate(), "second migrate is a no-op");
    }

    #[test]
    fn config_save_creates_backup() {
        let dir = std::env::temp_dir().join("tollgate_config_test_save_backup");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let p = path.to_str().unwrap();

        std::fs::write(&path, r#"{"metric":"milliseconds"}"#).unwrap();
        ServerConfig::default().save_to_file(p).unwrap();

        assert!(path.with_extension("json.bak").exists());
        let reloaded = ServerConfig::load_from_file(p).unwrap();
        assert_eq!(reloaded.metric, "bytes");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn identities_load_or_generate_and_resolve() {
        let dir = std::env::temp_dir().join("tollgate_identities_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identities.json");
        let p = path.to_str().unwrap();

        let ids = Identities::load_or_generate(p).unwrap();
        assert_eq!(ids.config_version, "v0.0.1");
        assert_eq!(ids.owned_identities.len(), 1);
        assert_eq!(ids.owned_identities[0].name, "owner");
        assert!(ids.owned_keys("owner").is_some());

        // Reload is stable.
        let again = Identities::load_or_generate(p).unwrap();
        assert_eq!(
            ids.owned_identities[0].privatekey,
            again.owned_identities[0].privatekey
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn identities_lightning_address_lookup() {
        let json = r#"{
            "config_version": "v0.0.1",
            "owned_identities": [{ "name": "owner", "privatekey": "deadbeef" }],
            "public_identities": [
                { "name": "owner", "pubkey": "abc", "lightning_address": "owner@ln.tld" },
                { "name": "dev", "pubkey": "def", "lightning_address": "" }
            ]
        }"#;
        let ids: Identities = serde_json::from_str(json).unwrap();
        assert_eq!(ids.lightning_address("owner").as_deref(), Some("owner@ln.tld"));
        assert_eq!(ids.lightning_address("dev"), None);
        assert_eq!(ids.lightning_address("missing"), None);
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
