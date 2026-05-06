//! Tokenless longpoll consumer — for browser widgets, Twitch extensions, mobile apps.
//!
//! Run: `cargo run --example tokenless_longpoll -- <category>`

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use audd::longpoll::LongpollIterateOptions;
use audd::{AudDError, LongpollConsumer};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> Result<(), AudDError> {
    let category = std::env::args()
        .nth(1)
        .ok_or_else(|| AudDError::Source("usage: tokenless_longpoll <category>".into()))?;

    let consumer = LongpollConsumer::new(category);
    let mut stream = consumer.iterate(LongpollIterateOptions {
        timeout: 30,
        ..Default::default()
    });
    while let Some(ev) = stream.next().await {
        println!("{:?}", ev?);
    }
    Ok(())
}
