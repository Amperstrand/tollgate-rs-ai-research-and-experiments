//! CLI message and response types for the Unix domain socket server.

use serde::{Deserialize, Serialize};

/// Incoming CLI command.
#[derive(Debug, Deserialize, Serialize)]
pub struct CLIMessage {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub flags: std::collections::HashMap<String, String>,
}

/// Response sent back to CLI client.
#[derive(Debug, Serialize, Deserialize)]
pub struct CLIResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    pub timestamp: String,
}

impl CLIResponse {
    /// Successful response with a message.
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            error: None,
            data: None,
            progress: None,
            timestamp: iso_now(),
        }
    }

    /// Successful response with message and JSON data payload.
    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            error: None,
            data: Some(data),
            progress: None,
            timestamp: iso_now(),
        }
    }

    /// Error response.
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            success: false,
            message: None,
            error: Some(error.into()),
            data: None,
            progress: None,
            timestamp: iso_now(),
        }
    }

    /// Progress update (for streaming commands in future).
    pub fn progress(step: impl Into<String>) -> Self {
        Self {
            success: true,
            message: None,
            error: None,
            data: None,
            progress: Some(step.into()),
            timestamp: iso_now(),
        }
    }
}

/// Wallet balance summary.
#[derive(Debug, Serialize)]
pub struct WalletInfo {
    pub balance: u64,
}

/// Detailed wallet info with per-mint breakdown.
#[derive(Debug, Serialize)]
pub struct WalletDetail {
    pub total_balance: u64,
    pub mint_count: usize,
    pub mint_balances: std::collections::HashMap<String, u64>,
}

/// Result of funding the wallet with a token.
#[derive(Debug, Serialize)]
pub struct FundResult {
    pub amount_received: u64,
}

/// Overall service status.
#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub uptime_secs: u64,
    pub wallet_ok: bool,
    pub active_sessions: usize,
    pub version: String,
}

/// Status of a single active upstream session.
#[derive(Debug, Serialize)]
pub struct SessionStatus {
    pub gateway_ip: String,
    pub interface: String,
    pub allotment: u64,
    pub metric: String,
    pub spent_sats: u64,
    pub payments: u32,
}

/// Produce an ISO 8601 timestamp using only `std::time`.
///
/// Uses `SystemTime::now` and formats manually to avoid pulling in `chrono`.
fn iso_now() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Gregorian calendar calculation (good enough for timestamps, not date arithmetic)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Calculate year/month/day from days since epoch
    let (year, month, day) = days_to_date(days);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0u64;
    for (i, &md) in month_days.iter().enumerate() {
        if days < md {
            month = i as u64 + 1;
            break;
        }
        days -= md;
    }
    if month == 0 {
        month = 12;
    }

    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cli_message_minimal() {
        let msg: CLIMessage =
            serde_json::from_str(r#"{"command":"status","args":[],"flags":{}}"#).unwrap();
        assert_eq!(msg.command, "status");
        assert!(msg.args.is_empty());
        assert!(msg.flags.is_empty());
    }

    #[test]
    fn parse_cli_message_defaults() {
        let msg: CLIMessage = serde_json::from_str(r#"{"command":"version"}"#).unwrap();
        assert_eq!(msg.command, "version");
        assert!(msg.args.is_empty());
        assert!(msg.flags.is_empty());
    }

    #[test]
    fn parse_cli_message_with_args_and_flags() {
        let msg: CLIMessage = serde_json::from_str(
            r#"{"command":"wallet","args":["fund","cashuA..."],"flags":{"save":"yes"}}"#,
        )
        .unwrap();
        assert_eq!(msg.command, "wallet");
        assert_eq!(msg.args, vec!["fund", "cashuA..."]);
        assert_eq!(msg.flags.get("save").unwrap(), "yes");
    }

    #[test]
    fn serialize_cli_response_ok() {
        let resp = CLIResponse::ok("hello");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"message\":\"hello\""));
        assert!(!json.contains("\"error\""));
        assert!(!json.contains("\"data\""));
        assert!(!json.contains("\"progress\""));
    }

    #[test]
    fn serialize_cli_response_error() {
        let resp = CLIResponse::error("bad request");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"bad request\""));
        assert!(!json.contains("\"message\""));
    }

    #[test]
    fn serialize_cli_response_with_data() {
        let resp = CLIResponse::ok_with_data("balance", serde_json::json!({"balance": 42}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"data\""));
        assert!(json.contains("\"balance\":42"));
    }

    #[test]
    fn iso_now_produces_valid_format() {
        let ts = iso_now();
        // Should look like 2025-01-15T12:30:45Z
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], "Z");
    }
}
