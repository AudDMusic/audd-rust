//! Configure a stream subscription.
//!
//! Run: `cargo run --example streams_setup`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::{AudD, AudDError};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let token = std::env::var("AUDD_API_TOKEN").expect("set AUDD_API_TOKEN");
    let audd = AudD::new(token);

    // 1. Set the callback URL for the account.
    audd.streams()
        .set_callback_url("https://example.com/audd-callback", None, None)
        .await?;

    // 2. Add a stream.
    audd.streams()
        .add("https://example.com/icecast.mp3", 42, None)
        .await?;

    // 3. List streams.
    for s in audd.streams().list().await? {
        println!("{}\t{}\t{}", s.radio_id, s.url, s.stream_running);
    }

    Ok(())
}
