//! MAC address resolution for the v1 compatibility layer.
//!
//! Resolves client MAC addresses from IP addresses using DHCP lease files
//! (and stub/fallback strategies), and extracts the client IP from request
//! headers or the connection peer address.
//!
//! Ported from the experimental v1 archive; depends only on the standard
//! library, `axum`, and `thiserror` — no experimental `tollgate-core` types.

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

/// Resolves MAC addresses from a dnsmasq-style DHCP leases file
/// (`/tmp/dhcp.leases`).
///
/// Each lease line is `<expiry> <mac> <ip> <hostname> <client-id>`, so the
/// MAC sits at column 1 and the IP at column 2.
pub struct DhcpLeasesResolver;

impl MacResolver for DhcpLeasesResolver {
    fn resolve(&self, ip: &str) -> Result<String, MacResolveError> {
        let contents = std::fs::read_to_string("/tmp/dhcp.leases")?;
        for line in contents.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[2] == ip {
                return Ok(parts[1].to_owned());
            }
        }
        Err(MacResolveError::NotFound(ip.to_owned()))
    }
}

/// Extracts the client IP address from a request.
///
/// Precedence: `X-Forwarded-For` (first entry) > `X-Real-IP` > connection
/// peer address. Returns an empty string when no source is available.
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
