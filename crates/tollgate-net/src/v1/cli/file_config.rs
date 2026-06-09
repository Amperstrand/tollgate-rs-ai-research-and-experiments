use super::commands::CliConfig;
use super::config_schema::{get_config_schema, get_identities_schema, FieldSchema};
use std::path::PathBuf;

pub struct FileConfig {
    path: PathBuf,
    identities_path: Option<PathBuf>,
}

impl FileConfig {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            identities_path: None,
        }
    }

    #[must_use]
    pub fn with_identities_path(mut self, path: PathBuf) -> Self {
        self.identities_path = Some(path);
        self
    }
}

impl CliConfig for FileConfig {
    fn get_config(&self) -> Result<serde_json::Value, String> {
        let data =
            std::fs::read_to_string(&self.path).map_err(|e| format!("Cannot read config: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {e}"))
    }

    fn set_value(&self, key: &str, value: &str) -> Result<(), String> {
        // Validate against schema first
        validate_against_schema(key, value)?;

        let parts: Vec<&str> = key.split('.').collect();
        if parts.is_empty() {
            return Err("empty key".to_owned());
        }

        let root = parts[0];

        if root == "identities" {
            // Navigate identities JSON
            let id_path = self
                .identities_path
                .as_ref()
                .ok_or_else(|| "Identities path not configured".to_owned())?;
            let mut id_cfg = read_json_file(id_path)?;
            let rest = &parts[1..];
            if rest.is_empty() {
                return Err(
                    "cannot replace entire identities object; use specific fields".to_owned(),
                );
            }
            navigate_and_set(&mut id_cfg, rest, value)?;
            write_json_file(id_path, &id_cfg)
        } else {
            // Navigate config JSON
            let mut cfg = read_json_file(&self.path)?;
            navigate_and_set(&mut cfg, &parts, value)?;
            write_json_file(&self.path, &cfg)
        }
    }

    fn save_config(&self, json: &str) -> Result<(), String> {
        let parsed: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("Invalid JSON: {e}"))?;

        let required = [
            "config_version",
            "metric",
            "step_size",
            "accepted_mints",
            "profit_share",
        ];
        let missing: Vec<&str> = required
            .iter()
            .filter(|f| parsed.get(**f).is_none())
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(format!("Missing required fields: {missing:?}"));
        }

        // Validate profit_share factors sum to 1.0
        validate_profit_share(&parsed)?;

        let output =
            serde_json::to_string_pretty(&parsed).map_err(|e| format!("Cannot serialize: {e}"))?;
        std::fs::write(&self.path, output).map_err(|e| format!("Cannot write config: {e}"))?;

        Ok(())
    }

    fn get_identities(&self) -> Result<serde_json::Value, String> {
        let Some(id_path) = self.identities_path.as_ref() else {
            return Ok(serde_json::json!({}));
        };
        if !id_path.exists() {
            return Ok(serde_json::json!({}));
        }
        let data =
            std::fs::read_to_string(id_path).map_err(|e| format!("Cannot read identities: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("Invalid identities JSON: {e}"))
    }

    fn save_identities(&self, json: &str) -> Result<(), String> {
        let id_path = self
            .identities_path
            .as_ref()
            .ok_or_else(|| "Identities path not configured".to_owned())?;

        let parsed: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("Invalid JSON: {e}"))?;

        // Validate it has config_version
        if parsed.get("config_version").is_none() {
            return Err("Missing required field: config_version".to_owned());
        }

        let output =
            serde_json::to_string_pretty(&parsed).map_err(|e| format!("Cannot serialize: {e}"))?;
        std::fs::write(id_path, output).map_err(|e| format!("Cannot write identities: {e}"))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Dot-path schema validation (matches Go's validateAgainstSchema)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn validate_against_schema(key: &str, value: &str) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        return Err("empty key".to_owned());
    }

    let root = parts[0];

    let (schema, path_parts): (Vec<FieldSchema>, &[&str]) = if root == "identities" {
        (get_identities_schema(), &parts[1..])
    } else {
        (get_config_schema(), &parts[..])
    };

    if path_parts.is_empty() {
        return Err(format!("unknown config key {key:?}"));
    }

    let mut field: Option<&FieldSchema> = None;
    let mut current_schema: &Vec<FieldSchema> = &schema;

    for (i, part) in path_parts.iter().enumerate() {
        // Array index?
        if part.parse::<usize>().is_ok() {
            if let Some(f) = field {
                if f.field_type == "array" && !f.children.is_empty() {
                    current_schema = &f.children;
                }
            }
            continue;
        }

        let mut found = false;
        for schema_item in current_schema {
            if schema_item.json_key == *part {
                field = Some(schema_item);
                found = true;
                if i < path_parts.len() - 1 && !schema_item.children.is_empty() {
                    current_schema = &schema_item.children;
                }
                break;
            }
        }
        if !found {
            return Err(format!("unknown config key {:?}", parts[..=i].join(".")));
        }
    }

    let Some(field) = field else {
        return Err(format!("unknown config key {key:?}"));
    };

    // Reject container types
    if field.field_type == "array" || field.field_type == "object" {
        return Err(format!(
            "cannot set container type {:?} via dot-path",
            field.field_type
        ));
    }

    // Check enum values
    if !field.r#enum.is_empty() {
        if field.r#enum.iter().any(|e| e == value) {
            return Ok(());
        }
        return Err(format!(
            "value {:?} not in allowed values: {:?}",
            value, field.r#enum
        ));
    }

    // Type-specific validation
    match field.field_type.as_str() {
        "duration" => {
            parse_go_duration(value)?;
        }
        "bool" if value != "true" && value != "false" => {
            return Err(format!("invalid bool value {value:?}"));
        }
        "uint64" => {
            let n: u64 = value
                .parse()
                .map_err(|_| format!("invalid uint64 value {value:?}"))?;
            if let Some(ref min) = field.min {
                if let Some(min_val) = min.as_u64() {
                    if n < min_val {
                        return Err(format!("value {n} is below minimum {min_val}"));
                    }
                }
            }
            if let Some(ref max) = field.max {
                if let Some(max_val) = max.as_u64() {
                    if n > max_val {
                        return Err(format!("value {n} exceeds maximum {max_val}"));
                    }
                }
            }
        }
        "int" => {
            let n: i64 = value
                .parse()
                .map_err(|_| format!("invalid int value {value:?}"))?;
            if let Some(ref min) = field.min {
                if let Some(min_val) = json_to_i64(min) {
                    if n < min_val {
                        return Err(format!("value {n} is below minimum {min_val}"));
                    }
                }
            }
            if let Some(ref max) = field.max {
                if let Some(max_val) = json_to_i64(max) {
                    if n > max_val {
                        return Err(format!("value {n} exceeds maximum {max_val}"));
                    }
                }
            }
        }
        "float64" => {
            let f: f64 = value
                .parse()
                .map_err(|_| format!("invalid float64 value {value:?}"))?;
            if let Some(ref min) = field.min {
                if let Some(min_val) = json_to_f64(min) {
                    if f < min_val {
                        return Err(format!("value {f} is below minimum {min_val}"));
                    }
                }
            }
            if let Some(ref max) = field.max {
                if let Some(max_val) = json_to_f64(max) {
                    if f > max_val {
                        return Err(format!("value {f} exceeds maximum {max_val}"));
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn json_to_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Parse Go-style duration strings matching Go's `time.ParseDuration`.
/// Supports: "300ms", "10s", "2m", "1h", "5m30s", "1h2m3s", etc.
fn parse_go_duration(s: &str) -> Result<(), String> {
    let original = s;
    let s = s.trim();
    if s.is_empty() {
        return Err("invalid duration (empty)".to_owned());
    }

    let mut total_ns: i64 = 0;
    let mut i = 0;
    let bytes = s.as_bytes();

    while i < bytes.len() {
        // Read number part
        let num_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if i == num_start {
            return Err(format!("invalid duration {original:?}"));
        }
        let num_str = &s[num_start..i];

        // Read unit part
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit_str = &s[unit_start..i];
        if unit_str.is_empty() {
            return Err(format!("invalid duration {original:?} (missing unit)"));
        }

        let n: f64 = num_str
            .parse()
            .map_err(|_| format!("invalid duration {original:?}"))?;

        let multiplier: i64 = match unit_str {
            "ns" => 1,
            "us" | "µs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => {
                return Err(format!(
                    "invalid duration unit {unit_str:?} in {original:?}"
                ))
            }
        };

        #[allow(clippy::cast_possible_truncation)]
        {
            total_ns += (n * multiplier as f64) as i64;
        }
    }

    if total_ns <= 0 {
        return Err(format!("invalid duration {original:?}"));
    }

    let _ = total_ns; // Just validating it parses
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON navigation helpers
// ---------------------------------------------------------------------------

fn read_json_file(path: &PathBuf) -> Result<serde_json::Value, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("Cannot read config: {e}"))?;
    serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {e}"))
}

fn write_json_file(path: &PathBuf, val: &serde_json::Value) -> Result<(), String> {
    let output = serde_json::to_string_pretty(val).map_err(|e| format!("Cannot serialize: {e}"))?;
    std::fs::write(path, output).map_err(|e| format!("Cannot write config: {e}"))
}

/// Navigate a JSON value tree by dot-path parts and set the final leaf.
/// Parts can be object keys or array indices (numeric strings).
fn navigate_and_set(
    root: &mut serde_json::Value,
    parts: &[&str],
    value: &str,
) -> Result<(), String> {
    if parts.is_empty() {
        return Err("empty path".to_owned());
    }

    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;

        if is_last {
            let json_val = coerce_value(value);
            match current {
                serde_json::Value::Object(map) => {
                    if let Some(existing) = map.get_mut(*part) {
                        *existing = json_val;
                    } else {
                        return Err(format!("key {part:?} not found in object"));
                    }
                }
                serde_json::Value::Array(arr) => {
                    let idx: usize = part
                        .parse()
                        .map_err(|_| format!("invalid array index {part:?}"))?;
                    if idx >= arr.len() {
                        return Err(format!(
                            "index {idx} out of range (len={}) at {}",
                            arr.len(),
                            parts[..=i].join(".")
                        ));
                    }
                    arr[idx] = json_val;
                }
                _ => {
                    return Err(format!(
                        "cannot navigate into {} at {}",
                        json_type_name(current),
                        parts[..i].join(".")
                    ));
                }
            }
        } else {
            // Navigate deeper
            current = match current {
                serde_json::Value::Object(map) => map
                    .get_mut(*part)
                    .ok_or_else(|| format!("key {part:?} not found in object"))?,
                serde_json::Value::Array(arr) => {
                    let idx: usize = part
                        .parse()
                        .map_err(|_| format!("invalid array index {part:?}"))?;
                    if idx >= arr.len() {
                        return Err(format!(
                            "index {idx} out of range (len={}) at {}",
                            arr.len(),
                            parts[..=i].join(".")
                        ));
                    }
                    &mut arr[idx]
                }
                _ => {
                    return Err(format!(
                        "cannot navigate into {} at {}",
                        json_type_name(current),
                        parts[..i].join(".")
                    ));
                }
            };
        }
    }

    Ok(())
}

/// Coerce a string value to the appropriate JSON type.
/// Tries to parse as JSON first, falls back to string.
fn coerce_value(value: &str) -> serde_json::Value {
    // Try parsing as JSON (covers numbers, bools, null, arrays, objects)
    if let Ok(v) = serde_json::from_str(value) {
        return v;
    }
    // Fallback: treat as bare string
    serde_json::Value::String(value.to_owned())
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Profit share validation
// ---------------------------------------------------------------------------

const PROFIT_SHARE_SUM_TOLERANCE: f64 = 1e-6;

fn validate_profit_share(cfg: &serde_json::Value) -> Result<(), String> {
    let profit_share = cfg
        .get("profit_share")
        .ok_or_else(|| "profit_share is required".to_owned())?;

    let arr = profit_share
        .as_array()
        .ok_or_else(|| "profit_share must be an array".to_owned())?;

    if arr.is_empty() {
        return Err("profit_share is empty: at least one entry required".to_owned());
    }

    let mut sum: f64 = 0.0;
    for (i, entry) in arr.iter().enumerate() {
        let factor = entry
            .get("factor")
            .ok_or_else(|| format!("profit_share[{i}] missing factor field"))?;
        let f = factor
            .as_f64()
            .ok_or_else(|| format!("profit_share[{i}] factor is not a number"))?;

        if f < 0.0 {
            return Err(format!("profit_share[{i}] has negative factor {f}"));
        }
        if f > 1.0 {
            return Err(format!(
                "profit_share[{i}] has factor {f} > 1.0 (use decimal ratio, not percentage)"
            ));
        }
        sum += f;
    }

    if (sum - 1.0).abs() > PROFIT_SHARE_SUM_TOLERANCE {
        return Err(format!(
            "profit_share factors must sum to 1.0, got {sum} ({:.1}% will remain in wallet each payout cycle)",
            (1.0 - sum) * 100.0
        ));
    }

    Ok(())
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
        std::fs::write(
            &path,
            r#"{"metric":"milliseconds","step_size":60000,"log_level":"info","accepted_mints":[],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("metric", "bytes").unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();
        let updated_val: serde_json::Value = serde_json::from_str(&updated).unwrap();
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
            r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[{"url":"https://testmint.example.com"}],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
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
        std::fs::write(
            &path,
            r#"{"step_size":60000,"metric":"bytes","accepted_mints":[],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("step_size", "30000").unwrap();
        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["step_size"], 30000);
    }

    // --- Dot-path set tests ---

    #[test]
    fn dot_path_set_top_level_metric() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"metric":"milliseconds","step_size":60000,"log_level":"info","accepted_mints":[],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("metric", "bytes").unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["metric"], "bytes");
    }

    #[test]
    fn dot_path_set_nested_array_element() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"metric":"bytes","step_size":60000,"accepted_mints":[{"url":"https://old.example.com"}],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("accepted_mints.0.url", "https://testnut.cashu.exchange")
            .unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            val["accepted_mints"][0]["url"],
            "https://testnut.cashu.exchange"
        );
    }

    #[test]
    fn dot_path_set_rejects_unknown_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.set_value("nonexistent_field", "value").unwrap_err();
        assert!(err.contains("unknown config key"));
    }

    #[test]
    fn dot_path_set_rejects_invalid_enum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.set_value("metric", "invalid_metric").unwrap_err();
        assert!(err.contains("not in allowed values"));
    }

    #[test]
    fn dot_path_set_rejects_invalid_uint64() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"step_size":60}"#).unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.set_value("step_size", "not_a_number").unwrap_err();
        assert!(err.contains("invalid uint64"));
    }

    #[test]
    fn dot_path_set_rejects_invalid_bool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"show_setup":true}"#).unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.set_value("show_setup", "yes").unwrap_err();
        assert!(err.contains("invalid bool"));
    }

    #[test]
    fn dot_path_set_validates_float64_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"margin":0.1}"#).unwrap();
        let cfg = FileConfig::new(path.clone());
        // Valid within bounds
        cfg.set_value("margin", "0.5").unwrap();
        // Out of bounds
        let err = cfg.set_value("margin", "1.5").unwrap_err();
        assert!(err.contains("exceeds maximum"));
    }

    #[test]
    fn dot_path_set_rejects_container_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"accepted_mints":[]}"#).unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg.set_value("accepted_mints", "[]").unwrap_err();
        assert!(err.contains("container type"));
    }

    #[test]
    fn dot_path_set_index_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"accepted_mints":[{"url":"https://test.example.com"}]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .set_value("accepted_mints.5.url", "https://bad.example.com")
            .unwrap_err();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn dot_path_set_identities_writes_to_identities_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let identities_path = dir.path().join("identities.json");

        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();
        std::fs::write(
            &identities_path,
            r#"{"config_version":"v0.0.1","public_identities":[{"name":"alice","pubkey":"abc123","lightning_address":"old@pay.com"}]}"#,
        )
        .unwrap();

        let cfg = FileConfig::new(config_path).with_identities_path(identities_path.clone());
        cfg.set_value(
            "identities.public_identities.0.lightning_address",
            "foo@bar.com",
        )
        .unwrap();

        let id: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&identities_path).unwrap()).unwrap();
        assert_eq!(
            id["public_identities"][0]["lightning_address"],
            "foo@bar.com"
        );
    }

    #[test]
    fn dot_path_set_identities_no_path_configured() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(config_path);
        let err = cfg
            .set_value("identities.public_identities.0.name", "alice")
            .unwrap_err();
        assert!(err.contains("Identities path not configured"));
    }

    // --- Identities get/save tests ---

    #[test]
    fn get_identities_returns_empty_when_no_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(path);
        let id = cfg.get_identities().unwrap();
        assert_eq!(id, serde_json::json!({}));
    }

    #[test]
    fn get_identities_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let identities_path = dir.path().join("identities.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(config_path).with_identities_path(identities_path);
        let id = cfg.get_identities().unwrap();
        assert_eq!(id, serde_json::json!({}));
    }

    #[test]
    fn get_identities_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let identities_path = dir.path().join("identities.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();
        std::fs::write(
            &identities_path,
            r#"{"config_version":"v0.0.1","public_identities":[]}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(config_path).with_identities_path(identities_path);
        let id = cfg.get_identities().unwrap();
        assert_eq!(id["config_version"], "v0.0.1");
    }

    #[test]
    fn save_identities_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let identities_path = dir.path().join("identities.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();

        let cfg = FileConfig::new(config_path).with_identities_path(identities_path.clone());
        cfg.save_identities(r#"{"config_version":"v0.0.1","public_identities":[{"name":"bob"}]}"#)
            .unwrap();

        let content = std::fs::read_to_string(&identities_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["config_version"], "v0.0.1");
        assert_eq!(parsed["public_identities"][0]["name"], "bob");
    }

    #[test]
    fn save_identities_requires_config_version() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let identities_path = dir.path().join("identities.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();

        let cfg = FileConfig::new(config_path).with_identities_path(identities_path);
        let err = cfg
            .save_identities(r#"{"public_identities":[]}"#)
            .unwrap_err();
        assert!(err.contains("config_version"));
    }

    #[test]
    fn save_identities_no_path_configured() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        std::fs::write(&config_path, r#"{"metric":"bytes"}"#).unwrap();
        let cfg = FileConfig::new(config_path);
        let err = cfg
            .save_identities(r#"{"config_version":"v0.0.1"}"#)
            .unwrap_err();
        assert!(err.contains("Identities path not configured"));
    }

    // --- Profit share validation tests ---

    #[test]
    fn save_config_requires_profit_share() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .save_config(
                r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[]}"#,
            )
            .unwrap_err();
        assert!(err.contains("Missing required fields"));
        assert!(err.contains("profit_share"));
    }

    #[test]
    fn save_config_rejects_profit_share_sum_not_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .save_config(
                r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[],"profit_share":[{"factor":0.5,"identity":"alice"},{"factor":0.3,"identity":"bob"}]}"#,
            )
            .unwrap_err();
        assert!(err.contains("sum to 1.0"));
    }

    #[test]
    fn save_config_accepts_valid_profit_share() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.save_config(
            r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[],"profit_share":[{"factor":1.0,"identity":"self"}]}"#,
        )
        .unwrap();
    }

    #[test]
    fn save_config_rejects_negative_factor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .save_config(
                r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[],"profit_share":[{"factor":-0.5,"identity":"alice"},{"factor":1.5,"identity":"bob"}]}"#,
            )
            .unwrap_err();
        assert!(err.contains("negative factor"));
    }

    #[test]
    fn save_config_rejects_factor_above_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .save_config(
                r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[],"profit_share":[{"factor":1.5,"identity":"alice"}]}"#,
            )
            .unwrap_err();
        assert!(err.contains("> 1.0"));
    }

    #[test]
    fn save_config_rejects_empty_profit_share() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let cfg = FileConfig::new(path);
        let err = cfg
            .save_config(
                r#"{"config_version":"v0.0.7","metric":"ms","step_size":60,"accepted_mints":[],"profit_share":[]}"#,
            )
            .unwrap_err();
        assert!(err.contains("at least one entry required"));
    }

    // --- Duration parsing tests ---

    #[test]
    fn parse_duration_valid() {
        assert!(parse_go_duration("10s").is_ok());
        assert!(parse_go_duration("300ms").is_ok());
        assert!(parse_go_duration("2m").is_ok());
        assert!(parse_go_duration("1h").is_ok());
        assert!(parse_go_duration("5m30s").is_ok());
    }

    #[test]
    fn parse_duration_invalid() {
        assert!(parse_go_duration("").is_err());
        assert!(parse_go_duration("abc").is_err());
    }

    // --- Nested object dot-path set ---

    #[test]
    fn dot_path_set_nested_object_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"upstream_detector":{"probe_timeout":"10s","probe_retry_count":3,"probe_retry_delay":"2s","require_valid_signature":true,"ignore_interfaces":[],"only_interfaces":[],"discovery_timeout":"5m0s"}}"#,
        )
        .unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("upstream_detector.probe_timeout", "30s")
            .unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["upstream_detector"]["probe_timeout"], "30s");
    }

    #[test]
    fn dot_path_set_bool_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"reseller_mode":false}"#).unwrap();
        let cfg = FileConfig::new(path.clone());
        cfg.set_value("reseller_mode", "true").unwrap();
        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["reseller_mode"], true);
    }
}
