//! Demonstration of parsing a callback POST body in your own webhook handler.
//!
//! Run: `cargo run --example streams_callback_handler`
//!
//! The SDK provides `parse_callback`; bring your own HTTP framework
//! (axum, actix, hyper, ...).

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::{parse_callback, AudDError};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let example_body = serde_json::json!({
        "status": "success",
        "result": {
            "radio_id": 7,
            "timestamp": "2026-05-04 10:31:43",
            "play_length": 111,
            "results": [
                {
                    "artist": "Alan Walker, A$AP Rocky",
                    "title": "Live Fast (PUBGM)",
                    "score": 100
                }
            ]
        }
    });
    let payload = parse_callback(example_body)?;
    if let Some(result) = payload.result {
        for r in result.results {
            println!("{} — {}", r.artist, r.title);
        }
    }
    Ok(())
}
