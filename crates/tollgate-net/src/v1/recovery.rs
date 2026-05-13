//! Token recovery for failed upstream payments.
//!
//! When a payment to an upstream TollGate fails, the Cashu token may still be
//! reclaimable. This module implements a two-step recovery strategy:
//!
//! 1. Try `wallet.receive_token()` to reclaim the token back to our wallet.
//! 2. If that fails, append the token + metadata to a recovery file for manual
//!    processing by the operator.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tollgate_core::types::Amount;
use tollgate_core::wallet::Wallet;

/// Default recovery file path.
pub const DEFAULT_RECOVERY_FILE: &str = "/etc/tollgate/tokens-to-recover.txt";

/// Result of a token recovery attempt.
#[derive(Debug)]
pub enum RecoveryResult {
    /// Token was automatically reclaimed via `wallet.receive_token()`.
    AutoRecovered(Amount),
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
pub async fn recover_failed_token<W: Wallet>(
    wallet: &W,
    token: &[u8],
    mint_url: &str,
    error: &str,
    recovery_file: &Path,
) -> RecoveryResult {
    tracing::info!(%mint_url, %error, "Attempting token recovery");

    // Step 1: Try automatic reclaim via wallet.
    match wallet.receive_token(token).await {
        Ok(amount) => {
            tracing::info!(amount = amount.0, "Token auto-recovered successfully");
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
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.flush().await?;
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
        write!(s, "{b:02x}").unwrap();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockWallet;

    /// Helper: create a temp file path unique to this test process and name.
    fn test_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("tollgate-recovery-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("recovery.txt")
    }

    /// Helper: build a token that MockWallet will accept (>= 8 bytes, first 8 = amount > 0).
    fn make_valid_token(amount: u64) -> Vec<u8> {
        let mut token = amount.to_be_bytes().to_vec();
        token.extend_from_slice(b"recovery-test-data");
        token
    }

    #[tokio::test]
    async fn auto_recovery_succeeds_for_valid_token() {
        let wallet = MockWallet::new(0);
        let token = make_valid_token(100);

        let path = test_path("auto_recovery");

        let result = recover_failed_token(
            &wallet,
            &token,
            "https://mint.example.com",
            "payment timeout",
            &path,
        )
        .await;

        match result {
            RecoveryResult::AutoRecovered(amount) => assert_eq!(amount.0, 100),
            other => panic!("Expected AutoRecovered, got: {other:?}"),
        }

        // File should NOT have been created.
        assert!(
            !tokio::fs::try_exists(&path).await.unwrap(),
            "recovery file should not exist after auto-recovery"
        );
    }

    #[tokio::test]
    async fn saves_to_file_when_receive_fails() {
        let wallet = MockWallet::new(0);
        let token = vec![0u8; 4]; // Too short → receive_token returns Err

        let path = test_path("save_to_file");

        let result = recover_failed_token(
            &wallet,
            &token,
            "https://mint.example.com",
            "connection refused",
            &path,
        )
        .await;

        match &result {
            RecoveryResult::SavedToFile(p) => assert_eq!(p, &path),
            other => panic!("Expected SavedToFile, got: {other:?}"),
        }

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(contents.contains("mint=https://mint.example.com"));
        assert!(contents.contains("error=connection refused"));
        assert!(contents.contains("token="));
        assert!(contents.ends_with('\n'), "file should end with newline");
    }

    #[tokio::test]
    async fn fails_when_both_reclaim_and_file_write_fail() {
        let wallet = MockWallet::new(0);
        let token = vec![0u8; 4]; // Too short → receive fails

        // Create a file where we need a directory → write will fail.
        let base = std::env::temp_dir().join("tollgate-recovery-tests");
        let _ = std::fs::create_dir_all(&base);
        let blocker = base.join(format!("blocker-{}", std::process::id()));
        std::fs::write(&blocker, b"x").unwrap();
        let impossible_path = blocker.join("sub/recovery.txt");

        let result = recover_failed_token(
            &wallet,
            &token,
            "https://mint.example.com",
            "total failure",
            &impossible_path,
        )
        .await;

        match result {
            RecoveryResult::Failed(msg) => {
                assert!(
                    msg.contains("file write failed"),
                    "error message should mention file write: {msg}"
                );
            }
            other => panic!("Expected Failed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn file_entry_contains_rfc3339_timestamp() {
        let wallet = MockWallet::new(0);
        let token = vec![0u8; 4]; // Trigger file path

        let path = test_path("rfc3339");

        recover_failed_token(&wallet, &token, "https://mint.example.com", "err", &path).await;

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        // Format: "YYYY-MM-DDTHH:MM:SSZ | mint=..."
        let timestamp = contents.split(" | ").next().unwrap();
        assert!(timestamp.contains('T'), "RFC 3339 requires T separator");
        assert!(timestamp.ends_with('Z'), "RFC 3339 requires Z suffix");
        assert_eq!(
            timestamp.len(),
            20,
            "expected YYYY-MM-DDTHH:MM:SSZ (20 chars)"
        );
    }

    #[test]
    fn unix_to_calendar_known_value() {
        // 2024-01-01T00:00:00Z = 1704067200
        let (year, month, day, hour, minute, second) = unix_to_calendar(1_704_067_200);
        assert_eq!(year, 2024);
        assert_eq!(month, 1);
        assert_eq!(day, 1);
        assert_eq!(hour, 0);
        assert_eq!(minute, 0);
        assert_eq!(second, 0);
    }

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_bytes() {
        assert_eq!(hex_encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
    }
}
