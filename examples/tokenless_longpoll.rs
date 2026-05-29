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
    let mut poll = consumer.iterate(LongpollIterateOptions {
        timeout: 30,
        ..Default::default()
    });

    loop {
        tokio::select! {
            biased;
            Some(err) = poll.errors.next() => {
                eprintln!("longpoll error: {err}");
                break;
            }
            Some(notif) = poll.notifications.next() => {
                eprintln!(
                    "[notification] radio_id={} code={} {}",
                    notif.radio_id.unwrap_or_default(),
                    notif.notification_code.unwrap_or_default(),
                    notif.notification_message.as_deref().unwrap_or(""),
                );
            }
            Some(m) = poll.matches.next() => {
                println!(
                    "[match] radio_id={} {} — {}",
                    m.radio_id.unwrap_or_default(),
                    m.song.artist.as_deref().unwrap_or(""),
                    m.song.title.as_deref().unwrap_or(""),
                );
            }
            else => break,
        }
    }
    poll.close().await;
    Ok(())
}
