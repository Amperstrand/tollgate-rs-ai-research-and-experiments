//! Error types for tollgate-core.
//!
//! Each error enum corresponds to a core subsystem: wallet operations,
//! resource adapters, protocol state machine, and pricing computation.

/// Errors from the Wallet trait implementation.
#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("token rejected: {0}")]
    TokenRejected(String),
    #[error("funding invalid: {0}")]
    FundingInvalid(String),
    #[error("balance verification failed: {0}")]
    BalanceVerificationFailed(String),
    #[error("settlement failed: {0}")]
    SettlementFailed(String),
    #[error("mint unreachable: {0}")]
    MintUnreachable(String),
    #[error("channel error: {0}")]
    ChannelError(String),
    #[error("wallet internal error: {0}")]
    Internal(String),
}

/// Errors from the ResourceAdapter trait implementation.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("peer not found: {0}")]
    PeerNotFound(String),
    #[error("metering failed: {0}")]
    MeteringFailed(String),
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error("adapter internal error: {0}")]
    Internal(String),
}

/// Protocol-level errors (state machine violations, negotiation failures).
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unexpected message: got {got:?} in state {state}")]
    UnexpectedMessage { state: String, got: String },
    #[error("negotiation failed: {0}")]
    NegotiationFailed(String),
    #[error("channel not found: {0}")]
    ChannelNotFound(String),
    #[error("invalid balance update: {0}")]
    InvalidBalance(String),
    #[error("session closed: {0}")]
    SessionClosed(String),
}

/// Pricing computation errors.
#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("unknown product: {0}")]
    UnknownProduct(String),
    #[error("no applicable pricing: {0}")]
    NoPricing(String),
    #[error("negative balance would result: debit {debit} from balance {balance}")]
    InsufficientBalance { debit: u64, balance: u64 },
    #[error("scale divisor is zero")]
    ZeroScaleDivisor,
    #[error("overflow in pricing calculation")]
    Overflow,
}
