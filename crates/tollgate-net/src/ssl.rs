//! SSL certificate management — a thin wrapper around `certbot` for issuing
//! and renewing Let's Encrypt certificates, with all certbot state kept
//! under `/etc/tollgate/`.
//!
//! This is intentionally a CLI wrapper, not a full ACME client: certbot owns
//! the ACME protocol, we own the on-disk layout and the operator UX.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use x509_parser::pem::parse_x509_pem;
use x509_parser::prelude::X509Certificate;
use x509_parser::time::ASN1Time;

pub const CERTS_ROOT: &str = "/etc/tollgate/certs";
pub const LETSENCRYPT_DIR: &str = "/etc/tollgate/certs/letsencrypt";
pub const WORK_DIR: &str = "/var/lib/tollgate/certbot";
pub const LOGS_DIR: &str = "/var/log/tollgate/certbot";

pub fn apply(
    domain: &str,
    email: &str,
    dns_plugin: Option<&str>,
    staging: bool,
) -> Result<()> {
    ensure_certbot_installed()?;
    std::fs::create_dir_all(CERTS_ROOT)
        .with_context(|| format!("failed to create {CERTS_ROOT}"))?;

    let mut cmd = Command::new("certbot");
    cmd.arg("certonly")
        .arg("--non-interactive")
        .arg("--agree-tos")
        .arg("--no-eff-email")
        .arg("--email")
        .arg(email)
        .arg("-d")
        .arg(domain)
        .arg("--config-dir")
        .arg(LETSENCRYPT_DIR)
        .arg("--work-dir")
        .arg(WORK_DIR)
        .arg("--logs-dir")
        .arg(LOGS_DIR);

    match dns_plugin {
        Some(plugin) => {
            cmd.arg(format!("--dns-{plugin}"));
        }
        None => {
            cmd.arg("--standalone").arg("--http-01-port").arg("80");
        }
    }

    if staging {
        cmd.arg("--staging");
    }

    tracing::info!(%domain, %email, dns = ?dns_plugin, staging, "running certbot certonly");
    let status = cmd.status().context("failed to spawn certbot")?;
    if !status.success() {
        bail!(
            "certbot exited with {} for {domain}",
            exit_code_str(status.code())
        );
    }

    println!("SSL: certificate issued for {domain}");
    println!("  Live certs: {LETSENCRYPT_DIR}/live/{domain}/");
    println!("  Remove with: tollgate ssl remove --domain {domain}");
    Ok(())
}

pub fn remove(domain: &str) -> Result<()> {
    let live_dir = Path::new(LETSENCRYPT_DIR).join("live").join(domain);
    if !live_dir.exists() {
        println!("SSL: no certificate found for {domain}");
        return Ok(());
    }

    if ensure_certbot_installed().is_ok() {
        let status = Command::new("certbot")
            .arg("delete")
            .arg("--cert-name")
            .arg(domain)
            .arg("--non-interactive")
            .arg("--config-dir")
            .arg(LETSENCRYPT_DIR)
            .arg("--work-dir")
            .arg(WORK_DIR)
            .arg("--logs-dir")
            .arg(LOGS_DIR)
            .status()
            .context("failed to spawn certbot")?;
        if !status.success() {
            bail!(
                "certbot delete exited with {} for {domain}",
                exit_code_str(status.code())
            );
        }
    } else {
        for sub in ["live", "archive"] {
            let p = Path::new(LETSENCRYPT_DIR).join(sub).join(domain);
            if p.exists() {
                std::fs::remove_dir_all(&p)
                    .with_context(|| format!("failed to remove {}", p.display()))?;
            }
        }
        let conf = Path::new(LETSENCRYPT_DIR)
            .join("renewal")
            .join(format!("{domain}.conf"));
        if conf.exists() {
            std::fs::remove_file(&conf)
                .with_context(|| format!("failed to remove {}", conf.display()))?;
        }
    }

    println!("SSL: removed certificate for {domain}");
    Ok(())
}

pub fn status() -> Result<()> {
    let live_root = Path::new(LETSENCRYPT_DIR).join("live");
    print_status_report(&live_root)
}

/// Pure helper: walk `live_root`, return `(domain, formatted_summary)` pairs
/// sorted by domain. Extracted so tests can drive it against a temp dir.
fn collect_cert_summaries(live_root: &Path) -> Result<Vec<(String, String)>> {
    if !live_root.exists() {
        return Ok(Vec::new());
    }

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(live_root)
        .with_context(|| format!("failed to read {}", live_root.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| p.join("cert.pem").exists())
        .collect();
    dirs.sort();

    dirs.into_iter()
        .map(|dir| {
            let domain = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let summary = match read_cert_summary(&dir.join("cert.pem")) {
                Ok(s) => s,
                Err(e) => format!("(unreadable: {e})"),
            };
            Ok((domain, summary))
        })
        .collect()
}

fn print_status_report(live_root: &Path) -> Result<()> {
    let entries = collect_cert_summaries(live_root)?;
    if entries.is_empty() {
        println!("SSL: no certificates managed");
        println!(
            "  Run 'tollgate ssl apply --domain <domain> --email <email>' to issue one."
        );
        return Ok(());
    }

    println!(
        "SSL: {} certificate(s) under {LETSENCRYPT_DIR}/live/",
        entries.len()
    );
    for (domain, summary) in entries {
        println!("  {domain}\t{summary}");
    }
    Ok(())
}

fn ensure_certbot_installed() -> Result<()> {
    let out = Command::new("certbot")
        .arg("--version")
        .output()
        .context("certbot not found on PATH — install certbot first")?;
    if !out.status.success() {
        bail!("'certbot --version' failed; is certbot installed correctly?");
    }
    Ok(())
}

fn exit_code_str(c: Option<i32>) -> String {
    match c {
        Some(n) => n.to_string(),
        None => "<signal>".to_string(),
    }
}

fn read_cert_summary(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let (_, pem) = parse_x509_pem(&bytes).context("invalid PEM")?;
    let cert = pem.parse_x509().context("invalid X.509 certificate")?;
    Ok(format_cert_summary(&cert))
}

fn format_cert_summary(cert: &X509Certificate<'_>) -> String {
    let validity = cert.validity();
    let now = ASN1Time::now();
    let expiry = validity.not_after;

    if expiry < now {
        format!("EXPIRED on {expiry}")
    } else {
        let days = (expiry.timestamp() - now.timestamp()) / 86_400;
        format!("expires {expiry}  ({days} days remaining)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::OnceLock;

    static EXPIRED_CERT_PEM: &[u8] = include_bytes!("ssl_test_expired.pem");
    static VALID_CERT_PEM: &[u8] = include_bytes!("ssl_test_valid.pem");

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "tollgate-ssl-test-{label}-{}",
                std::process::id()
            ));
            fs::remove_dir_all(&root).ok();
            fs::create_dir_all(&root).expect("create temp root");
            TempTree { root }
        }

        fn write_cert(&self, domain: &str, pem: &[u8]) {
            let dir = self.root.join("live").join(domain);
            fs::create_dir_all(&dir).expect("create live/<domain>");
            fs::write(dir.join("cert.pem"), pem).expect("write cert.pem");
        }

        fn live_root(&self) -> PathBuf {
            self.root.join("live")
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    fn expired_summary() -> &'static str {
        static CELL: OnceLock<String> = OnceLock::new();
        CELL.get_or_init(|| {
            let (_, pem) = parse_x509_pem(EXPIRED_CERT_PEM).expect("parse expired PEM");
            let cert = pem.parse_x509().expect("parse expired cert");
            format_cert_summary(&cert)
        })
    }

    fn valid_summary() -> &'static str {
        static CELL: OnceLock<String> = OnceLock::new();
        CELL.get_or_init(|| {
            let (_, pem) = parse_x509_pem(VALID_CERT_PEM).expect("parse valid PEM");
            let cert = pem.parse_x509().expect("parse valid cert");
            format_cert_summary(&cert)
        })
    }

    #[test]
    fn format_cert_summary_marks_expired_cert_as_expired() {
        let s = expired_summary();
        assert!(
            s.starts_with("EXPIRED on"),
            "expected EXPIRED prefix, got: {s}"
        );
    }

    #[test]
    fn format_cert_summary_reports_days_remaining_for_valid_cert() {
        let s = valid_summary();
        assert!(
            s.starts_with("expires ") && s.contains("days remaining"),
            "expected 'expires ... (N days remaining)', got: {s}"
        );
    }

    #[test]
    fn read_cert_summary_rejects_non_pem_input() {
        let tree = TempTree::new("non-pem");
        let bad = tree.root.join("not-a-pem");
        fs::write(&bad, b"definitely not a PEM file").expect("write junk");
        let err = read_cert_summary(&bad).unwrap_err();
        assert!(
            err.to_string().contains("invalid PEM"),
            "expected invalid PEM error, got: {err}"
        );
    }

    #[test]
    fn collect_cert_summaries_returns_empty_when_live_root_missing() {
        let tree = TempTree::new("empty");
        let entries = collect_cert_summaries(&tree.live_root()).expect("ok");
        assert!(entries.is_empty(), "expected no entries, got {entries:?}");
    }

    #[test]
    fn collect_cert_summaries_lists_each_domain_sorted_with_summary() {
        let tree = TempTree::new("two-certs");
        tree.write_cert("b.example.com", VALID_CERT_PEM);
        tree.write_cert("a.example.com", EXPIRED_CERT_PEM);

        let entries = collect_cert_summaries(&tree.live_root()).expect("ok");
        assert_eq!(entries.len(), 2, "expected 2 entries, got {entries:?}");
        assert_eq!(entries[0].0, "a.example.com");
        assert!(entries[0].1.starts_with("EXPIRED on"));
        assert_eq!(entries[1].0, "b.example.com");
        assert!(entries[1].1.starts_with("expires "));
    }

    #[test]
    fn collect_cert_summaries_skips_dirs_without_cert_pem() {
        let tree = TempTree::new("no-cert");
        let live = tree.live_root();
        fs::create_dir_all(live.join("incomplete.example.com")).expect("mkdir");
        fs::write(live.join("incomplete.example.com").join("README"), b"no cert here")
            .expect("write readme");

        let entries = collect_cert_summaries(&live).expect("ok");
        assert!(entries.is_empty(), "incomplete dir should be skipped, got {entries:?}");
    }
}
