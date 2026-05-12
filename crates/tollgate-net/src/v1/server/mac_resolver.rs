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
