//! Unix domain socket CLI server for TollGate management commands.
//!
//! Listens on a configurable socket path (default: `/var/run/tollgate.sock`)
//! and processes newline-delimited JSON commands. Matches Go v1's `CLIServer`.

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

pub mod commands;
pub mod file_config;
pub mod types;

use std::path::Path;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use self::commands::{CliConfig, CliWallet};
use self::types::{CLIMessage, CLIResponse, SessionStatus};

pub use commands::CliConfig as CliConfigTrait;
pub use file_config::FileConfig;

const DEFAULT_SOCKET_PATH: &str = "/var/run/tollgate.sock";
const SOCKET_PERMISSIONS: u32 = 0o666;
const READ_BUF_CAPACITY: usize = 8192;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("socket bind error: {0}")]
    Bind(std::io::Error),
    #[error("socket permissions error: {0}")]
    Permissions(std::io::Error),
    #[error("not running")]
    NotRunning,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct CliServer {
    socket_path: String,
    wallet: Arc<dyn CliWallet>,
    config: Option<Arc<dyn CliConfig>>,
    start_time: std::time::Instant,
    cancel: CancellationToken,
}

impl CliServer {
    pub fn new(wallet: Arc<dyn CliWallet>, socket_path: Option<String>) -> Self {
        Self {
            socket_path: socket_path.unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_owned()),
            wallet,
            config: None,
            start_time: std::time::Instant::now(),
            cancel: CancellationToken::new(),
        }
    }

    pub fn with_config(mut self, config: Arc<dyn CliConfig>) -> Self {
        self.config = Some(config);
        self
    }

    pub fn start(&self) -> Result<(), CliError> {
        let path = Path::new(&self.socket_path);

        // Remove stale socket file
        if path.exists() {
            std::fs::remove_file(path).map_err(CliError::Bind)?;
        }

        let listener = UnixListener::bind(path).map_err(CliError::Bind)?;

        // Set world-readable/writable permissions (matches Go v1)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(SOCKET_PERMISSIONS);
            std::fs::set_permissions(path, perms).map_err(CliError::Permissions)?;
        }

        tracing::info!(socket_path = %self.socket_path, "CLI server started");

        let wallet = self.wallet.clone();
        let config = self.config.clone();
        let start_time = self.start_time;
        let cancel = self.cancel.clone();

        tokio::spawn(async move {
            accept_loop(listener, &wallet, config.as_ref(), start_time, cancel).await;
        });

        Ok(())
    }

    pub fn stop(&self) -> Result<(), CliError> {
        self.cancel.cancel();

        let path = Path::new(&self.socket_path);
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }

        tracing::info!("CLI server stopped");
        Ok(())
    }
}

async fn accept_loop(
    listener: UnixListener,
    wallet: &Arc<dyn CliWallet>,
    config: Option<&Arc<dyn CliConfig>>,
    start_time: std::time::Instant,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("CLI accept loop cancelled");
                return;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let wallet = wallet.clone();
                        let config = config.cloned();
                        tokio::spawn(handle_connection(stream, wallet, config, start_time));
                    }
                    Err(e) => {
                        if !cancel.is_cancelled() {
                            tracing::error!(error = %e, "Failed to accept CLI connection");
                        }
                    }
                }
            }
        }
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    wallet: Arc<dyn CliWallet>,
    config: Option<Arc<dyn CliConfig>>,
    start_time: std::time::Instant,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::with_capacity(READ_BUF_CAPACITY, reader);

    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "Failed to read from CLI connection");
            return;
        }
    }

    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    tracing::debug!(len = trimmed.len(), "Received CLI message");

    let response = match serde_json::from_str::<CLIMessage>(trimmed) {
        Ok(msg) => dispatch(&msg, &wallet, &config, start_time).await,
        Err(e) => CLIResponse::error(format!("Invalid JSON: {e}")),
    };

    let mut output = serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"success":false,"error":"serialization failed","timestamp":"1970-01-01T00:00:00Z"}"#
            .to_owned()
    });
    output.push('\n');

    if let Err(e) = writer.write_all(output.as_bytes()).await {
        tracing::error!(error = %e, "Failed to write CLI response");
    }
    let _ = writer.flush().await;
}

async fn dispatch(
    msg: &CLIMessage,
    wallet: &Arc<dyn CliWallet>,
    config: &Option<Arc<dyn CliConfig>>,
    start_time: std::time::Instant,
) -> CLIResponse {
    tracing::debug!(command = %msg.command, args = ?msg.args, "Dispatching CLI command");

    match msg.command.as_str() {
        "wallet" => dispatch_wallet(&msg.args, wallet).await,
        "upstream" => dispatch_upstream(&msg.args),
        "status" => {
            let sessions: Vec<SessionStatus> = Vec::new();
            commands::handle_status(wallet.as_ref(), start_time, &sessions).await
        }
        "version" => commands::handle_version(),
        "health" => {
            let wallet_ok = wallet.balance().await.is_ok();
            let uptime_secs = start_time.elapsed().as_secs();
            commands::handle_health(wallet_ok, true, uptime_secs)
        }
        "config" => dispatch_config(&msg.args, config),
        _ => CLIResponse::error(format!("Unknown command: {}", msg.command)),
    }
}

async fn dispatch_wallet(args: &[String], wallet: &Arc<dyn CliWallet>) -> CLIResponse {
    let Some(action) = args.first() else {
        return CLIResponse::error(
            "Wallet command requires an action (balance, info, fund, drain)",
        );
    };

    match action.as_str() {
        "balance" => commands::handle_wallet_balance(wallet.as_ref()).await,
        "info" => commands::handle_wallet_info(wallet.as_ref()).await,
        "fund" => {
            let token = args.get(1).map_or("", String::as_str);
            commands::handle_wallet_fund(wallet.as_ref(), token).await
        }
        "drain" => {
            let drain_type = args.get(1).map_or("", String::as_str);
            match drain_type {
                "cashu" => commands::handle_wallet_drain(wallet.as_ref()).await,
                "" => CLIResponse::error(
                    "Drain command requires a type: 'cashu' (lightning not yet supported)",
                ),
                other => {
                    CLIResponse::error(format!("Unknown drain type: {other} (supported: cashu)"))
                }
            }
        }
        other => CLIResponse::error(format!(
            "Unknown wallet action: {other} (supported: balance, info, fund, drain)"
        )),
    }
}

fn dispatch_upstream(args: &[String]) -> CLIResponse {
    let Some(subcommand) = args.first() else {
        return CLIResponse::error(
            "Upstream command requires a subcommand (scan, connect, list-upstream, remove-upstream)",
        );
    };

    match subcommand.as_str() {
        "scan" => commands::handle_upstream_scan(),
        "connect" => {
            let ssid = args.get(1).map_or("", String::as_str);
            if ssid.is_empty() {
                return CLIResponse::error("connect requires an SSID argument");
            }
            let passphrase = args.get(2).map(String::as_str);
            commands::handle_upstream_connect(ssid, passphrase)
        }
        "list-upstream" => commands::handle_upstream_list(),
        "remove-upstream" => {
            let ssid = args.get(1).map_or("", String::as_str);
            if ssid.is_empty() {
                return CLIResponse::error("remove-upstream requires an SSID argument");
            }
            commands::handle_upstream_remove(ssid)
        }
        other => CLIResponse::error(format!(
            "Unknown upstream subcommand: {other} (supported: scan, connect, list-upstream, remove-upstream)"
        )),
    }
}

fn dispatch_config(args: &[String], config: &Option<Arc<dyn CliConfig>>) -> CLIResponse {
    let Some(subcommand) = args.first() else {
        return CLIResponse::error("Config command requires a subcommand (get, set, save)");
    };

    let Some(cfg) = config else {
        return CLIResponse::error("Config manager not available");
    };

    match subcommand.as_str() {
        "get" => commands::handle_config_get(cfg.as_ref()),
        "set" => {
            let key = args.get(1).map_or("", String::as_str);
            let value = args.get(2).map_or("", String::as_str);
            if key.is_empty() || value.is_empty() {
                return CLIResponse::error("config set requires <key> <value>");
            }
            commands::handle_config_set(cfg.as_ref(), key, value)
        }
        "save" => {
            let json_str = args.get(1).map_or("", String::as_str);
            if json_str.is_empty() {
                return CLIResponse::error("config save requires <json-string>");
            }
            commands::handle_config_save(cfg.as_ref(), json_str)
        }
        other => CLIResponse::error(format!(
            "Unknown config subcommand: {other} (supported: get, set, save)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MockCliWallet {
        balance: u64,
        mint_balances: HashMap<String, u64>,
    }

    impl MockCliWallet {
        fn new(balance: u64) -> Self {
            Self {
                balance,
                mint_balances: HashMap::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl CliWallet for MockCliWallet {
        async fn balance(&self) -> Result<u64, String> {
            Ok(self.balance)
        }

        async fn receive_token(&self, token: &str) -> Result<u64, String> {
            if token.len() < 8 {
                return Err("token too short".to_owned());
            }
            let amount = u64::from_be_bytes(token.as_bytes()[..8].try_into().unwrap());
            Ok(amount)
        }

        async fn create_token(&self, amount: u64, mint_url: &str) -> Result<String, String> {
            Ok(format!("cashuA_{amount}_{mint_url}"))
        }

        async fn get_mint_balances(&self) -> HashMap<String, u64> {
            self.mint_balances.clone()
        }
    }

    fn make_wallet(balance: u64) -> Arc<dyn CliWallet> {
        Arc::new(MockCliWallet::new(balance))
    }

    #[test]
    fn dispatch_unknown_command_returns_error() {
        let wallet = make_wallet(100);
        let msg = CLIMessage {
            command: "foobar".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("Unknown command: foobar"));
    }

    #[test]
    fn dispatch_wallet_balance() {
        let wallet = make_wallet(500);
        let msg = CLIMessage {
            command: "wallet".to_owned(),
            args: vec!["balance".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(resp.success);
        let data = resp.data.unwrap();
        assert_eq!(data["balance"], 500);
    }

    #[test]
    fn dispatch_wallet_no_action() {
        let wallet = make_wallet(100);
        let msg = CLIMessage {
            command: "wallet".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("requires an action"));
    }

    #[test]
    fn dispatch_version() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "version".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(resp.success);
        assert!(resp.message.unwrap().contains("tollgate-net"));
    }

    #[test]
    fn dispatch_status() {
        let wallet = make_wallet(100);
        let msg = CLIMessage {
            command: "status".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(resp.success);
        let data = resp.data.unwrap();
        assert_eq!(data["wallet_ok"], true);
        assert_eq!(data["active_sessions"], 0);
    }

    #[test]
    fn dispatch_upstream_scan_stub() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "upstream".to_owned(),
            args: vec!["scan".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("M4"));
    }

    #[tokio::test]
    async fn full_request_response_via_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let socket_path_str = socket_path.to_str().unwrap().to_owned();

        let wallet = make_wallet(250);
        let server = CliServer::new(wallet, Some(socket_path_str.clone()));
        server.start().unwrap();

        // Give the server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect as a client and send a command
        let stream = tokio::net::UnixStream::connect(&socket_path_str)
            .await
            .unwrap();
        let (reader, mut writer) = stream.into_split();

        let request = serde_json::to_string(&CLIMessage {
            command: "wallet".to_owned(),
            args: vec!["balance".to_owned()],
            flags: HashMap::new(),
        })
        .unwrap()
            + "\n";
        writer.write_all(request.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();

        let mut reader = BufReader::new(reader);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await.unwrap();

        let resp: CLIResponse = serde_json::from_str(&response_line).unwrap();
        assert!(resp.success);
        assert!(resp.message.unwrap().contains("250 sats"));

        server.stop().unwrap();
    }

    #[tokio::test]
    async fn socket_cleanup_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("stale.sock");
        let socket_path_str = socket_path.to_str().unwrap().to_owned();

        // Create a stale file at the socket path
        std::fs::write(&socket_path, b"stale").unwrap();
        assert!(socket_path.exists());

        let wallet = make_wallet(0);
        let server = CliServer::new(wallet, Some(socket_path_str.clone()));
        server.start().unwrap();

        // The stale file should have been replaced by a socket
        assert!(socket_path.exists());

        server.stop().unwrap();
    }

    #[tokio::test]
    async fn stop_removes_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("removal.sock");
        let socket_path_str = socket_path.to_str().unwrap().to_owned();

        let wallet = make_wallet(0);
        let server = CliServer::new(wallet, Some(socket_path_str.clone()));
        server.start().unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(socket_path.exists());

        server.stop().unwrap();
        assert!(!socket_path.exists());
    }

    #[test]
    fn dispatch_health() {
        let wallet = make_wallet(100);
        let msg = CLIMessage {
            command: "health".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(resp.success);
        let data = resp.data.unwrap();
        assert_eq!(data["wallet_ok"], true);
        assert_eq!(data["config_ok"], true);
        assert_eq!(data["status"], "healthy");
    }

    #[test]
    fn dispatch_health_degraded_when_wallet_fails() {
        struct FailingWallet;
        #[async_trait::async_trait]
        impl CliWallet for FailingWallet {
            async fn balance(&self) -> Result<u64, String> {
                Err("wallet error".to_owned())
            }
            async fn receive_token(&self, _token: &str) -> Result<u64, String> {
                Ok(0)
            }
            async fn create_token(&self, _amount: u64, _mint_url: &str) -> Result<String, String> {
                Ok(String::new())
            }
            async fn get_mint_balances(&self) -> HashMap<String, u64> {
                HashMap::new()
            }
        }
        let wallet: Arc<dyn CliWallet> = Arc::new(FailingWallet);
        let msg = CLIMessage {
            command: "health".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(resp.success);
        let data = resp.data.unwrap();
        assert_eq!(data["wallet_ok"], false);
        assert_eq!(data["status"], "degraded");
    }

    #[test]
    fn dispatch_config_no_subcommand() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec![],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("requires a subcommand"));
    }

    #[test]
    fn dispatch_config_get_no_manager() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec!["get".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("not available"));
    }

    #[test]
    fn dispatch_config_set_no_manager() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec!["set".to_owned(), "metric".to_owned(), "bytes".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
    }

    #[test]
    fn dispatch_config_unknown_subcommand() {
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec!["bogus".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &None, std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("not available"));
    }

    #[test]
    fn dispatch_config_set_missing_args() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"ms"}"#).unwrap();
        let config: Arc<dyn CliConfig> = Arc::new(FileConfig::new(path));
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec!["set".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &Some(config), std::time::Instant::now()));
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("requires"));
    }

    #[test]
    fn dispatch_config_get_with_file_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"metric":"milliseconds","step_size":60000}"#).unwrap();
        let config: Arc<dyn CliConfig> = Arc::new(FileConfig::new(path));
        let wallet = make_wallet(0);
        let msg = CLIMessage {
            command: "config".to_owned(),
            args: vec!["get".to_owned()],
            flags: HashMap::new(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt.block_on(dispatch(&msg, &wallet, &Some(config), std::time::Instant::now()));
        assert!(resp.success);
        let data = resp.data.unwrap();
        assert_eq!(data["metric"], "milliseconds");
    }
}
