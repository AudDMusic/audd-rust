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

    let mut poll = audd
        .streams()
        .longpoll(&category, LongpollOptions::default().timeout(30))
        .await?;

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
