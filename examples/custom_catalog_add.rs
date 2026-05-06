//! ⚠ NOT for music recognition. The custom-catalog endpoint adds a song to
//! your private fingerprint database. For recognition, use `AudD::recognize`.
//!
//! Run: `cargo run --example custom_catalog_add`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::{AudD, AudDError};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let token =
        std::env::var("AUDD_API_TOKEN").expect("set AUDD_API_TOKEN (special-access required)");
    let audd = AudD::new(token);

    audd.custom_catalog()
        .add(42, "https://example.com/my-track.mp3")
        .await?;

    println!("added audio_id=42 to your catalog");
    Ok(())
}
