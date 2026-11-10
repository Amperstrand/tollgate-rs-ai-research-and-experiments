use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MacResolveError {
    #[error("IP not found in DHCP leases: {0}")]
    NotFound(String),
    #[error("failed to read DHCP leases: {0}")]
    Io(#[from] std::io::Error),
}

pub trait MacResolver: Send + Sync {
    fn resolve(&self, ip: &str) -> Result<String, MacResolveError>;
}

pub struct StubMacResolver {
    mac: String,
}

impl StubMacResolver {
    pub fn new(mac: &str) -> Self {
        Self {
            mac: mac.to_owned(),
        }
    }
}

impl Default for StubMacResolver {
    fn default() -> Self {
        Self::new("00:11:22:33:44:55")
    }
}

impl MacResolver for StubMacResolver {
    fn resolve(&self, _ip: &str) -> Result<String, MacResolveError> {
        Ok(self.mac.clone())
    }
}

/// OpenWrt dnsmasq default leasefile (`dhcp.lan.leasefile`).
pub const DEFAULT_LEASES_FILE: &str = "/tmp/dhcp.leases";

/// Resolves client IPs to MAC addresses via the dnsmasq leasefile
/// (dnsmasq line format: `expiry mac ip hostname [client-id]`).
pub struct DhcpLeasesResolver {
    leases_path: std::path::PathBuf,
}

impl DhcpLeasesResolver {
    pub fn with_path<P: Into<std::path::PathBuf>>(leases_path: P) -> Self {
        Self {
            leases_path: leases_path.into(),
        }
    }
}

impl Default for DhcpLeasesResolver {
    fn default() -> Self {
        Self::with_path(DEFAULT_LEASES_FILE)
    }
}

/// Extract the MAC bound to `ip` from dnsmasq leasefile contents.
fn mac_from_leases(contents: &str, ip: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        (parts.len() >= 3 && parts[2] == ip).then(|| parts[1].to_owned())
    })
}

impl MacResolver for DhcpLeasesResolver {
    fn resolve(&self, ip: &str) -> Result<String, MacResolveError> {
        let contents = std::fs::read_to_string(&self.leases_path)?;
        mac_from_leases(&contents, ip).ok_or_else(|| MacResolveError::NotFound(ip.to_owned()))
    }
}

pub fn extract_client_ip(
    connect_info: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
) -> String {
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(xff_str) = xff.to_str() {
            if let Some(first_ip) = xff_str.split(',').next() {
                let trimmed = first_ip.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_owned();
                }
            }
        }
    }

    if let Some(xri) = headers.get("x-real-ip") {
        if let Ok(ip) = xri.to_str() {
            let trimmed = ip.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }

    connect_info
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_connect_info(ip: &str) -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 1234))
    }

    #[test]
    fn extract_ip_xff_first_ip_wins() {
        let ci = make_connect_info("10.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        assert_eq!(extract_client_ip(Some(&ci), &headers), "1.2.3.4");
    }

    #[test]
    fn extract_ip_x_real_ip_fallback() {
        let ci = make_connect_info("10.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "9.8.7.6".parse().unwrap());
        assert_eq!(extract_client_ip(Some(&ci), &headers), "9.8.7.6");
    }

    #[test]
    fn extract_ip_xff_beats_x_real_ip() {
        let ci = make_connect_info("10.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.1.1.1".parse().unwrap());
        headers.insert("x-real-ip", "2.2.2.2".parse().unwrap());
        assert_eq!(extract_client_ip(Some(&ci), &headers), "1.1.1.1");
    }

    #[test]
    fn extract_ip_falls_back_to_connect_info() {
        let ci = make_connect_info("10.0.0.1");
        let headers = HeaderMap::new();
        assert_eq!(extract_client_ip(Some(&ci), &headers), "10.0.0.1");
    }

    #[test]
    fn dhcp_leases_parse_finds_mac_for_ip() {
        let fixture = "1718000000 aa:bb:cc:00:00:01 192.168.5.23 client1\n\
                       1718000000 aa:bb:cc:00:00:02 192.168.5.24 client2\n";
        assert_eq!(
            mac_from_leases(fixture, "192.168.5.24"),
            Some("aa:bb:cc:00:00:02".to_owned())
        );
    }

    #[test]
    fn dhcp_leases_parse_ignores_malformed_lines() {
        let fixture = "garbage line\n1718000000 aa:bb:cc:00:00:03 10.0.0.7 host\n";
        assert_eq!(
            mac_from_leases(fixture, "10.0.0.7"),
            Some("aa:bb:cc:00:00:03".to_owned())
        );
    }

    #[test]
    fn dhcp_leases_parse_unknown_ip_is_none() {
        assert_eq!(mac_from_leases("", "192.168.5.23"), None);
    }

    #[test]
    fn dhcp_leases_resolver_reads_configured_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dhcp.leases");
        std::fs::write(&path, "1718000000 aa:bb:cc:00:00:09 172.16.0.5 x\n")
            .expect("write fixture");
        let resolver = DhcpLeasesResolver::with_path(&path);
        assert_eq!(
            resolver.resolve("172.16.0.5").expect("resolve"),
            "aa:bb:cc:00:00:09"
        );
        assert!(resolver.resolve("172.16.0.6").is_err());
    }

    #[test]
    fn dhcp_leases_default_points_at_openwrt_dnsmasq_path() {
        assert_eq!(DEFAULT_LEASES_FILE, "/tmp/dhcp.leases");
    }

    #[test]
    fn extract_ip_no_connect_info_no_headers() {
        let headers = HeaderMap::new();
        assert_eq!(extract_client_ip(None, &headers), "");
    }

    #[test]
    fn extract_ip_trims_whitespace() {
        let ci = make_connect_info("10.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "  1.2.3.4  , 5.6.7.8".parse().unwrap());
        assert_eq!(extract_client_ip(Some(&ci), &headers), "1.2.3.4");
    }
}
