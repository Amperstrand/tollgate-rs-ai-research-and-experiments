//! SSRF protection for outbound HTTP requests to mint URLs.
//!
//! A malicious Cashu token can carry a mint URL pointing to internal
//! services (AWS metadata, localhost admin panels, internal APIs). The
//! NUT-07 checkstate call POSTs to the token's mint URL — without
//! validation, an attacker could use the tollgate server as a proxy
//! into the internal network.
//!
//! Ported from the Go v1's SSRF-safe client (lightning/lightning.go):
//! resolve the hostname, check every IP against blocklists (loopback,
//! private, link-local, unspecified), reject if ANY address is blocked.

use std::net::IpAddr;
use std::net::ToSocketAddrs;

/// Validate that a URL does not resolve to a blocked address.
///
/// Blocks: loopback (127.x, ::1), private (RFC 1918), link-local
/// (169.254.x, fe80::), unspecified (0.0.0.0, ::). Checks EVERY
/// resolved IP — a hostname that resolves to both a public and a
/// private address is still blocked (DNS rebinding mitigation).
///
/// # Errors
///
/// Returns an error if the URL is malformed, the host doesn't resolve,
/// or any resolved IP is in a blocked range.
pub fn validate_url_no_ssrf(url: &str) -> anyhow::Result<()> {
    let parsed: url::Url = url
        .parse()
        .map_err(|e| anyhow::anyhow!("SSRF: invalid URL: {e}"))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("SSRF: URL has no host: {url}"))?;

    // If the host is already an IP literal, check it directly.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked(ip) {
            anyhow::bail!("SSRF blocked: {host} is a blocked address");
        }
        return Ok(());
    }

    // Resolve the hostname and check every IP.
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("SSRF: DNS resolution failed for {host}: {e}"))?
        .collect();

    if addrs.is_empty() {
        anyhow::bail!("SSRF: {host} resolved to no addresses");
    }

    for addr in &addrs {
        if is_blocked(addr.ip()) {
            anyhow::bail!(
                "SSRF blocked: {host} resolves to blocked address {}",
                addr.ip()
            );
        }
    }

    Ok(())
}

/// Check if an IP is in a blocked range.
fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || is_v6_link_local(v6)
                || is_v6_unique_local(v6)
        }
    }
}

/// IPv6 link-local (fe80::/10).
fn is_v6_link_local(v6: std::net::Ipv6Addr) -> bool {
    v6.segments()[0] & 0xffc0 == 0xfe80
}

/// IPv6 unique-local (fc00::/7 — includes fd00::/8).
fn is_v6_unique_local(v6: std::net::Ipv6Addr) -> bool {
    v6.segments()[0] & 0xfe00 == 0xfc00
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_loopback_ipv4() {
        assert!(validate_url_no_ssrf("http://127.0.0.1:3338/v1/info").is_err());
        assert!(validate_url_no_ssrf("http://localhost:3338/v1/info").is_err());
    }

    #[test]
    fn rejects_private_ipv4() {
        assert!(validate_url_no_ssrf("http://192.168.1.1/v1/info").is_err());
        assert!(validate_url_no_ssrf("http://10.0.0.1/v1/info").is_err());
        assert!(validate_url_no_ssrf("http://172.16.0.1/v1/info").is_err());
    }

    #[test]
    fn rejects_link_local() {
        assert!(validate_url_no_ssrf("http://169.254.169.254/latest/meta-data").is_err());
    }

    #[test]
    fn rejects_loopback_ipv6() {
        assert!(validate_url_no_ssrf("http://[::1]:3338/v1/info").is_err());
    }

    #[test]
    fn rejects_ipv6_link_local() {
        assert!(validate_url_no_ssrf("http://[fe80::1]/v1/info").is_err());
    }

    #[test]
    fn rejects_aws_metadata_endpoint() {
        // The classic SSRF target
        assert!(
            validate_url_no_ssrf(
                "http://169.254.169.254/latest/meta-data/iam/security-credentials/"
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_public_addresses() {
        // testnut.cashu.space resolves to a public IP
        // (if DNS is unavailable in the test env, this test is skipped
        //  by the DNS failure being an error, not a panic)
        let result = validate_url_no_ssrf("https://testnut.cashu.space/v1/info");
        // In CI, DNS should work. If it doesn't, we can't test this.
        // The important thing is that it doesn't SUCCEED with a blocked IP.
        if let Err(e) = &result {
            let msg = format!("{e}");
            assert!(
                !msg.contains("SSRF blocked"),
                "public address should not be blocked: {msg}"
            );
        }
    }

    #[test]
    fn rejects_unspecified() {
        assert!(validate_url_no_ssrf("http://0.0.0.0:3338/v1/info").is_err());
    }

    #[test]
    fn accepts_public_ip_literal() {
        // 8.8.8.8 is Google DNS — public
        assert!(validate_url_no_ssrf("https://8.8.8.8/v1/info").is_ok());
    }
}
