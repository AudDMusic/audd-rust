//! Build the User-Agent string sent on every request.

use crate::version::VERSION;

/// Build the User-Agent string. Format mirrors the sibling SDKs:
/// `audd-rust/<version> rust/<rustc-version> (<os>)`.
///
/// We deliberately don't read `RUSTC_VERSION` at runtime — `env!()`-baked at compile time is fine,
/// and we don't want to require any extra build infra. We resolve the rustc version at compile
/// time via the `RUSTC_VERSION` environment variable if set, falling back to "unknown".
pub fn user_agent() -> String {
    format!(
        "audd-rust/{} rust/{} ({})",
        VERSION,
        rustc_version(),
        std::env::consts::OS
    )
}

fn rustc_version() -> &'static str {
    // Compile-time fallback — most users don't have RUSTC_VERSION set, and that's fine.
    option_env!("RUSTC_VERSION").unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_format() {
        let ua = user_agent();
        assert!(ua.starts_with("audd-rust/"), "got {ua}");
        assert!(ua.contains(" rust/"));
        assert!(ua.contains('('));
    }
}
