use crate::protocol::Hash32;
use crate::protocol::PubKey;

/// Top-level configuration for a TollGate node.
///
/// See docs/design/core/tollgate-configuration.md for the YAML schema.
/// This struct holds all configuration; parsing from YAML will be added in M1.7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub identity: IdentityConfig,
    pub products: Vec<ProductConfig>,
    pub channels: ChannelConfig,
    pub metering: MeteringConfig,
    pub bootstrap: BootstrapConfig,
    pub mints: Vec<MintConfig>,
    pub peers: Vec<PeerConfig>,
}

/// Node identity configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityConfig {
    /// Node's public key (secp256k1 compressed, 33 bytes).
    pub pubkey: PubKey,
    /// Human-readable node name (for logging/debugging only).
    pub name: String,
}

/// Product configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductConfig {
    /// Product identifier.
    pub product_id: Hash32,
    /// Opaque product descriptor (format depends on deployment).
    pub extensions: Vec<u8>,
    /// Pricing scale divisor (divides raw counter values into billable units).
    pub pricing_scale: u64,
    /// Available mint options for this product.
    pub mint_options: Vec<ProductMintConfig>,
}

/// Mint option within a product configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductMintConfig {
    pub option_id: Hash32,
    pub mint_url: String,
    pub price_per_second: i64,
    pub price_per_unit: i64,
    pub mint_unit: String,
}

/// Channel configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelConfig {
    /// Default channel funding amount.
    pub default_funding: u64,
    /// Maximum channel balance before forced rollover.
    pub max_balance: u64,
    /// Whether to auto-rollover channels when max_balance is reached.
    pub auto_rollover: bool,
}

/// Metering configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeteringConfig {
    /// Default metering interval in milliseconds.
    pub interval_ms: u32,
    /// Minimum allowed metering interval.
    pub min_interval_ms: u32,
    /// Maximum allowed metering interval.
    pub max_interval_ms: u32,
    /// Transit loss threshold for calibration.
    pub transit_loss_threshold: super::metering::TransitLossThreshold,
}

/// Bootstrap token configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapConfig {
    /// Whether bootstrap tokens are accepted.
    pub enabled: bool,
    /// Minimum token value to accept.
    pub min_token_value: u64,
    /// How long a bootstrap token keeps access (in ms). 0 = indefinite until channel opens.
    pub token_lifetime_ms: u64,
}

/// Mint configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintConfig {
    /// Mint URL.
    pub url: String,
    /// Whether this mint is trusted for channel funding.
    pub trusted: bool,
}

/// Peer-specific configuration (overrides defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerConfig {
    /// Peer's public key.
    pub pubkey: PubKey,
    /// Optional product override for this peer.
    pub product_id: Option<Hash32>,
    /// Optional pricing override for this peer.
    pub pricing_scale: Option<u64>,
    /// Whether this peer is whitelisted for zero-price access.
    pub zero_price: bool,
}
