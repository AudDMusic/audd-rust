//! Recognize a long file via the enterprise endpoint.
//!
//! Run: `cargo run --example recognize_enterprise -- https://example.com/album.mp3`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::client::EnterpriseOptions;
use audd::{AudD, AudDError};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://audd.tech/example.mp3".into());
    let token = std::env::var("AUDD_API_TOKEN").unwrap_or_else(|_| "test".into());
    let audd = AudD::new(token);
    let opts = EnterpriseOptions {
        // Always pass limit=1 during dev/testing to keep enterprise quotas low.
        limit: Some(1),
        ..Default::default()
    };
    let matches = audd.recognize_enterprise(url.as_str(), opts).await?;
    for m in matches {
        println!(
            "[{}] {} — {}",
            m.timecode,
            m.artist.as_deref().unwrap_or(""),
            m.title.as_deref().unwrap_or("")
        );
    }
    Ok(())
}
