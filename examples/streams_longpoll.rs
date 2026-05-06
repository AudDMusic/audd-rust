//! Long-poll for stream events with the authenticated client.
//!
//! Run: `cargo run --example streams_longpoll -- <category>`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::streams::LongpollOptions;
use audd::{AudD, AudDError};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let category = std::env::args()
        .nth(1)
        .ok_or_else(|| AudDError::Source("usage: streams_longpoll <category>".into()))?;
    let token = std::env::var("AUDD_API_TOKEN").expect("set AUDD_API_TOKEN");
    let audd = AudD::new(token);

    let mut events = audd
        .streams()
        .longpoll(&category, LongpollOptions::default().timeout(30))
        .await?;

    while let Some(ev) = events.next().await {
        println!("{:?}", ev?);
    }
    Ok(())
}
