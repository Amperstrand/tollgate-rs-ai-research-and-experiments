#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::path::Path;

/// Initialize tracing subscriber based on config.
/// On OpenWrt: uses syslog via tracing-appender.
/// On other platforms: uses stdout with configurable level.
///
/// # Arguments
/// * `default_level` - Default log level (e.g., "info", "debug", "warn", "error")
///
/// # Behavior
/// * Respects `RUST_LOG` environment variable if set (overrides default)
/// * On OpenWrt: uses syslog output
/// * On other platforms: uses stdout with colors (dev mode)
pub fn init_logging(default_level: &str) {
    let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| default_level.to_owned());
    let on_openwrt = Path::new("/etc/openwrt_release").exists();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level));

    if on_openwrt {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
}
