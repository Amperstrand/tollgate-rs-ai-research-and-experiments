//! Configuration schema definition for CLI `config schema` command.
//!
//! Returns the same schema as Go v1's `GetConfigSchema()` and
//! `GetIdentitiesSchema()` so that UI frontends can dynamically render
//! config editing forms. This is pure metadata — it does NOT read or
//! modify any config file.

#![allow(clippy::too_many_lines, clippy::unreadable_literal)]

use serde::{Deserialize, Serialize};

use super::types::CLIResponse;

/// Schema descriptor for a single config field, matching Go v1's `FieldSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#enum: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FieldSchema>,
    pub json_key: String,
    pub editable: bool,
}

/// Returns the config schema matching Go v1's `GetConfigSchema()` exactly.
#[must_use]
pub fn get_config_schema() -> Vec<FieldSchema> {
    vec![
        FieldSchema {
            name: "ConfigVersion".to_owned(),
            json_key: "config_version".to_owned(),
            field_type: "string".to_owned(),
            description: Some("Configuration file version".to_owned()),
            default: Some(serde_json::json!("v0.0.7")),
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![],
            editable: false,
        },
        FieldSchema {
            name: "LogLevel".to_owned(),
            json_key: "log_level".to_owned(),
            field_type: "string".to_owned(),
            description: Some("Logging verbosity".to_owned()),
            default: Some(serde_json::json!("info")),
            required: true,
            r#enum: vec![
                "debug".to_owned(),
                "info".to_owned(),
                "warn".to_owned(),
                "error".to_owned(),
            ],
            min: None,
            max: None,
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "Metric".to_owned(),
            json_key: "metric".to_owned(),
            field_type: "string".to_owned(),
            description: Some("Metering metric type".to_owned()),
            default: Some(serde_json::json!("bytes")),
            required: true,
            r#enum: vec!["bytes".to_owned(), "milliseconds".to_owned()],
            min: None,
            max: None,
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "StepSize".to_owned(),
            json_key: "step_size".to_owned(),
            field_type: "uint64".to_owned(),
            description: Some(
                "Step size in bytes (if metric=bytes) or milliseconds (if metric=milliseconds)"
                    .to_owned(),
            ),
            default: Some(serde_json::json!(22_020_096)),
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "Margin".to_owned(),
            json_key: "margin".to_owned(),
            field_type: "float64".to_owned(),
            description: Some("Margin factor (0.0-1.0)".to_owned()),
            default: Some(serde_json::json!(0.1)),
            required: false,
            r#enum: vec![],
            min: Some(serde_json::json!(0.0)),
            max: Some(serde_json::json!(1.0)),
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "ShowSetup".to_owned(),
            json_key: "show_setup".to_owned(),
            field_type: "bool".to_owned(),
            description: Some("Show setup wizard on first access".to_owned()),
            default: Some(serde_json::json!(true)),
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "ResellerMode".to_owned(),
            json_key: "reseller_mode".to_owned(),
            field_type: "bool".to_owned(),
            description: Some("Enable reseller mode for upstream gateway discovery".to_owned()),
            default: Some(serde_json::json!(false)),
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![],
            editable: true,
        },
        FieldSchema {
            name: "AcceptedMints".to_owned(),
            json_key: "accepted_mints".to_owned(),
            field_type: "array".to_owned(),
            description: Some("List of accepted Cashu mints".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "URL".to_owned(),
                    json_key: "url".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Mint URL".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "MinBalance".to_owned(),
                    json_key: "min_balance".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Minimum balance before auto-replenish (sats)".to_owned()),
                    default: Some(serde_json::json!(64)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "BalanceTolerancePercent".to_owned(),
                    json_key: "balance_tolerance_percent".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Tolerance percentage for balance checks".to_owned()),
                    default: Some(serde_json::json!(10)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "PayoutIntervalSeconds".to_owned(),
                    json_key: "payout_interval_seconds".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Seconds between payout rounds".to_owned()),
                    default: Some(serde_json::json!(60)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "MinPayoutAmount".to_owned(),
                    json_key: "min_payout_amount".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Minimum payout amount in sats".to_owned()),
                    default: Some(serde_json::json!(128)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "PricePerStep".to_owned(),
                    json_key: "price_per_step".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Price per step in sats".to_owned()),
                    default: Some(serde_json::json!(1)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "PriceUnit".to_owned(),
                    json_key: "price_unit".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Price unit".to_owned()),
                    default: Some(serde_json::json!("sats")),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "MinPurchaseSteps".to_owned(),
                    json_key: "purchase_min_steps".to_owned(),
                    field_type: "uint64".to_owned(),
                    description: Some("Minimum number of steps per purchase".to_owned()),
                    default: Some(serde_json::json!(0)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
            ],
            editable: true,
        },
        FieldSchema {
            name: "ProfitShare".to_owned(),
            json_key: "profit_share".to_owned(),
            field_type: "array".to_owned(),
            description: Some("Profit sharing configuration".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "Factor".to_owned(),
                    json_key: "factor".to_owned(),
                    field_type: "float64".to_owned(),
                    description: Some(
                        "Share ratio (0.0\u{2013}1.0). All factors MUST sum to 1.0. \
                         Use 0.79 not 79\u{2014}this is a ratio, not a percentage."
                            .to_owned(),
                    ),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(0.0)),
                    max: Some(serde_json::json!(1.0)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "Identity".to_owned(),
                    json_key: "identity".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Identity name from identities.json".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
            ],
            editable: true,
        },
        // UpstreamDetector — lives in the `extra` BTreeMap, but we include it
        // in the schema so UI frontends can render editing forms.
        FieldSchema {
            name: "UpstreamDetector".to_owned(),
            json_key: "upstream_detector".to_owned(),
            field_type: "object".to_owned(),
            description: Some("Upstream gateway detector configuration".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "ProbeTimeout".to_owned(),
                    json_key: "probe_timeout".to_owned(),
                    field_type: "duration".to_owned(),
                    description: Some("Timeout for each probe".to_owned()),
                    default: Some(serde_json::json!("10s")),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "ProbeRetryCount".to_owned(),
                    json_key: "probe_retry_count".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Number of probe retries".to_owned()),
                    default: Some(serde_json::json!(3)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "ProbeRetryDelay".to_owned(),
                    json_key: "probe_retry_delay".to_owned(),
                    field_type: "duration".to_owned(),
                    description: Some("Delay between retries".to_owned()),
                    default: Some(serde_json::json!("2s")),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "RequireValidSignature".to_owned(),
                    json_key: "require_valid_signature".to_owned(),
                    field_type: "bool".to_owned(),
                    description: Some("Require valid NIP-70 signature".to_owned()),
                    default: Some(serde_json::json!(true)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "IgnoreInterfaces".to_owned(),
                    json_key: "ignore_interfaces".to_owned(),
                    field_type: "array".to_owned(),
                    description: Some("Interfaces to ignore".to_owned()),
                    default: Some(serde_json::json!(["lo", "docker0", "br-lan", "hostap0"])),
                    required: false,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![FieldSchema {
                        name: String::new(),
                        json_key: String::new(),
                        field_type: "string".to_owned(),
                        description: None,
                        default: None,
                        required: false,
                        r#enum: vec![],
                        min: None,
                        max: None,
                        children: vec![],
                        editable: false,
                    }],
                    editable: true,
                },
                FieldSchema {
                    name: "OnlyInterfaces".to_owned(),
                    json_key: "only_interfaces".to_owned(),
                    field_type: "array".to_owned(),
                    description: Some("Only probe these interfaces (empty = all)".to_owned()),
                    default: Some(serde_json::json!(Vec::<String>::new())),
                    required: false,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![FieldSchema {
                        name: String::new(),
                        json_key: String::new(),
                        field_type: "string".to_owned(),
                        description: None,
                        default: None,
                        required: false,
                        r#enum: vec![],
                        min: None,
                        max: None,
                        children: vec![],
                        editable: false,
                    }],
                    editable: true,
                },
                FieldSchema {
                    name: "DiscoveryTimeout".to_owned(),
                    json_key: "discovery_timeout".to_owned(),
                    field_type: "duration".to_owned(),
                    description: Some("Deduplication window".to_owned()),
                    default: Some(serde_json::json!("5m0s")),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
            ],
            editable: true,
        },
        // UpstreamSessionManager
        FieldSchema {
            name: "UpstreamSessionManager".to_owned(),
            json_key: "upstream_session_manager".to_owned(),
            field_type: "object".to_owned(),
            description: Some("Upstream session manager configuration".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "MaxPricePerMillisecond".to_owned(),
                    json_key: "max_price_per_millisecond".to_owned(),
                    field_type: "float64".to_owned(),
                    description: Some("Max sats per millisecond".to_owned()),
                    default: Some(serde_json::json!(0.002777777778)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "MaxPricePerByte".to_owned(),
                    json_key: "max_price_per_byte".to_owned(),
                    field_type: "float64".to_owned(),
                    description: Some("Max sats per byte".to_owned()),
                    default: Some(serde_json::json!(0.00003725782414)),
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "Trust".to_owned(),
                    json_key: "trust".to_owned(),
                    field_type: "object".to_owned(),
                    description: Some("Trust policy".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![
                        FieldSchema {
                            name: "DefaultPolicy".to_owned(),
                            json_key: "default_policy".to_owned(),
                            field_type: "string".to_owned(),
                            description: Some("Default trust policy".to_owned()),
                            default: Some(serde_json::json!("trust_all")),
                            required: true,
                            r#enum: vec!["trust_all".to_owned(), "trust_none".to_owned()],
                            min: None,
                            max: None,
                            children: vec![],
                            editable: true,
                        },
                        FieldSchema {
                            name: "Allowlist".to_owned(),
                            json_key: "allowlist".to_owned(),
                            field_type: "array".to_owned(),
                            description: Some("Trusted pubkeys".to_owned()),
                            default: Some(serde_json::json!(Vec::<String>::new())),
                            required: false,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![FieldSchema {
                                name: String::new(),
                                json_key: String::new(),
                                field_type: "string".to_owned(),
                                description: None,
                                default: None,
                                required: false,
                                r#enum: vec![],
                                min: None,
                                max: None,
                                children: vec![],
                                editable: false,
                            }],
                            editable: true,
                        },
                        FieldSchema {
                            name: "Blocklist".to_owned(),
                            json_key: "blocklist".to_owned(),
                            field_type: "array".to_owned(),
                            description: Some("Blocked pubkeys".to_owned()),
                            default: Some(serde_json::json!(Vec::<String>::new())),
                            required: false,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![FieldSchema {
                                name: String::new(),
                                json_key: String::new(),
                                field_type: "string".to_owned(),
                                description: None,
                                default: None,
                                required: false,
                                r#enum: vec![],
                                min: None,
                                max: None,
                                children: vec![],
                                editable: false,
                            }],
                            editable: true,
                        },
                    ],
                    editable: true,
                },
                FieldSchema {
                    name: "Sessions".to_owned(),
                    json_key: "sessions".to_owned(),
                    field_type: "object".to_owned(),
                    description: Some("Session settings".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![
                        FieldSchema {
                            name: "PreferredSessionIncrementsMilliseconds".to_owned(),
                            json_key: "preferred_session_increments_milliseconds".to_owned(),
                            field_type: "uint64".to_owned(),
                            description: Some("Preferred time session increment (ms)".to_owned()),
                            default: Some(serde_json::json!(60_000)),
                            required: true,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![],
                            editable: true,
                        },
                        FieldSchema {
                            name: "PreferredSessionIncrementsBytes".to_owned(),
                            json_key: "preferred_session_increments_bytes".to_owned(),
                            field_type: "uint64".to_owned(),
                            description: Some(
                                "Preferred data session increment (bytes)".to_owned(),
                            ),
                            default: Some(serde_json::json!(131_100_000)),
                            required: true,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![],
                            editable: true,
                        },
                        FieldSchema {
                            name: "MillisecondRenewalOffset".to_owned(),
                            json_key: "millisecond_renewal_offset".to_owned(),
                            field_type: "uint64".to_owned(),
                            description: Some("Renew this many ms before expiry".to_owned()),
                            default: Some(serde_json::json!(10_000)),
                            required: true,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![],
                            editable: true,
                        },
                        FieldSchema {
                            name: "BytesRenewalOffset".to_owned(),
                            json_key: "bytes_renewal_offset".to_owned(),
                            field_type: "uint64".to_owned(),
                            description: Some("Renew this many bytes before limit".to_owned()),
                            default: Some(serde_json::json!(131_100_000)),
                            required: true,
                            r#enum: vec![],
                            min: None,
                            max: None,
                            children: vec![],
                            editable: true,
                        },
                    ],
                    editable: true,
                },
                FieldSchema {
                    name: "UsageTracking".to_owned(),
                    json_key: "usage_tracking".to_owned(),
                    field_type: "object".to_owned(),
                    description: Some("Usage tracking settings".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![FieldSchema {
                        name: "DataMonitoringInterval".to_owned(),
                        json_key: "data_monitoring_interval".to_owned(),
                        field_type: "duration".to_owned(),
                        description: Some("How often to check data usage".to_owned()),
                        default: Some(serde_json::json!("500ms")),
                        required: true,
                        r#enum: vec![],
                        min: None,
                        max: None,
                        children: vec![],
                        editable: true,
                    }],
                    editable: true,
                },
            ],
            editable: true,
        },
        // UpstreamWifi
        FieldSchema {
            name: "UpstreamWifi".to_owned(),
            json_key: "upstream_wifi".to_owned(),
            field_type: "object".to_owned(),
            description: Some("Upstream WiFi scanning and selection configuration".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "ScanIntervalSeconds".to_owned(),
                    json_key: "scan_interval_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Seconds between full WiFi scans".to_owned()),
                    default: Some(serde_json::json!(300)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(10)),
                    max: Some(serde_json::json!(3600)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "FastCheckSeconds".to_owned(),
                    json_key: "fast_check_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Seconds between fast signal checks".to_owned()),
                    default: Some(serde_json::json!(30)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(5)),
                    max: Some(serde_json::json!(300)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "LostThreshold".to_owned(),
                    json_key: "lost_threshold".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some(
                        "Consecutive fast-check failures before marking as lost".to_owned(),
                    ),
                    default: Some(serde_json::json!(2)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(1)),
                    max: Some(serde_json::json!(10)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "HysteresisDB".to_owned(),
                    json_key: "hysteresis_db".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Signal hysteresis in dB to prevent flapping".to_owned()),
                    default: Some(serde_json::json!(12)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(0)),
                    max: Some(serde_json::json!(30)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "SignalFloor".to_owned(),
                    json_key: "signal_floor".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some(
                        "Minimum signal strength in dBm to consider a network usable".to_owned(),
                    ),
                    default: Some(serde_json::json!(-85)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(-100)),
                    max: Some(serde_json::json!(-30)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "BlacklistTTLMinutes".to_owned(),
                    json_key: "blacklist_ttl_minutes".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Minutes before a blacklisted network is retried".to_owned()),
                    default: Some(serde_json::json!(60)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(1)),
                    max: Some(serde_json::json!(1440)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "EmergencyPenalty".to_owned(),
                    json_key: "emergency_penalty".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Penalty score added on emergency disconnect".to_owned()),
                    default: Some(serde_json::json!(20)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(0)),
                    max: Some(serde_json::json!(100)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "MaxConsecutiveFailures".to_owned(),
                    json_key: "max_consecutive_failures".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Consecutive failures before emergency scan".to_owned()),
                    default: Some(serde_json::json!(3)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(1)),
                    max: Some(serde_json::json!(20)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "SwitchCooldownMinutes".to_owned(),
                    json_key: "switch_cooldown_minutes".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Minimum minutes between network switches".to_owned()),
                    default: Some(serde_json::json!(10)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(1)),
                    max: Some(serde_json::json!(120)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "StartupGraceSeconds".to_owned(),
                    json_key: "startup_grace_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Grace period on startup before scoring".to_owned()),
                    default: Some(serde_json::json!(90)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(10)),
                    max: Some(serde_json::json!(600)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "PostSwitchWaitSeconds".to_owned(),
                    json_key: "post_switch_wait_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Seconds to wait after a switch before scoring".to_owned()),
                    default: Some(serde_json::json!(5)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(1)),
                    max: Some(serde_json::json!(60)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "DHCPTimeoutSeconds".to_owned(),
                    json_key: "dhcp_timeout_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some("Timeout for DHCP after connecting to a network".to_owned()),
                    default: Some(serde_json::json!(180)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(10)),
                    max: Some(serde_json::json!(600)),
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "ManualPauseSeconds".to_owned(),
                    json_key: "manual_pause_seconds".to_owned(),
                    field_type: "int".to_owned(),
                    description: Some(
                        "Seconds to pause scanning after manual intervention".to_owned(),
                    ),
                    default: Some(serde_json::json!(120)),
                    required: true,
                    r#enum: vec![],
                    min: Some(serde_json::json!(10)),
                    max: Some(serde_json::json!(600)),
                    children: vec![],
                    editable: true,
                },
            ],
            editable: true,
        },
    ]
}

/// Returns the identities schema matching Go v1's `GetIdentitiesSchema()`.
#[must_use]
pub fn get_identities_schema() -> Vec<FieldSchema> {
    vec![
        FieldSchema {
            name: "ConfigVersion".to_owned(),
            json_key: "config_version".to_owned(),
            field_type: "string".to_owned(),
            description: Some("Identities file version".to_owned()),
            default: Some(serde_json::json!("v0.0.1")),
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![],
            editable: false,
        },
        FieldSchema {
            name: "OwnedIdentities".to_owned(),
            json_key: "owned_identities".to_owned(),
            field_type: "array".to_owned(),
            description: Some("Identities with private keys (managed by the system)".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "Name".to_owned(),
                    json_key: "name".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Identity name".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: false,
                },
                FieldSchema {
                    name: "PrivateKey".to_owned(),
                    json_key: "privatekey".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Nostr private key (sensitive)".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: false,
                },
            ],
            editable: false,
        },
        FieldSchema {
            name: "PublicIdentities".to_owned(),
            json_key: "public_identities".to_owned(),
            field_type: "array".to_owned(),
            description: Some("Public identities for profit sharing and trust".to_owned()),
            default: None,
            required: true,
            r#enum: vec![],
            min: None,
            max: None,
            children: vec![
                FieldSchema {
                    name: "Name".to_owned(),
                    json_key: "name".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Identity name".to_owned()),
                    default: None,
                    required: true,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "PubKey".to_owned(),
                    json_key: "pubkey".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some(
                        "Nostr public key \u{2014} not currently used for payouts \
                         (lightning_address is used instead)"
                            .to_owned(),
                    ),
                    default: None,
                    required: false,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
                FieldSchema {
                    name: "LightningAddress".to_owned(),
                    json_key: "lightning_address".to_owned(),
                    field_type: "string".to_owned(),
                    description: Some("Lightning address for payouts".to_owned()),
                    default: None,
                    required: false,
                    r#enum: vec![],
                    min: None,
                    max: None,
                    children: vec![],
                    editable: true,
                },
            ],
            editable: true,
        },
    ]
}

/// Handle the `config schema` CLI subcommand.
pub fn handle_config_schema() -> CLIResponse {
    CLIResponse::ok_with_data(
        "Configuration schema retrieved",
        serde_json::json!({
            "config": get_config_schema(),
            "identities": get_identities_schema(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_has_required_fields() {
        let schema = get_config_schema();
        let names: Vec<&str> = schema.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "ConfigVersion",
                "LogLevel",
                "Metric",
                "StepSize",
                "Margin",
                "ShowSetup",
                "ResellerMode",
                "AcceptedMints",
                "ProfitShare",
                "UpstreamDetector",
                "UpstreamSessionManager",
                "UpstreamWifi",
            ]
        );

        // Verify json_key values
        let keys: Vec<&str> = schema.iter().map(|f| f.json_key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "config_version",
                "log_level",
                "metric",
                "step_size",
                "margin",
                "show_setup",
                "reseller_mode",
                "accepted_mints",
                "profit_share",
                "upstream_detector",
                "upstream_session_manager",
                "upstream_wifi",
            ]
        );
    }

    #[test]
    fn test_schema_metric_has_enum() {
        let schema = get_config_schema();
        let metric = schema.iter().find(|f| f.name == "Metric").unwrap();
        assert_eq!(metric.r#enum, vec!["bytes", "milliseconds"]);
        assert!(metric.editable);
        assert_eq!(metric.default.as_ref().unwrap(), "bytes");
    }

    #[test]
    fn test_schema_log_level_has_enum() {
        let schema = get_config_schema();
        let log_level = schema.iter().find(|f| f.name == "LogLevel").unwrap();
        assert_eq!(log_level.r#enum, vec!["debug", "info", "warn", "error"]);
    }

    #[test]
    fn test_schema_all_editable_fields_have_defaults() {
        let schema = get_config_schema();
        for field in &schema {
            if field.editable && field.field_type != "array" && field.field_type != "object" {
                assert!(
                    field.default.is_some(),
                    "editable field {} should have a default",
                    field.name
                );
            }
        }
    }

    #[test]
    fn test_schema_config_version_not_editable() {
        let schema = get_config_schema();
        let cv = schema.iter().find(|f| f.name == "ConfigVersion").unwrap();
        assert!(!cv.editable);
        assert_eq!(cv.default.as_ref().unwrap(), "v0.0.7");
    }

    #[test]
    fn test_schema_accepted_mints_children() {
        let schema = get_config_schema();
        let mints = schema.iter().find(|f| f.name == "AcceptedMints").unwrap();
        assert_eq!(mints.field_type, "array");
        assert_eq!(mints.children.len(), 8);

        let child_names: Vec<&str> = mints.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            child_names,
            vec![
                "URL",
                "MinBalance",
                "BalanceTolerancePercent",
                "PayoutIntervalSeconds",
                "MinPayoutAmount",
                "PricePerStep",
                "PriceUnit",
                "MinPurchaseSteps",
            ]
        );

        // Verify MinBalance default
        let min_bal = mints
            .children
            .iter()
            .find(|c| c.name == "MinBalance")
            .unwrap();
        assert_eq!(min_bal.default.as_ref().unwrap(), 64);
    }

    #[test]
    fn test_schema_profit_share_children() {
        let schema = get_config_schema();
        let ps = schema.iter().find(|f| f.name == "ProfitShare").unwrap();
        assert_eq!(ps.children.len(), 2);

        let factor = ps.children.iter().find(|c| c.name == "Factor").unwrap();
        assert_eq!(factor.field_type, "float64");
        assert_eq!(factor.min.as_ref().unwrap(), 0.0);
        assert_eq!(factor.max.as_ref().unwrap(), 1.0);
    }

    #[test]
    fn test_schema_margin_constraints() {
        let schema = get_config_schema();
        let margin = schema.iter().find(|f| f.name == "Margin").unwrap();
        assert!(!margin.required);
        assert_eq!(margin.min.as_ref().unwrap(), 0.0);
        assert_eq!(margin.max.as_ref().unwrap(), 1.0);
        assert_eq!(margin.default.as_ref().unwrap(), 0.1);
    }

    #[test]
    fn test_schema_upstream_wifi_children() {
        let schema = get_config_schema();
        let wifi = schema.iter().find(|f| f.name == "UpstreamWifi").unwrap();
        assert_eq!(wifi.field_type, "object");
        assert_eq!(wifi.children.len(), 13);

        // Spot-check a child with min/max
        let scan = wifi
            .children
            .iter()
            .find(|c| c.name == "ScanIntervalSeconds")
            .unwrap();
        assert_eq!(scan.default.as_ref().unwrap(), 300);
        assert_eq!(scan.min.as_ref().unwrap(), 10);
        assert_eq!(scan.max.as_ref().unwrap(), 3600);
    }

    #[test]
    fn test_schema_upstream_detector_children() {
        let schema = get_config_schema();
        let detector = schema
            .iter()
            .find(|f| f.name == "UpstreamDetector")
            .unwrap();
        assert_eq!(detector.children.len(), 7);

        let probe = detector
            .children
            .iter()
            .find(|c| c.name == "ProbeTimeout")
            .unwrap();
        assert_eq!(probe.default.as_ref().unwrap(), "10s");
        assert_eq!(probe.field_type, "duration");
    }

    #[test]
    fn test_schema_upstream_session_manager_nested() {
        let schema = get_config_schema();
        let sm = schema
            .iter()
            .find(|f| f.name == "UpstreamSessionManager")
            .unwrap();

        let trust = sm.children.iter().find(|c| c.name == "Trust").unwrap();
        let default_policy = trust
            .children
            .iter()
            .find(|c| c.name == "DefaultPolicy")
            .unwrap();
        assert_eq!(default_policy.r#enum, vec!["trust_all", "trust_none"]);
        assert_eq!(default_policy.default.as_ref().unwrap(), "trust_all");
    }

    #[test]
    fn test_identities_schema_has_fields() {
        let schema = get_identities_schema();
        let names: Vec<&str> = schema.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["ConfigVersion", "OwnedIdentities", "PublicIdentities"]
        );

        // ConfigVersion not editable
        assert!(!schema[0].editable);
        assert_eq!(schema[0].default.as_ref().unwrap(), "v0.0.1");

        // OwnedIdentities not editable
        assert!(!schema[1].editable);
        assert_eq!(schema[1].children.len(), 2);

        // PublicIdentities editable
        assert!(schema[2].editable);
        assert_eq!(schema[2].children.len(), 3);

        let pub_child_names: Vec<&str> =
            schema[2].children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(pub_child_names, vec!["Name", "PubKey", "LightningAddress"]);
    }

    #[test]
    fn test_handle_config_schema_response() {
        let resp = handle_config_schema();
        assert!(resp.success);
        assert_eq!(resp.message.unwrap(), "Configuration schema retrieved");

        let data = resp.data.unwrap();
        assert!(data.get("config").is_some());
        assert!(data.get("identities").is_some());

        let config = data.get("config").unwrap().as_array().unwrap();
        assert_eq!(config.len(), 12);

        let identities = data.get("identities").unwrap().as_array().unwrap();
        assert_eq!(identities.len(), 3);
    }

    #[test]
    fn test_schema_serializes_without_empty_optional_fields() {
        let schema = get_config_schema();
        let json = serde_json::to_string(&schema).unwrap();

        // Fields with no enum should not serialize "enum": []
        // Fields with no children should not serialize "children": []
        // Fields with no min/max/description/default should omit them
        let parsed: Vec<FieldSchema> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), schema.len());

        // Verify round-trip stability for a nested field
        let metric = parsed.iter().find(|f| f.name == "Metric").unwrap();
        assert_eq!(metric.r#enum, vec!["bytes", "milliseconds"]);
    }

    #[test]
    fn test_schema_step_size_default() {
        let schema = get_config_schema();
        let step = schema.iter().find(|f| f.name == "StepSize").unwrap();
        assert_eq!(step.default.as_ref().unwrap(), 22_020_096);
        assert_eq!(step.field_type, "uint64");
    }
}
