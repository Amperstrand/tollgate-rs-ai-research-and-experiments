use super::commands::CliConfig;
use std::path::PathBuf;

pub struct FileConfig {
    path: PathBuf,
}

impl FileConfig {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl CliConfig for FileConfig {
    fn get_config(&self) -> Result<serde_json::Value, String> {
        let data = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("Cannot read config: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {e}"))
    }

    fn set_value(&self, key: &str, value: &str) -> Result<(), String> {
        let mut cfg: serde_json::Value = {
            let data = std::fs::read_to_string(&self.path)
                .map_err(|e| format!("Cannot read config: {e}"))?;
            serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {e}"))?
        };

        let json_value = serde_json::from_str(value)
            .unwrap_or(serde_json::Value::String(value.to_owned()));

        if let serde_json::Value::Object(ref mut map) = cfg {
            map.insert(key.to_owned(), json_value);
        } else {
            return Err("Config is not a JSON object".to_owned());
        }

        let output = serde_json::to_string_pretty(&cfg)
            .map_err(|e| format!("Cannot serialize: {e}"))?;
        std::fs::write(&self.path, output).map_err(|e| format!("Cannot write config: {e}"))?;

        Ok(())
    }

    fn save_config(&self, json: &str) -> Result<(), String> {
        let parsed: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("Invalid JSON: {e}"))?;

        let required = ["config_version", "metric", "step_size", "accepted_mints"];
        let missing: Vec<&str> = required
            .iter()
            .filter(|f| parsed.get(**f).is_none())
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(format!("Missing required fields: {missing:?}"));
        }

        let output = serde_json::to_string_pretty(&parsed)
            .map_err(|e| format!("Cannot serialize: {e}"))?;
        std::fs::write(&self.path, output).map_err(|e| format!("Cannot write config: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::cli::commands::CliConfig;

    #[test]
    fn file_config_get_reads_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"milliseconds","step_size":60000}"#).unwrap();
        let cfg = FileConfig::new(path);
        let val = cfg.get_config().unwrap();
        assert_eq!(val["metric"], "milliseconds");
    }

    #[test]
    fn file_config_set_modifies_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"milliseconds","step_size":60000}"#).unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("metric", "bytes").unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();
        let updated_val: serde_json::Value =
            serde_json::from_str(&updated).unwrap();
        assert_eq!(updated_val["metric"], "bytes");
    }

    #[test]
    fn file_config_save_validates_required_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.save_config(r#"{"metric":"ms"}"#).unwrap_err();
        assert!(err.contains("Missing required fields"));
    }

    #[test]
    fn file_config_save_writes_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.save_config(
            r#"{"config_version":1,"metric":"ms","step_size":60,"accepted_mints":[]}"#,
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["metric"], "ms");
    }

    #[test]
    fn file_config_get_missing_file() {
        let cfg = FileConfig::new(std::path::PathBuf::from("/nonexistent/config.json"));
        assert!(cfg.get_config().is_err());
    }

    #[test]
    fn file_config_set_parses_json_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"step_size":60000}"#).unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("step_size", "30000").unwrap();
        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["step_size"], 30000);
    }
}
