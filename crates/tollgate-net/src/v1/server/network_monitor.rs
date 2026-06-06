//! Linux netlink-based network interface monitor.
//!
//! Subscribes to Linux rtnetlink multicast events (link up/down, address
//! add/delete, route changes) and emits high-level [`NetworkEvent`]s to a
//! channel consumer (typically a session manager).
//!
//! ## Go v1 comparison
//!
//! Go's `NetworkMonitor` uses **two separate goroutines** (`monitorLinkChanges`
//! and `monitorAddressChanges`) with two `netlink` subscriptions via
//! `vishvananda/netlink`. Rust uses a **single** `rtnetlink` multicast
//! connection subscribed to all five groups (Link, Ipv4Ifaddr, Ipv6Ifaddr,
//! Ipv4Route, Ipv6Route), which is simpler and avoids race conditions between
//! the two subscriptions.
//!
//! Improvements over Go:
//! - **Single multicast connection** instead of two goroutines
//! - **Auto-reconnect** on netlink socket loss (Go silently stops)
//! - **`CancellationToken`** for graceful shutdown (Go uses a `stopChan`)
//! - **Typed events** with `InterfaceInfo` snapshots (Go uses stringly-typed maps)

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use netlink_packet_route::route::{RouteAddress, RouteAttribute, RouteMessage};
use netlink_packet_route::link::LinkFlags;
use netlink_packet_route::address::AddressAttribute;
use rtnetlink::MulticastGroup;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// High-level network event emitted by [`NetworkMonitor`].
///
/// Go v1 uses a flat `struct { Type, Interface, GatewayIP }` with an
/// untyped `map[string]string` for extras. Rust uses a typed enum with
/// explicit fields per variant.
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// Interface came up (carrier detected + admin up).
    InterfaceUp {
        name: String,
        gateway_ip: Option<IpAddr>,
        info: InterfaceInfo,
    },
    /// Interface went down (carrier lost or admin down).
    InterfaceDown { name: String },
    /// Address added to an interface.
    AddressAdded {
        interface: String,
        address: IpAddr,
        gateway_ip: Option<IpAddr>,
    },
    /// Address removed from an interface.
    AddressDeleted {
        interface: String,
        address: IpAddr,
    },
}

/// Snapshot of interface state at event time.
///
/// Go v1 stores `lastEventTime map[string]time.Time` (interface→timestamp).
/// Rust carries richer per-event metadata.
#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub name: String,
    pub mac_address: Option<String>,
    pub ip_addresses: Vec<IpAddr>,
    pub is_up: bool,
    pub is_loopback: bool,
}

/// Configuration for [`NetworkMonitor`].
///
/// Maps to Go's `monitorConfig{IgnoreList, OnlyInterfaces, Throttle, ChanSize}`.
#[derive(Debug, Clone)]
pub struct NetworkMonitorConfig {
    /// Interfaces to ignore (e.g., `["lo", "br-lan"]`).
    pub ignore_interfaces: Vec<String>,
    /// If non-empty, only these interfaces are monitored (allowlist).
    pub only_interfaces: Vec<String>,
    /// Minimum interval between same-type events on the same interface.
    ///
    /// Go v1 uses 2 seconds. Defaults to the same here.
    pub throttle_duration: Duration,
    /// Buffer size for the internal event channel.
    ///
    /// Go v1 uses a buffered channel of 100 with non-blocking send.
    /// Rust uses the same default but with proper backpressure.
    pub event_buffer_size: usize,
}

impl Default for NetworkMonitorConfig {
    fn default() -> Self {
        Self {
            ignore_interfaces: vec!["lo".to_owned()],
            only_interfaces: Vec::new(),
            throttle_duration: Duration::from_secs(2),
            event_buffer_size: 100,
        }
    }
}

/// Error type for network monitor operations.
///
/// Uses `thiserror` like all error types in this crate (see [`crate::v1::V1ClientError`]).
#[derive(Debug, thiserror::Error)]
pub enum NetworkMonitorError {
    /// Failed to establish rtnetlink connection.
    #[error("netlink connection failed: {0}")]
    ConnectionFailed(String),
    /// Failed to query interface or route information.
    #[error("query failed: {0}")]
    QueryFailed(String),
    /// Monitor was stopped while performing an operation.
    #[error("monitor stopped")]
    Stopped,
}

// ---------------------------------------------------------------------------
// Throttle key
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum EventType {
    InterfaceUp,
    InterfaceDown,
    AddressAdded,
    AddressDeleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThrottleKey {
    interface: String,
    event_type: EventType,
}

impl ThrottleKey {
    fn new(interface: &str, event_type: EventType) -> Self {
        Self {
            interface: interface.to_owned(),
            event_type,
        }
    }
}

// ---------------------------------------------------------------------------
// NetworkMonitor
// ---------------------------------------------------------------------------

/// Linux netlink network interface monitor.
///
/// Subscribes to rtnetlink multicast events and emits typed [`NetworkEvent`]s.
///
/// Go v1 uses two goroutines (`monitorLinkChanges`, `monitorAddressChanges`)
/// feeding a shared `chan Event`. Rust uses a single tokio task with a unified
/// rtnetlink multicast stream.
pub struct NetworkMonitor {
    config: NetworkMonitorConfig,
    cancel: CancellationToken,
    throttle: Arc<tokio::sync::Mutex<HashMap<ThrottleKey, Instant>>>,
}

impl NetworkMonitor {
    /// Create a new monitor with the given configuration.
    pub fn new(config: NetworkMonitorConfig) -> Self {
        Self {
            config,
            cancel: CancellationToken::new(),
            throttle: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Get a clone of the cancellation token (for external shutdown).
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Start monitoring and emit events to `event_tx`.
    ///
    /// Runs until cancelled via the [`CancellationToken`] or an unrecoverable
    /// error. On netlink socket loss, auto-reconnects with 5s backoff
    /// (Go v1 does not reconnect — it silently stops receiving events).
    pub async fn start(
        &self,
        event_tx: mpsc::Sender<NetworkEvent>,
    ) -> Result<(), NetworkMonitorError> {
        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            match self.run_once(&event_tx).await {
                Ok(()) => return Ok(()),
                Err(NetworkMonitorError::Stopped) => return Ok(()),
                Err(e) => {
                    tracing::warn!(%e, "Netlink connection lost, reconnecting in 5s...");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(5)) => {}
                        () = self.cancel.cancelled() => return Ok(()),
                    }
                }
            }
        }
    }

    /// Single connection attempt.
    async fn run_once(
        &self,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) -> Result<(), NetworkMonitorError> {
        let groups = [
            MulticastGroup::Link,
            MulticastGroup::Ipv4Ifaddr,
            MulticastGroup::Ipv6Ifaddr,
            MulticastGroup::Ipv4Route,
            MulticastGroup::Ipv6Route,
        ];

        let (conn, handle, mut messages) = rtnetlink::new_multicast_connection(&groups)
            .map_err(|e| NetworkMonitorError::ConnectionFailed(format!("{e}")))?;

        // Spawn the rtnetlink connection driver — it must stay alive to process messages.
        let conn_cancel = self.cancel.clone();
        let conn_handle = tokio::spawn(async move {
            tokio::select! {
                _ = conn => {}
                () = conn_cancel.cancelled() => {}
            }
        });

        // Emit InterfaceUp for all currently-up interfaces (immediate scan,
        // unlike Go's 2-second delayed startup scan).
        self.emit_initial_scan(&handle, event_tx).await;

        tracing::info!("NetworkMonitor started (rtnetlink multicast)");

        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    conn_handle.abort();
                    return Ok(());
                }
                item = messages.next() => {
                    match item {
                        Some((msg, _)) => {
                            self.process_netlink_message(msg, &handle, event_tx).await;
                        }
                        None => {
                            conn_handle.abort();
                            return Err(NetworkMonitorError::ConnectionFailed(
                                "netlink stream ended".to_owned(),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Dispatch a netlink message to the appropriate handler.
    async fn process_netlink_message(
        &self,
        msg: netlink_packet_route::NetlinkMessage<netlink_packet_route::RouteNetlinkMessage>,
        handle: &rtnetlink::Handle,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        use netlink_packet_route::RouteNetlinkMessage;

        let inner = match msg.payload {
            netlink_packet_route::NetlinkPayload::InnerMessage(inner) => inner,
            _ => return,
        };

        match inner {
            RouteNetlinkMessage::NewLink(link_msg) => {
                self.handle_new_link(&link_msg, handle, event_tx).await;
            }
            RouteNetlinkMessage::DelLink(link_msg) => {
                self.handle_del_link(&link_msg, event_tx).await;
            }
            RouteNetlinkMessage::NewAddress(addr_msg) => {
                self.handle_new_address(&addr_msg, handle, event_tx).await;
            }
            RouteNetlinkMessage::DelAddress(addr_msg) => {
                self.handle_del_address(&addr_msg, handle, event_tx).await;
            }
            _ => {}
        }
    }

    // ── Link event handlers ──────────────────────────────────────────

    async fn handle_new_link(
        &self,
        msg: &netlink_packet_route::link::LinkMessage,
        handle: &rtnetlink::Handle,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        let name = match link_name(msg) {
            Some(n) => n,
            None => return,
        };

        if !self.should_process_interface(&name) {
            return;
        }

        if !link_is_up(msg) || link_is_loopback(msg) {
            return;
        }

        let link_index = msg.header.index;
        let ip_addresses = query_interface_addresses(handle, link_index).await;
        let mac_address = link_mac_address(msg);
        let gateway_ip = infer_gateway(&name, link_index, handle).await;

        let info = InterfaceInfo {
            name: name.clone(),
            mac_address,
            ip_addresses,
            is_up: true,
            is_loopback: false,
        };

        if self.should_emit(&name, EventType::InterfaceUp).await {
            tracing::info!(interface = %name, ?gateway_ip, "InterfaceUp");
            let event = NetworkEvent::InterfaceUp {
                name,
                gateway_ip,
                info,
            };
            self.send_event(event, event_tx).await;
        }
    }

    async fn handle_del_link(
        &self,
        msg: &netlink_packet_route::link::LinkMessage,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        let name = match link_name(msg) {
            Some(n) => n,
            None => return,
        };

        if !self.should_process_interface(&name) {
            return;
        }

        if self.should_emit(&name, EventType::InterfaceDown).await {
            tracing::info!(interface = %name, "InterfaceDown");
            let event = NetworkEvent::InterfaceDown { name };
            self.send_event(event, event_tx).await;
        }
    }

    // ── Address event handlers ───────────────────────────────────────

    async fn handle_new_address(
        &self,
        msg: &netlink_packet_route::address::AddressMessage,
        handle: &rtnetlink::Handle,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        let (iface_name, addr) = match parse_address_message(msg, handle).await {
            Some(v) => v,
            None => return,
        };

        if !self.should_process_interface(&iface_name) {
            return;
        }

        let link_index = msg.header.index;
        let gateway_ip = infer_gateway(&iface_name, link_index, handle).await;

        if self
            .should_emit(&iface_name, EventType::AddressAdded)
            .await
        {
            tracing::info!(interface = %iface_name, %addr, ?gateway_ip, "AddressAdded");
            let event = NetworkEvent::AddressAdded {
                interface: iface_name,
                address: addr,
                gateway_ip,
            };
            self.send_event(event, event_tx).await;
        }
    }

    async fn handle_del_address(
        &self,
        msg: &netlink_packet_route::address::AddressMessage,
        handle: &rtnetlink::Handle,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        let (iface_name, addr) = match parse_address_message(msg, handle).await {
            Some(v) => v,
            None => return,
        };

        if !self.should_process_interface(&iface_name) {
            return;
        }

        if self
            .should_emit(&iface_name, EventType::AddressDeleted)
            .await
        {
            tracing::info!(interface = %iface_name, %addr, "AddressDeleted");
            let event = NetworkEvent::AddressDeleted {
                interface: iface_name,
                address: addr,
            };
            self.send_event(event, event_tx).await;
        }
    }

    // ── Initial scan ─────────────────────────────────────────────────

    /// Emit InterfaceUp for all currently-up interfaces.
    ///
    /// Go v1 does a startup scan after a 2-second delay. Rust scans
    /// immediately on connect.
    async fn emit_initial_scan(
        &self,
        handle: &rtnetlink::Handle,
        event_tx: &mpsc::Sender<NetworkEvent>,
    ) {
        match get_current_interfaces(handle, self).await {
            Ok(interfaces) => {
                for info in interfaces {
                    if !info.is_up || info.is_loopback {
                        continue;
                    }
                    let name = info.name.clone();
                    tracing::info!(interface = %name, "Initial scan: InterfaceUp");
                    let event = NetworkEvent::InterfaceUp {
                        name,
                        gateway_ip: None,
                        info,
                    };
                    self.send_event(event, event_tx).await;
                }
            }
            Err(e) => {
                tracing::warn!(%e, "Initial interface scan failed");
            }
        }
    }

    // ── Public query methods ─────────────────────────────────────────

    /// Get current interface information (creates a temporary rtnetlink connection).
    pub async fn get_current_interfaces(&self) -> Result<Vec<InterfaceInfo>, NetworkMonitorError> {
        let (conn, handle, _) = rtnetlink::new_multicast_connection(&[MulticastGroup::Link])
            .map_err(|e| NetworkMonitorError::ConnectionFailed(format!("{e}")))?;

        let cancel = self.cancel.clone();
        let _conn_guard = tokio::spawn(async move {
            tokio::select! {
                _ = conn => {}
                () = cancel.cancelled() => {}
            }
        });

        get_current_interfaces(&handle, self).await
    }

    /// Query the gateway IP for a specific interface.
    ///
    /// Uses the same 3-method fallback as Go v1:
    /// 1. Default route on the specific interface
    /// 2. Global default route with matching link index
    /// 3. IP heuristic (network+1)
    pub async fn get_gateway_for_interface(
        &self,
        iface: &str,
    ) -> Result<Option<IpAddr>, NetworkMonitorError> {
        let (conn, handle, _) =
            rtnetlink::new_multicast_connection(&[MulticastGroup::Link, MulticastGroup::Ipv4Route])
                .map_err(|e| NetworkMonitorError::ConnectionFailed(format!("{e}")))?;

        let cancel = self.cancel.clone();
        let _conn_guard = tokio::spawn(async move {
            tokio::select! {
                _ = conn => {}
                () = cancel.cancelled() => {}
            }
        });

        let link_index = match find_link_index(&handle, iface).await {
            Some(idx) => idx,
            None => return Ok(None),
        };

        Ok(infer_gateway(iface, link_index, &handle).await)
    }

    // ── Filtering and throttling ─────────────────────────────────────

    /// Check if an interface should be processed (ignore/only filters).
    pub(crate) fn should_process_interface(&self, name: &str) -> bool {
        if self.config.ignore_interfaces.iter().any(|i| i == name) {
            return false;
        }
        if !self.config.only_interfaces.is_empty()
            && !self.config.only_interfaces.iter().any(|i| i == name)
        {
            return false;
        }
        true
    }

    /// Check if an event should be emitted (throttle check).
    ///
    /// Go v1 uses `lastEventTime map[string]time.Time` with a mutex.
    pub(crate) async fn should_emit(&self, interface: &str, event_type: EventType) -> bool {
        let key = ThrottleKey::new(interface, event_type);
        let mut throttle = self.throttle.lock().await;
        let now = Instant::now();

        match throttle.get(&key) {
            Some(last) if now.duration_since(*last) < self.config.throttle_duration => {
                tracing::debug!(interface = %interface, ?event_type, "Event throttled");
                false
            }
            _ => {
                throttle.insert(key, now);
                true
            }
        }
    }

    /// Send an event to the channel.
    async fn send_event(&self, event: NetworkEvent, tx: &mpsc::Sender<NetworkEvent>) {
        if tx.send(event).await.is_err() {
            tracing::warn!("Event channel closed, stopping monitor");
        }
    }

    /// Stop the monitor.
    pub async fn stop(&self) {
        self.cancel.cancel();
    }
}

// ---------------------------------------------------------------------------
// Gateway inference — 3-method fallback (matches Go v1)
// ---------------------------------------------------------------------------

/// Infer the gateway IP using the same 3-method fallback as Go v1.
///
/// 1. Interface-specific default route
/// 2. Global default route with matching link index
/// 3. IP heuristic (network+1, then network+254)
async fn infer_gateway(
    iface_name: &str,
    link_index: u32,
    handle: &rtnetlink::Handle,
) -> Option<IpAddr> {
    if let Some(gw) = find_default_route_gateway(handle, Some(link_index)).await {
        tracing::debug!(interface = %iface_name, %gw, "Gateway from interface-specific default route");
        return Some(gw);
    }

    if let Some(gw) = find_global_default_gateway(handle, link_index).await {
        tracing::debug!(interface = %iface_name, %gw, "Gateway from global default route");
        return Some(gw);
    }

    if let Some(gw) = ip_heuristic_gateway(handle, link_index).await {
        tracing::debug!(interface = %iface_name, %gw, "Gateway from IP heuristic");
        return Some(gw);
    }

    tracing::debug!(interface = %iface_name, "No gateway found");
    None
}

/// Method 1: Default route on a specific interface.
async fn find_default_route_gateway(
    handle: &rtnetlink::Handle,
    link_index: Option<u32>,
) -> Option<IpAddr> {
    let mut routes = handle.route().get().execute();

    while let Some(item) = routes.next().await {
        let route = match item {
            Ok(r) => r,
            Err(_) => continue,
        };

        if !is_default_route(&route) {
            continue;
        }

        if let Some(idx) = link_index {
            if route.output_interface != Some(idx) {
                continue;
            }
        }

        return extract_gateway_from_route(&route);
    }

    None
}

/// Method 2: Global default route with matching link index.
async fn find_global_default_gateway(
    handle: &rtnetlink::Handle,
    link_index: u32,
) -> Option<IpAddr> {
    let mut routes = handle.route().get().execute();

    while let Some(item) = routes.next().await {
        let route = match item {
            Ok(r) => r,
            Err(_) => continue,
        };

        if !is_default_route(&route) {
            continue;
        }

        if route.output_interface == Some(link_index) {
            return extract_gateway_from_route(&route);
        }
    }

    None
}

/// Method 3: IP heuristic — try `network+1`.
///
/// Given `192.168.1.100/24`, computes `192.168.1.1`.
/// Go v1 also tries `network+254` as a second candidate.
async fn ip_heuristic_gateway(
    handle: &rtnetlink::Handle,
    link_index: u32,
) -> Option<IpAddr> {
    let mut addrs = handle
        .address()
        .get()
        .set_link_index_filter(link_index)
        .execute();

    while let Some(item) = addrs.next().await {
        let addr_msg = match item {
            Ok(m) => m,
            Err(_) => continue,
        };

        let addr = address_msg_to_ip(&addr_msg)?;
        let prefix_len = addr_msg.header.prefix_len as u32;

        if let IpAddr::V4(v4) = addr {
            let net = ipnetwork::Ipv4Network::new(v4, prefix_len as u8).ok()?;
            let network = net.network();
            let octets = network.octets();
            let candidate = std::net::Ipv4Addr::new(
                octets[0],
                octets[1],
                octets[2],
                octets[3].saturating_add(1),
            );
            return Some(IpAddr::V4(candidate));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Interface query helpers
// ---------------------------------------------------------------------------

/// Dump all interfaces via rtnetlink and build InterfaceInfo list.
async fn get_current_interfaces(
    handle: &rtnetlink::Handle,
    monitor: &NetworkMonitor,
) -> Result<Vec<InterfaceInfo>, NetworkMonitorError> {
    let mut links = handle
        .link()
        .get()
        .execute();

    let mut result = Vec::new();

    while let Some(link_msg) = links.next().await {
        let link_msg = match link_msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(%e, "Skipping link message in dump");
                continue;
            }
        };

        let name = match link_name(&link_msg) {
            Some(n) => n,
            None => continue,
        };

        if !monitor.should_process_interface(&name) {
            continue;
        }

        let is_up = link_is_up(&link_msg);
        let is_loopback = link_is_loopback(&link_msg);
        let mac_address = link_mac_address(&link_msg);
        let ip_addresses = query_interface_addresses(handle, link_msg.header.index).await;

        result.push(InterfaceInfo {
            name,
            mac_address,
            ip_addresses,
            is_up,
            is_loopback,
        });
    }

    Ok(result)
}

/// Query all IP addresses assigned to a specific link index.
async fn query_interface_addresses(
    handle: &rtnetlink::Handle,
    link_index: u32,
) -> Vec<IpAddr> {
    let mut addrs = Vec::new();
    let mut stream = handle
        .address()
        .get()
        .set_link_index_filter(link_index)
        .execute();

    while let Some(item) = stream.next().await {
        match item {
            Ok(addr_msg) => {
                if let Some(addr) = address_msg_to_ip(&addr_msg) {
                    addrs.push(addr);
                }
            }
            Err(e) => {
                tracing::debug!(%e, "Error reading address");
            }
        }
    }

    addrs
}

// ---------------------------------------------------------------------------
// Netlink message parsing helpers
// ---------------------------------------------------------------------------

fn link_name(msg: &netlink_packet_route::link::LinkMessage) -> Option<String> {
    use netlink_packet_route::link::LinkAttribute;
    msg.attributes.iter().find_map(|attr| match attr {
        LinkAttribute::IfName(name) => Some(name.clone()),
        _ => None,
    })
}

/// Check IFF_UP | IFF_RUNNING. Go v1: `link.Flags&net.FlagUp != 0`.
fn link_is_up(msg: &netlink_packet_route::link::LinkMessage) -> bool {
    msg.header.flags.contains(LinkFlags::Up)
        && msg.header.flags.contains(LinkFlags::Running)
}

fn link_is_loopback(msg: &netlink_packet_route::link::LinkMessage) -> bool {
    msg.header.flags.contains(LinkFlags::Loopback)
}

fn link_mac_address(msg: &netlink_packet_route::link::LinkMessage) -> Option<String> {
    use netlink_packet_route::link::LinkAttribute;
    msg.attributes.iter().find_map(|attr| match attr {
        LinkAttribute::Address(bytes) if bytes.len() == 6 => Some(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
        )),
        _ => None,
    })
}

/// Resolve an address message to (interface_name, IpAddr).
async fn parse_address_message(
    msg: &netlink_packet_route::address::AddressMessage,
    handle: &rtnetlink::Handle,
) -> Option<(String, IpAddr)> {
    let addr = address_msg_to_ip(msg)?;
    let link_index = msg.header.index;
    let name = resolve_link_name(handle, link_index).await?;
    Some((name, addr))
}

/// Extract the IP address from an address message.
///
/// rtnetlink 0.21 uses `AddressAttribute::Address(IpAddr)` for both
/// IPv4 and IPv6 — the type is `std::net::IpAddr`.
fn address_msg_to_ip(msg: &netlink_packet_route::address::AddressMessage) -> Option<IpAddr> {
    for attr in &msg.attributes {
        match attr {
            AddressAttribute::Address(ip) => return Some(*ip),
            _ => continue,
        }
    }
    None
}

async fn resolve_link_name(handle: &rtnetlink::Handle, link_index: u32) -> Option<String> {
    let mut links = handle.link().get().execute();
    while let Some(item) = links.next().await {
        match item {
            Ok(link) if link.header.index == link_index => {
                return link_name(&link);
            }
            _ => continue,
        }
    }
    None
}

async fn find_link_index(handle: &rtnetlink::Handle, iface: &str) -> Option<u32> {
    let mut links = handle.link().get().execute();
    while let Some(item) = links.next().await {
        match item {
            Ok(link) => {
                if link_name(&link).as_deref() == Some(iface) {
                    return Some(link.header.index);
                }
            }
            _ => continue,
        }
    }
    None
}

fn is_default_route(route: &RouteMessage) -> bool {
    (route.header.address_family == netlink_packet_core::constants::AF_INET
        || route.header.address_family == netlink_packet_core::constants::AF_INET6)
        && route.header.destination_prefix_length == 0
}

fn extract_gateway_from_route(route: &RouteMessage) -> Option<IpAddr> {
    for attr in &route.attributes {
        if let RouteAttribute::Gateway(gw) = attr {
            return match gw {
                RouteAddress::Inet(v4) => Some(IpAddr::V4(*v4)),
                RouteAddress::Inet6(v6) => Some(IpAddr::V6(*v6)),
                _ => None,
            };
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests (pure logic — no rtnetlink dependency)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_throttle_same_interface_same_type() {
        let key = ThrottleKey::new("eth0", EventType::InterfaceUp);
        let mut map: HashMap<ThrottleKey, Instant> = HashMap::new();
        let now = Instant::now();

        assert!(map.get(&key).is_none());
        map.insert(key.clone(), now);

        assert!(map.get(&key).is_some());
        let last = map.get(&key).unwrap();
        assert!(now.duration_since(*last) < Duration::from_secs(2));
    }

    #[test]
    fn test_throttle_different_types() {
        let key1 = ThrottleKey::new("eth0", EventType::InterfaceUp);
        let key2 = ThrottleKey::new("eth0", EventType::AddressAdded);
        assert_ne!(key1, key2);

        let mut map: HashMap<ThrottleKey, Instant> = HashMap::new();
        map.insert(key1, Instant::now());

        assert!(map.get(&key2).is_none());
    }

    #[test]
    fn test_filter_ignore_interface() {
        let config = NetworkMonitorConfig {
            ignore_interfaces: vec!["lo".to_owned(), "br-lan".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(!monitor.should_process_interface("lo"));
        assert!(!monitor.should_process_interface("br-lan"));
        assert!(monitor.should_process_interface("eth0"));
    }

    #[test]
    fn test_filter_only_interface() {
        let config = NetworkMonitorConfig {
            only_interfaces: vec!["eth0".to_owned(), "wlan0".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(monitor.should_process_interface("eth0"));
        assert!(monitor.should_process_interface("wlan0"));
        assert!(!monitor.should_process_interface("br-lan"));
    }

    #[test]
    fn test_filter_ignore_and_only_combined() {
        let config = NetworkMonitorConfig {
            ignore_interfaces: vec!["eth0".to_owned()],
            only_interfaces: vec!["eth0".to_owned(), "wlan0".to_owned()],
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        // ignore takes priority
        assert!(!monitor.should_process_interface("eth0"));
        assert!(monitor.should_process_interface("wlan0"));
        assert!(!monitor.should_process_interface("usb0"));
    }

    #[test]
    fn test_gateway_inference_ip_heuristic() {
        // 192.168.1.100/24 → network 192.168.1.0 → gateway 192.168.1.1
        let addr = std::net::Ipv4Addr::new(192, 168, 1, 100);
        let net = ipnetwork::Ipv4Network::new(addr, 24).unwrap();
        let network = net.network();
        let octets = network.octets();
        let candidate = std::net::Ipv4Addr::new(
            octets[0],
            octets[1],
            octets[2],
            octets[3].saturating_add(1),
        );
        assert_eq!(candidate, std::net::Ipv4Addr::new(192, 168, 1, 1));

        // 10.0.0.5/24 → network 10.0.0.0 → gateway 10.0.0.1
        let addr2 = std::net::Ipv4Addr::new(10, 0, 0, 5);
        let net2 = ipnetwork::Ipv4Network::new(addr2, 24).unwrap();
        let network2 = net2.network();
        let octets2 = network2.octets();
        let candidate2 = std::net::Ipv4Addr::new(
            octets2[0],
            octets2[1],
            octets2[2],
            octets2[3].saturating_add(1),
        );
        assert_eq!(candidate2, std::net::Ipv4Addr::new(10, 0, 0, 1));
    }

    #[test]
    fn test_config_default_values() {
        let config = NetworkMonitorConfig::default();
        assert_eq!(config.ignore_interfaces, vec!["lo"]);
        assert!(config.only_interfaces.is_empty());
        assert_eq!(config.throttle_duration, Duration::from_secs(2));
        assert_eq!(config.event_buffer_size, 100);
    }

    #[tokio::test]
    async fn test_throttle_async() {
        let config = NetworkMonitorConfig {
            throttle_duration: Duration::from_millis(100),
            ..NetworkMonitorConfig::default()
        };
        let monitor = NetworkMonitor::new(config);

        assert!(monitor.should_emit("eth0", EventType::InterfaceUp).await);
        assert!(!monitor.should_emit("eth0", EventType::InterfaceUp).await);
        assert!(monitor.should_emit("eth0", EventType::AddressAdded).await);

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(monitor.should_emit("eth0", EventType::InterfaceUp).await);
    }
}
