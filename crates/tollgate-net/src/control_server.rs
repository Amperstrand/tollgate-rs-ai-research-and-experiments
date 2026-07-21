//! FIPS-style Unix control socket: serves the node status snapshot on request.
//!
//! Two protocols share one socket:
//! - **JSON CLIMessage** (Go v1-compatible): `{"command":"status"}\n` →
//!   `{"success":true,"data":...}\n` ([`CLIResponse`]).
//! - **Plain text** (legacy): `status\n` → raw [`NodeStatus`] JSON line. Kept
//!   for the existing `tolltop` client ([`crate::control`]) and any other legacy
//!   tooling. Unknown plain text commands get a JSON error line.
//!
//! The client half lives in the shared lib ([`tollgate_net::control`]).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::config::Config;
use crate::driver::Driver;

/// Go v1-compatible CLI request: a command plus optional positional args and
/// named flags. Deserialized from one JSON line.
#[derive(Deserialize)]
struct CLIMessage {
    command: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    flags: Option<HashMap<String, String>>,
}

/// Go v1-compatible CLI response. Always serialized as one JSON line.
#[derive(Serialize)]
struct CLIResponse {
    success: bool,
    message: Option<String>,
    data: Option<Value>,
    error: Option<String>,
}

impl CLIResponse {
    fn ok(data: Value) -> Self {
        Self {
            success: true,
            message: None,
            data: Some(data),
            error: None,
        }
    }

    fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            message: None,
            data: None,
            error: Some(msg.into()),
        }
    }
}

/// Bind `path` and serve status to every connection until the process exits.
/// Removes any stale socket file first. Errors are returned for the caller to log.
pub async fn serve(path: &Path, driver: Driver, config: Config) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding control socket {}", path.display()))?;
    tracing::info!(socket = %path.display(), "control socket listening");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let driver = driver.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, driver, config).await {
                        tracing::debug!(err = %e, "control connection ended");
                    }
                });
            }
            Err(e) => tracing::warn!(err = %e, "control socket accept failed"),
        }
    }
}

async fn handle(stream: UnixStream, driver: Driver, config: Config) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let response = process_line(&line, &driver, &config).await?;
        if !response.is_empty() {
            write.write_all(response.as_bytes()).await?;
            write.write_all(b"\n").await?;
        }
    }
    Ok(())
}

/// Process one input line and return the JSON response line (empty for no reply).
/// tries JSON CLIMessage first; on parse failure falls back to the legacy
/// plain-text protocol (backwards compatible with `tolltop`).
async fn process_line(line: &str, driver: &Driver, config: &Config) -> anyhow::Result<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    match serde_json::from_str::<CLIMessage>(trimmed) {
        Ok(msg) => {
            let resp = route(&msg, driver, config).await;
            Ok(serde_json::to_string(&resp)?)
        }
        Err(_) => {
            if trimmed == "status" {
                let status = driver.status().await;
                Ok(serde_json::to_string(&status)?)
            } else {
                Ok(json!({ "error": format!("unknown command: {trimmed}") }).to_string())
            }
        }
    }
}

/// Route a parsed CLIMessage to its handler and return a [`CLIResponse`].
async fn route(msg: &CLIMessage, driver: &Driver, config: &Config) -> CLIResponse {
    let args: &[String] = msg.args.as_deref().unwrap_or(&[]);
    let sub = args.first().map(String::as_str);
    match msg.command.as_str() {
        "status" => match serde_json::to_value(driver.status().await) {
            Ok(v) => CLIResponse::ok(v),
            Err(e) => CLIResponse::error(format!("serialize status: {e}")),
        },
        "version" => {
            let mut features: Vec<&str> = Vec::new();
            if cfg!(feature = "v1-compat") {
                features.push("v1-compat");
            }
            if cfg!(feature = "openwrt") {
                features.push("openwrt");
            }
            if cfg!(feature = "spilman") {
                features.push("spilman");
            }
            CLIResponse::ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "features": features,
            }))
        }
        "health" => CLIResponse::ok(json!({ "ok": true })),
        "wallet" => match sub {
            Some("balance") | None => CLIResponse::ok(json!({
                "balance": 0,
                "note": "wallet balance not available in current architecture"
            })),
            Some(other) => CLIResponse::error(format!("unknown wallet subcommand: {other}")),
        },
        "config" => match sub {
            Some("get") | None => match serde_json::to_value(config) {
                Ok(v) => CLIResponse::ok(v),
                Err(e) => CLIResponse::error(format!("serialize config: {e}")),
            },
            Some(other) => CLIResponse::error(format!("unknown config subcommand: {other}")),
        },
        other => CLIResponse::error(format!("unknown command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    use tollgate_core::Price;

    use crate::adapter::IpAdapter;
    use crate::wallet::BootstrapWallet;

    fn test_driver() -> Driver {
        let identity = Arc::new(crate::config::Identity::load_or_generate(&Config::default()).unwrap());
        Driver::new(
            BootstrapWallet::new(vec![]),
            IpAdapter::new(),
            identity,
            Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("response is valid JSON")
    }

    #[tokio::test]
    async fn empty_line_yields_no_response() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line("   ", &driver, &cfg).await.unwrap();
        assert!(out.is_empty(), "empty input must produce no reply");
    }

    #[tokio::test]
    async fn plain_text_status_returns_raw_node_status_json() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line("status", &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert!(v.get("pubkey").is_some(), "legacy status has NodeStatus shape");
        assert!(v.get("unit").is_some());
        assert!(v["peers"].is_array());
        assert!(v.get("success").is_none(), "legacy status is NOT a CLIResponse");
    }

    #[tokio::test]
    async fn plain_text_unknown_returns_legacy_error_form() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line("frobnicate", &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["error"], "unknown command: frobnicate");
        assert!(v.get("success").is_none(), "legacy error is NOT a CLIResponse");
    }

    #[tokio::test]
    async fn malformed_json_falls_back_to_plain_text_unknown() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line("{not json", &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["error"], "unknown command: {not json");
    }

    #[tokio::test]
    async fn json_status_returns_cliresponse_wrapping_node_status() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"status"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert!(v["data"]["pubkey"].is_string());
        assert!(v["data"]["peers"].is_array());
        assert!(v["data"]["unit"].is_string());
    }

    #[tokio::test]
    async fn json_version_returns_pkg_version_and_feature_list() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"version"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["version"], env!("CARGO_PKG_VERSION"));
        let features = v["data"]["features"].as_array().expect("features is array");
        if cfg!(feature = "v1-compat") {
            assert!(features.iter().any(|f| f == "v1-compat"));
        }
    }

    #[tokio::test]
    async fn json_health_returns_ok_true() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"health"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["ok"], true);
    }

    #[tokio::test]
    async fn json_wallet_balance_returns_stub() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(
            r#"{"command":"wallet","args":["balance"]}"#,
            &driver,
            &cfg,
        )
        .await
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["balance"], 0);
        assert!(v["data"]["note"].is_string());
    }

    #[tokio::test]
    async fn json_wallet_without_subcommand_also_returns_balance_stub() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"wallet"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["balance"], 0);
    }

    #[tokio::test]
    async fn json_wallet_unknown_subcommand_returns_error() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(
            r#"{"command":"wallet","args":["mine"]}"#,
            &driver,
            &cfg,
        )
        .await
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], false);
        assert_eq!(v["error"], "unknown wallet subcommand: mine");
    }

    #[tokio::test]
    async fn json_config_get_returns_serialized_config() {
        let driver = test_driver();
        let cfg = Config {
            unit: "wh".to_string(),
            listen: "0.0.0.0:9999".to_string(),
            ..Config::default()
        };
        let out = process_line(
            r#"{"command":"config","args":["get"]}"#,
            &driver,
            &cfg,
        )
        .await
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["unit"], "wh");
        assert_eq!(v["data"]["listen"], "0.0.0.0:9999");
    }

    #[tokio::test]
    async fn json_config_without_subcommand_also_returns_config() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"config"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], true);
        assert!(v["data"].get("listen").is_some());
    }

    #[tokio::test]
    async fn json_config_unknown_subcommand_returns_error() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(
            r#"{"command":"config","args":["set"]}"#,
            &driver,
            &cfg,
        )
        .await
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], false);
        assert_eq!(v["error"], "unknown config subcommand: set");
    }

    #[tokio::test]
    async fn json_unknown_command_returns_cliresponse_error() {
        let driver = test_driver();
        let cfg = Config::default();
        let out = process_line(r#"{"command":"frobnicate"}"#, &driver, &cfg).await.unwrap();
        let v = parse(&out);
        assert_eq!(v["success"], false);
        assert_eq!(v["error"], "unknown command: frobnicate");
    }

    #[tokio::test]
    async fn end_to_end_json_status_over_real_unix_socket() {
        let socket = std::env::temp_dir().join(format!(
            "tollgate-control-test-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&socket);

        let driver = test_driver();
        let cfg = Config::default();
        let server_socket = socket.clone();
        let server_driver = driver.clone();
        let server_cfg = cfg.clone();
        let server = tokio::spawn(async move {
            let _ = serve(&server_socket, server_driver, server_cfg).await;
        });

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(socket.exists(), "server bound the socket");

        let mut stream = UnixStream::connect(&socket).await.expect("connect");
        stream
            .write_all(b"{\"command\":\"health\"}\n")
            .await
            .expect("write");
        let mut buf = String::new();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut buf).await.expect("read");
        let v = parse(buf.trim());
        assert_eq!(v["success"], true);
        assert_eq!(v["data"]["ok"], true);

        server.abort();
        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test]
    async fn end_to_end_legacy_plain_text_status_over_real_unix_socket() {
        let socket = std::env::temp_dir().join(format!(
            "tollgate-control-legacy-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::remove_file(&socket);

        let driver = test_driver();
        let cfg = Config::default();
        let server_socket = socket.clone();
        let server_driver = driver.clone();
        let server_cfg = cfg.clone();
        let server = tokio::spawn(async move {
            let _ = serve(&server_socket, server_driver, server_cfg).await;
        });

        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(socket.exists(), "server bound the socket");

        let mut stream = UnixStream::connect(&socket).await.expect("connect");
        stream.write_all(b"status\n").await.expect("write");
        let mut buf = String::new();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut buf).await.expect("read");
        let v = parse(buf.trim());
        assert!(v.get("pubkey").is_some(), "legacy path returns raw NodeStatus");
        assert!(v.get("success").is_none());

        server.abort();
        let _ = std::fs::remove_file(&socket);
    }
}
