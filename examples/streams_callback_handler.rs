//! Demonstration of parsing a callback POST body in your own webhook handler.
//!
//! Run: `cargo run --example streams_callback_handler`
//!
//! The SDK provides `handle_callback` (raw bytes → typed event) and
//! `parse_callback` (already-deserialized JSON → typed event). Bring your own
//! HTTP framework (axum, actix-web, hyper, rocket, ...) and pass the request
//! body bytes to `handle_callback`.

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::{handle_callback, AudDError, CallbackEvent};

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    // In a real webhook, this would be the bytes of the POST body extracted
    // from your framework — e.g. `axum::body::Bytes`, `actix_web::web::Bytes`,
    // `hyper::body::to_bytes(req.into_body()).await?`, etc.
    let raw_body: &[u8] = br#"{
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
    }"#;

    match handle_callback(raw_body)? {
        CallbackEvent::Match(m) => {
            println!(
                "[match] radio_id={} {} — {}",
                m.radio_id, m.song.artist, m.song.title,
            );
            for alt in &m.alternatives {
                println!("  alternative: {} — {}", alt.artist, alt.title);
            }
        }
        CallbackEvent::Notification(n) => {
            println!(
                "[notification] radio_id={} code={} {}",
                n.radio_id, n.notification_code, n.notification_message,
            );
        }
    }
    Ok(())
}
