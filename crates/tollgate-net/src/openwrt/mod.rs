//! OpenWrt platform integration.
//!
//! Feature-gated behind `openwrt`. Provides UCI config operations, ubus RPC,
//! WiFi scanning/connecting, and network monitoring specific to OpenWrt.

pub mod network_monitor;
pub mod uci_ops;
pub mod ubus_client;
pub mod wifi_scanner;
pub mod wifi_connector;
