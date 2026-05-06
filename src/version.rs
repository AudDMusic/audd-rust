//! Crate version, exposed for callers that want to log it.

/// The published version of this SDK (matches `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
