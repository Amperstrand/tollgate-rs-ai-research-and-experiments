//! Token recovery for failed upstream payments.
//!
//! When a payment to an upstream TollGate fails, the Cashu token may still be
//! reclaimable. This module implements a two-step recovery strategy:
//!
//! 1. Try `wallet.receive_token()` to reclaim the token back to our wallet.
//! 2. If that fails, append the token + metadata to a recovery file for manual
//!    processing by the operator.
//!
//! Ported from the experimental v1 archive into the v1-compat layer.
//! Uses concrete [`CdkWallet`](super::wallet::CdkWallet) instead of a
//! generic `W: Wallet`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::wallet::CdkWallet;

/// Default recovery file path.
pub const DEFAULT_RECOVERY_FILE: &str = "/etc/tollgate/tokens-to-recover.txt";

/// Result of a token recovery attempt.
#[derive(Debug)]
pub enum RecoveryResult {
    /// Token was automatically reclaimed via `wallet.receive_token()`.
    /// Contains the amount in sats.
    AutoRecovered(u64),
    /// Token saved to file for manual recovery.
    SavedToFile(PathBuf),
    /// Recovery failed entirely (auto-reclaim rejected, file write failed).
    Failed(String),
}

/// Attempt to recover a token after a failed upstream payment.
///
/// Strategy:
/// 1. Try `wallet.receive_token()` to reclaim the token.
/// 2. If that fails, save token + timestamp + mint_url to `recovery_file`.
/// 3. If the file write also fails, return [`RecoveryResult::Failed`].
pub async fn recover_failed_token(
    wallet: &CdkWallet,
    token: &[u8],
    mint_url: &str,
    error: &str,
    recovery_file: &Path,
) -> RecoveryResult {
    tracing::info!(%mint_url, %error, "Attempting token recovery");

    // Step 1: Try automatic reclaim via wallet.
    match wallet.receive_token(token).await {
        Ok(amount) => {
            tracing::info!(amount, "Token auto-recovered successfully");
            return RecoveryResult::AutoRecovered(amount);
        }
        Err(e) => {
            tracing::warn!(%e, "Auto-recovery failed, falling back to file");
        }
    }

    // Step 2: Append to recovery file for manual processing.
    match write_recovery_entry(recovery_file, token, mint_url, error).await {
        Ok(()) => {
            tracing::info!(path = %recovery_file.display(), "Token saved for manual recovery");
            RecoveryResult::SavedToFile(recovery_file.to_path_buf())
        }
        Err(io_err) => {
            let msg =
                format!("Recovery failed: auto-reclaim rejected, file write failed: {io_err}");
            tracing::error!(%msg);
            RecoveryResult::Failed(msg)
        }
    }
}

/// Append one recovery entry to the file.
async fn write_recovery_entry(
    path: &Path,
    token: &[u8],
    mint_url: &str,
    error: &str,
) -> std::io::Result<()> {
    let timestamp = format_rfc3339_now();
    let token_hex = hex_encode(token);
    let line = format!("{timestamp} | mint={mint_url} | error={error} | token={token_hex}\n");

    // Create parent directories if needed.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Format the current time as RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`) without chrono.
fn format_rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (year, month, day, hour, minute, second) = unix_to_calendar(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a Unix timestamp (seconds since epoch) to calendar components.
fn unix_to_calendar(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let days = secs / 86400;
    let remainder = secs % 86400;
    let hour = remainder / 3600;
    let minute = (remainder % 3600) / 60;
    let second = remainder % 60;

    let (year, day_of_year) = days_to_year_and_day(days);
    let (month, day) = day_of_year_to_month_day(day_of_year, is_leap_year(year));

    (year, month, day, hour, minute, second)
}

/// Convert a day-count since epoch to (year, day-of-year).
fn days_to_year_and_day(mut days: u64) -> (u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            return (year, days);
        }
        days -= days_in_year;
        year += 1;
    }
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Convert a zero-based day-of-year to (month, day) (both 1-based).
fn day_of_year_to_month_day(day_of_year: u64, leap: bool) -> (u64, u64) {
    let month_days: [u64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut remaining = day_of_year;
    for (i, &mdays) in month_days.iter().enumerate() {
        if remaining < mdays {
            return ((i as u64) + 1, remaining + 1);
        }
        remaining -= mdays;
    }
    (12, 31)
}

/// Hex-encode a byte slice for safe storage in text files.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
