//! Command handlers for the CLI server.
//!
//! Each handler takes the relevant inputs and returns a [`CLIResponse`].
//! Wallet operations go through the dyn-compatible [`CliWallet`] trait.

use std::collections::HashMap;

use super::types::{
    CLIResponse, FundResult, ServiceStatus, SessionStatus, WalletDetail, WalletInfo,
};

/// Dyn-compatible wallet operations for the CLI.
///
/// The core [`Wallet`](tollgate_core::wallet::Wallet) trait uses `impl Future` returns
/// and cannot be made into a trait object. This wrapper provides the same operations
/// with `Boxed` futures so the CLI server can hold `Arc<dyn CliWallet>`.
#[async_trait::async_trait]
pub trait CliWallet: Send + Sync {
    async fn balance(&self) -> Result<u64, String>;
    async fn receive_token(&self, token: &str) -> Result<u64, String>;
    async fn create_token(&self, amount: u64, mint_url: &str) -> Result<String, String>;
    async fn get_mint_balances(&self) -> HashMap<String, u64>;
}

pub async fn handle_wallet_balance(wallet: &dyn CliWallet) -> CLIResponse {
    match wallet.balance().await {
        Ok(balance) => CLIResponse::ok_with_data(
            format!("Total wallet balance: {balance} sats"),
            serde_json::to_value(WalletInfo { balance }).unwrap(),
        ),
        Err(e) => CLIResponse::error(format!("Failed to get balance: {e}")),
    }
}

pub async fn handle_wallet_info(wallet: &dyn CliWallet) -> CLIResponse {
    let total = match wallet.balance().await {
        Ok(b) => b,
        Err(e) => return CLIResponse::error(format!("Failed to get balance: {e}")),
    };

    let mint_balances = wallet.get_mint_balances().await;
    let nonzero: HashMap<String, u64> = mint_balances.into_iter().filter(|(_, v)| *v > 0).collect();
    let mint_count = nonzero.len();

    let detail = WalletDetail {
        total_balance: total,
        mint_count,
        mint_balances: nonzero,
    };

    CLIResponse::ok_with_data(
        format!("Wallet info - Total: {total} sats across {mint_count} mints"),
        serde_json::to_value(detail).unwrap(),
    )
}

pub async fn handle_wallet_fund(wallet: &dyn CliWallet, token: &str) -> CLIResponse {
    if token.is_empty() {
        return CLIResponse::error("Cashu token cannot be empty");
    }

    tracing::debug!(token_len = token.len(), "Attempting to fund wallet");

    match wallet.receive_token(token).await {
        Ok(amount) => {
            tracing::info!(amount, "Successfully funded wallet");
            CLIResponse::ok_with_data(
                format!("Successfully funded wallet with {amount} sats"),
                serde_json::to_value(FundResult {
                    amount_received: amount,
                })
                .unwrap(),
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fund wallet");
            CLIResponse::error(format!("Failed to fund wallet: {e}"))
        }
    }
}

pub async fn handle_wallet_drain(wallet: &dyn CliWallet) -> CLIResponse {
    let mint_balances = wallet.get_mint_balances().await;
    let nonzero: Vec<&str> = mint_balances
        .iter()
        .filter(|(_, &v)| v > 0)
        .map(|(url, _)| url.as_str())
        .collect();

    if nonzero.is_empty() {
        return CLIResponse::ok("No tokens to drain - all mint balances are zero");
    }

    let mut tokens = Vec::new();
    let mut total_drained: u64 = 0;
    let mut errors = Vec::new();

    for mint_url in &nonzero {
        let balance = mint_balances[*mint_url];
        match wallet.create_token(balance, mint_url).await {
            Ok(token_str) => {
                tracing::info!(mint = %mint_url, balance, "Created drain token");
                tokens.push(serde_json::json!({
                    "mint_url": mint_url,
                    "balance": balance,
                    "token": token_str,
                }));
                total_drained += balance;
            }
            Err(e) => {
                tracing::error!(mint = %mint_url, balance, error = %e, "Failed to drain mint");
                errors.push(format!("{mint_url}: {e}"));
            }
        }
    }

    if tokens.is_empty() {
        return CLIResponse::error(format!("Failed to drain all mints: {}", errors.join(", ")));
    }

    let msg = if errors.is_empty() {
        format!(
            "Successfully drained {total_drained} sats from {} mints",
            tokens.len()
        )
    } else {
        format!(
            "Drained {total_drained} sats from {} mints ({} failed)",
            tokens.len(),
            errors.len()
        )
    };

    CLIResponse::ok_with_data(
        msg,
        serde_json::json!({
            "tokens": tokens,
            "total_sats": total_drained,
        }),
    )
}

pub fn handle_version() -> CLIResponse {
    CLIResponse::ok(format!("tollgate-net v{}", env!("CARGO_PKG_VERSION")))
}

pub async fn handle_status(
    wallet: &dyn CliWallet,
    start_time: std::time::Instant,
    sessions: &[SessionStatus],
) -> CLIResponse {
    let wallet_ok = wallet.balance().await.is_ok();
    let status = ServiceStatus {
        running: true,
        uptime_secs: start_time.elapsed().as_secs(),
        wallet_ok,
        active_sessions: sessions.len(),
        version: format!("tollgate-net v{}", env!("CARGO_PKG_VERSION")),
    };

    CLIResponse::ok_with_data(
        "Service status retrieved",
        serde_json::to_value(status).unwrap(),
    )
}

pub fn handle_upstream_scan() -> CLIResponse {
    CLIResponse::error("WiFi scanning not implemented (requires M4)")
}

pub fn handle_upstream_connect(_ssid: &str, _passphrase: Option<&str>) -> CLIResponse {
    CLIResponse::error("WiFi connect not implemented (requires M4)")
}

pub fn handle_upstream_list() -> CLIResponse {
    CLIResponse::error("Upstream list not implemented (requires M4)")
}

pub fn handle_upstream_remove(_ssid: &str) -> CLIResponse {
    CLIResponse::error("Upstream remove not implemented (requires M4)")
}

/// Dyn-compatible config operations for the CLI.
pub trait CliConfig: Send + Sync {
    /// Get the full config as a JSON value.
    fn get_config(&self) -> Result<serde_json::Value, String>;
    /// Set a single config value by dot-path key (e.g. "metric", "step_size").
    fn set_value(&self, key: &str, value: &str) -> Result<(), String>;
    /// Save entire config from a JSON string, with validation.
    fn save_config(&self, json: &str) -> Result<(), String>;
}

/// Health check — lighter than status, matches Go's health endpoint.
pub fn handle_health(wallet_ok: bool, config_ok: bool, uptime_secs: u64) -> CLIResponse {
    let status = if wallet_ok && config_ok {
        "healthy"
    } else {
        "degraded"
    };
    CLIResponse::ok_with_data(
        format!("Service health: {status}"),
        serde_json::json!({
            "status": status,
            "version": format!("tollgate-net v{}", env!("CARGO_PKG_VERSION")),
            "config_ok": config_ok,
            "wallet_ok": wallet_ok,
            "uptime_secs": uptime_secs,
        }),
    )
}

/// Retrieve the current configuration as JSON.
pub fn handle_config_get(config: &dyn CliConfig) -> CLIResponse {
    match config.get_config() {
        Ok(cfg) => CLIResponse::ok_with_data("Configuration retrieved", cfg),
        Err(e) => CLIResponse::error(format!("Failed to get config: {e}")),
    }
}

/// Set a single config value by key.
pub fn handle_config_set(config: &dyn CliConfig, key: &str, value: &str) -> CLIResponse {
    match config.set_value(key, value) {
        Ok(()) => CLIResponse::ok_with_data(
            format!("Set {key} = {value} (restart tollgate-wrt to apply)"),
            serde_json::json!({"key": key, "value": value}),
        ),
        Err(e) => CLIResponse::error(format!("Failed to set {key}: {e}")),
    }
}

/// Replace the entire config file from a validated JSON string.
pub fn handle_config_save(config: &dyn CliConfig, json: &str) -> CLIResponse {
    match config.save_config(json) {
        Ok(()) => CLIResponse::ok("Configuration saved (restart tollgate-wrt to apply)"),
        Err(e) => CLIResponse::error(format!("Failed to save config: {e}")),
    }
}
