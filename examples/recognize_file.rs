//! Recognize a song from a local file.
//!
//! Run: `cargo run --example recognize_file -- path/to/clip.mp3`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use std::path::PathBuf;

use audd::{AudD, AudDError, Source};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| AudDError::Source("usage: recognize_file <path>".into()))?;
    let token = std::env::var("AUDD_API_TOKEN").unwrap_or_else(|_| "test".into());
    let audd = AudD::new(token);
    let result = audd.recognize(Source::Path(path)).await?;
    match result {
        Some(r) => println!(
            "{} — {}",
            r.artist.as_deref().unwrap_or(""),
            r.title.as_deref().unwrap_or("")
        ),
        None => println!("no match"),
    }
    Ok(())
}
