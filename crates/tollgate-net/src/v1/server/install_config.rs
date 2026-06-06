//! Installation lifecycle configuration (`install.json`).
//!
//! Tracks first-boot state and installation metadata, separate from
//! [`ServerConfig`](super::config::ServerConfig) which governs runtime
//! server behaviour.  Port of Go's `config_manager_install.go`.

use serde::{Deserialize, Serialize};
use std::path::Path;

const INSTALL_CONFIG_VERSION: &str = "v0.0.2";

/// Installation-specific parameters stored in `install.json`.
///
/// Tracks first-boot state and lifecycle metadata such as when the
/// package was downloaded, installed, and which release channel is in
/// use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallConfig {
    pub config_version: String,
    pub package_path: String,
    pub ip_address_randomized: bool,
    pub install_time: i64,
    pub download_time: i64,
    pub release_channel: String,
    pub ensure_default_timestamp: i64,
    pub installed_version: String,
}

impl InstallConfig {
    /// Create a new default install config.
    pub fn new_default() -> Self {
        Self {
            config_version: INSTALL_CONFIG_VERSION.to_owned(),
            package_path: "false".to_owned(),
            ip_address_randomized: false,
            install_time: 0,
            download_time: 0,
            release_channel: "stable".to_owned(),
            ensure_default_timestamp: now_unix(),
            installed_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Load `install.json` from file. Returns `None` if the file does not
    /// exist or is empty.
    pub fn load(path: &Path) -> Result<Option<Self>, std::io::Error> {
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(path)?;
        if data.trim().is_empty() {
            return Ok(None);
        }
        let config: Self = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(config))
    }

    /// Save to file as pretty-printed JSON, creating parent directories as
    /// needed.
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, data)
    }

    /// Ensure a valid `install.json` exists at `path`.
    ///
    /// - If the file exists and the version matches, loads and returns it.
    /// - If the file exists but the version is wrong, backs it up to a
    ///   `config_backups/` sibling directory and recreates with defaults.
    /// - If the file is missing, creates a new default config.
    pub fn ensure(path: &Path) -> Result<Self, std::io::Error> {
        match Self::load(path)? {
            Some(config) if config.config_version == INSTALL_CONFIG_VERSION => Ok(config),
            Some(_old_config) => {
                // Version mismatch — backup and recreate
                if let Some(parent) = path.parent() {
                    let backup_dir = parent.join("config_backups");
                    let _ = std::fs::create_dir_all(&backup_dir);
                    let ts = now_unix();
                    let backup_name = format!("install_{ts}.json");
                    let _ = std::fs::copy(path, backup_dir.join(&backup_name));
                }
                tracing::warn!("install.json version mismatch — recreating with defaults");
                let default = Self::new_default();
                default.save(path)?;
                Ok(default)
            }
            None => {
                let default = Self::new_default();
                default.save(path)?;
                Ok(default)
            }
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = InstallConfig::new_default();
        assert_eq!(cfg.config_version, "v0.0.2");
        assert!(!cfg.ip_address_randomized);
        assert_eq!(cfg.release_channel, "stable");
        assert_eq!(cfg.install_time, 0);
        assert!(cfg.ensure_default_timestamp > 0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        let cfg = InstallConfig::new_default();
        cfg.save(&path).unwrap();

        let loaded = InstallConfig::load(&path).unwrap().unwrap();
        assert_eq!(loaded.config_version, cfg.config_version);
        assert_eq!(loaded.ip_address_randomized, cfg.ip_address_randomized);
        assert_eq!(loaded.release_channel, cfg.release_channel);
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        assert!(InstallConfig::load(&path).unwrap().is_none());
    }

    #[test]
    fn load_returns_none_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, "").unwrap();
        assert!(InstallConfig::load(&path).unwrap().is_none());
    }

    #[test]
    fn ensure_creates_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        let cfg = InstallConfig::ensure(&path).unwrap();
        assert_eq!(cfg.config_version, "v0.0.2");
        assert!(path.exists());
    }

    #[test]
    fn ensure_loads_existing_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        let mut cfg = InstallConfig::new_default();
        cfg.ip_address_randomized = true;
        cfg.save(&path).unwrap();

        let loaded = InstallConfig::ensure(&path).unwrap();
        assert!(loaded.ip_address_randomized);
    }

    #[test]
    fn ensure_recreates_on_version_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.json");
        let old = serde_json::json!({
            "config_version": "v0.0.1",
            "package_path": "false",
            "ip_address_randomized": true,
            "install_time": 0,
            "download_time": 0,
            "release_channel": "stable",
            "ensure_default_timestamp": 12345,
            "installed_version": "0.0.0"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&old).unwrap()).unwrap();

        let cfg = InstallConfig::ensure(&path).unwrap();
        assert_eq!(cfg.config_version, "v0.0.2");
        assert!(!cfg.ip_address_randomized);
    }
}
