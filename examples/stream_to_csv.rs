//! Long-poll a single AudD stream and append every recognition to a CSV.
//!
//! Two modes:
//!
//! * **provision-and-listen** — pass `--url <stream-url>`. The example adds the
//!   stream (defaulting `--radio-id 99999` if you don't set one), polls, and
//!   **deletes the stream slot on exit**.
//! * **listen-only** — pass `--radio-id <id>` alone. The example uses an
//!   existing stream and **does NOT add or delete** anything.
//!
//! Run:
//! ```text
//! AUDD_API_TOKEN=... cargo run --example stream_to_csv -- --url https://example.com/icecast.mp3
//! AUDD_API_TOKEN=... cargo run --example stream_to_csv -- --radio-id 42 --output radio42.csv
//! ```
//!
//! Output is appended; the CSV header is only written when the file is new.
//! Press Ctrl-C to stop. In provision-and-listen mode the stream slot is
//! deleted on shutdown.

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use std::path::PathBuf;

use audd::streams::LongpollOptions;
use audd::{AudD, StreamCallbackPayload};
use chrono::Utc;
use clap::Parser;
use csv::WriterBuilder;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::signal;

const DEFAULT_RADIO_ID: i64 = 99_999;
const DEFAULT_OUTPUT: &str = "audd_stream_tracks.csv";
/// Placeholder URL we set when the account has no callback URL configured —
/// AudD's longpoll requires a 200-OK URL server-side, but we don't need a real
/// receiver since we're consuming via longpoll.
const DEFAULT_CALLBACK_URL: &str = "https://audd.tech/empty/";
/// Server error code returned by `getCallbackUrl` when none is set.
const NO_CALLBACK_ERROR_CODE: i32 = 19;

#[derive(Parser, Debug)]
#[command(
    name = "stream_to_csv",
    about = "Long-poll an AudD stream and append every recognition to a CSV.",
    long_about = None,
)]
struct Args {
    /// Stream URL — when set, the example adds the stream on startup and
    /// deletes it on shutdown. Pair with `--radio-id` to pin the slot ID.
    #[arg(long)]
    url: Option<String>,

    /// Existing stream slot to listen on. Without `--url`, the example treats
    /// the slot as already-provisioned and won't add or delete it.
    #[arg(long)]
    radio_id: Option<i64>,

    /// Output CSV path. Re-runs append; the header is only written for fresh
    /// files.
    #[arg(long, default_value = DEFAULT_OUTPUT)]
    output: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Add the stream on startup, delete it on exit.
    ProvisionAndListen,
    /// Use an existing slot. Don't touch it.
    ListenOnly,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (mode, radio_id) = match (&args.url, args.radio_id) {
        (Some(_), Some(id)) => (Mode::ProvisionAndListen, id),
        (Some(_), None) => (Mode::ProvisionAndListen, DEFAULT_RADIO_ID),
        (None, Some(id)) => (Mode::ListenOnly, id),
        (None, None) => {
            return Err(
                "pass --url to provision a stream, or --radio-id to listen on an existing one"
                    .into(),
            );
        }
    };

    // Empty token => SDK falls back to AUDD_API_TOKEN env var.
    let audd = AudD::new(std::env::var("AUDD_API_TOKEN").unwrap_or_default());

    let mut we_set_default_callback = false;
    match mode {
        Mode::ProvisionAndListen => {
            we_set_default_callback = ensure_callback_for_provisioning(&audd).await?;
            let url = args.url.as_deref().expect("ProvisionAndListen has --url");
            audd.streams().add(url, radio_id, None).await?;
            eprintln!("added stream radio_id={radio_id} url={url}");
        }
        Mode::ListenOnly => {
            ensure_callback_for_listen_only(&audd).await?;
            eprintln!("listening on existing stream radio_id={radio_id}");
        }
    }

    // Open the CSV in append mode; write the header only if the file is fresh.
    let file_is_fresh = !args.output.exists()
        || std::fs::metadata(&args.output)
            .map(|m| m.len() == 0)
            .unwrap_or(true);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.output)?;
    let mut writer = WriterBuilder::new().has_headers(false).from_writer(file);
    if file_is_fresh {
        writer.write_record([
            "received_at",
            "radio_id",
            "timestamp",
            "score",
            "artist",
            "title",
            "album",
            "song_link",
        ])?;
        writer.flush()?;
    }

    let category = audd.streams().derive_longpoll_category(radio_id);
    eprintln!(
        "writing matches to {} (category={category}, Ctrl-C to stop)",
        args.output.display()
    );

    // We've already validated the callback URL above; tell longpoll to skip
    // the redundant preflight.
    let mut events = audd
        .streams()
        .longpoll(
            &category,
            LongpollOptions::default()
                .timeout(50)
                .skip_callback_check(true),
        )
        .await?;

    let shutdown = signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                eprintln!("\nshutdown requested");
                break;
            }
            ev = events.next() => {
                match ev {
                    Some(Ok(value)) => handle_event(value, &mut writer)?,
                    Some(Err(e)) => {
                        eprintln!("longpoll error: {e}");
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    if mode == Mode::ProvisionAndListen {
        match audd.streams().delete(radio_id).await {
            Ok(()) => eprintln!("deleted stream radio_id={radio_id}"),
            Err(e) => eprintln!("could not delete stream radio_id={radio_id}: {e}"),
        }
    }
    if we_set_default_callback {
        eprintln!(
            "left {DEFAULT_CALLBACK_URL} as your account callback — change it via \
             streams().set_callback_url(...) if needed."
        );
    }

    Ok(())
}

/// In provision-and-listen mode: read the configured callback URL. If none is
/// set (#19), set [`DEFAULT_CALLBACK_URL`] ourselves. Returns `true` if we did.
async fn ensure_callback_for_provisioning(audd: &AudD) -> Result<bool, Box<dyn std::error::Error>> {
    match audd.streams().get_callback_url().await {
        Ok(_) => Ok(false),
        Err(e) if e.error_code() == Some(NO_CALLBACK_ERROR_CODE) => {
            eprintln!(
                "longpoll requires any 200-OK URL server-side; using {DEFAULT_CALLBACK_URL} as a default."
            );
            audd.streams()
                .set_callback_url(DEFAULT_CALLBACK_URL, None)
                .await?;
            Ok(true)
        }
        Err(e) => Err(e.into()),
    }
}

/// In listen-only mode: refuse to start if the account has no callback URL —
/// longpoll won't deliver. We don't silently mutate someone else's account.
async fn ensure_callback_for_listen_only(audd: &AudD) -> Result<(), Box<dyn std::error::Error>> {
    match audd.streams().get_callback_url().await {
        Ok(_) => Ok(()),
        Err(e) if e.error_code() == Some(NO_CALLBACK_ERROR_CODE) => Err(
            "stream slot exists but no callback URL is configured for this account; \
             longpoll won't deliver. Set one first via streams().set_callback_url(...).\n\
             https://audd.tech/empty/ is fine if you only want longpolling."
                .into(),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Decode one longpoll envelope and append a row per matched track. Logs
/// notification envelopes via `eprintln!`.
fn handle_event(
    value: Value,
    writer: &mut csv::Writer<std::fs::File>,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = match StreamCallbackPayload::parse(value.clone()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("could not parse longpoll envelope: {e} — raw={value}");
            return Ok(());
        }
    };

    if let Some(notif) = payload.notification.as_ref() {
        eprintln!(
            "[notification] radio_id={} code={} {}",
            notif.radio_id, notif.notification_code, notif.notification_message,
        );
        return Ok(());
    }

    let Some(result) = payload.result.as_ref() else {
        // Longpoll heartbeat / empty envelope — no result, no notification.
        return Ok(());
    };

    let received_at = Utc::now().to_rfc3339();
    for entry in &result.results {
        writer.write_record([
            received_at.as_str(),
            &result.radio_id.to_string(),
            result.timestamp.as_deref().unwrap_or(""),
            &entry.score.to_string(),
            entry.artist.as_str(),
            entry.title.as_str(),
            entry.album.as_deref().unwrap_or(""),
            entry.song_link.as_deref().unwrap_or(""),
        ])?;
    }
    writer.flush()?;
    println!(
        "[match] radio_id={} timestamp={} matches={}",
        result.radio_id,
        result.timestamp.as_deref().unwrap_or(""),
        result.results.len(),
    );
    Ok(())
}
