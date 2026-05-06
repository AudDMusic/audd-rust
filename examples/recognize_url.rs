//! Recognize a song from a URL.
//!
//! Run: `cargo run --example recognize_url`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::{AudD, AudDError};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    // Use the public `test` token by default. Set AUDD_API_TOKEN to override.
    let token = std::env::var("AUDD_API_TOKEN").unwrap_or_else(|_| "test".to_string());
    let audd = AudD::new(token);
    if let Some(result) = audd.recognize("https://audd.tech/example.mp3").await? {
        println!(
            "{} — {}",
            result.artist.as_deref().unwrap_or(""),
            result.title.as_deref().unwrap_or(""),
        );
    } else {
        println!("no match");
    }
    Ok(())
}
