//! Official Rust SDK for the [AudD](https://audd.io) music recognition API.
//!
//! ```no_run
//! # async fn run() -> Result<(), audd::AudDError> {
//! use audd::AudD;
//!
//! let audd = AudD::new("test");
//! if let Some(result) = audd.recognize("https://audd.tech/example.mp3").await? {
//!     println!("{} — {}", result.artist.as_deref().unwrap_or(""), result.title.as_deref().unwrap_or(""));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Use [`AudD::builder`] to configure retries, timeouts, or a custom
//! [`reqwest::Client`]. See the README for a full capability tour and the
//! [audd-openapi](https://github.com/AudDMusic/audd-openapi) repository for the
//! canonical API contract.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::must_use_candidate)]
// `AudDError` is intentionally rich (preserves error_code, request_id,
// raw_response, etc.) — boxing every Result variant would hurt DX. Acceptable
// for an SDK error type.
#![allow(clippy::result_large_err)]
// Sync/async parity intentionally surfaces async fns on the auth client even
// when an individual one doesn't await — keeps the API consistent.
#![allow(clippy::unused_async)]
// Owned-String / Vec<u8> arguments are taken by value to make per-attempt
// retry easier downstream (no borrow lifetimes to thread).
#![allow(clippy::needless_pass_by_value)]
// Boilerplate quibbles.
#![allow(clippy::type_complexity)]
#![allow(clippy::redundant_guards)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]

pub mod advanced;
pub mod client;
pub mod custom_catalog;
pub mod errors;
pub mod helpers;
mod http;
pub mod longpoll;
pub mod models;
pub mod retry;
pub mod source;
pub mod streams;
mod user_agent;
mod version;

pub use advanced::Advanced;
pub use client::{AudD, AudDBuilder, AudDEvent, EnterpriseOptions, EventKind, OnEventHook};
pub use custom_catalog::CustomCatalog;
pub use errors::{error_for_code, AudDError, ErrorKind};
pub use helpers::{add_return_to_url, derive_longpoll_category, parse_callback};
pub use longpoll::{LongpollConsumer, LongpollConsumerBuilder, LongpollIterateOptions};
pub use models::{
    AppleMusicMetadata, DeezerMetadata, EnterpriseChunkResult, EnterpriseMatch, LyricsResult,
    MusicBrainzEntry, NapsterMetadata, RecognitionMatch, RecognitionResult, SpotifyMetadata,
    Stream, StreamCallbackNotification, StreamCallbackPayload, StreamCallbackResult,
    StreamCallbackResultEntry, StreamingProvider,
};
pub use retry::{RetryClass, RetryPolicy};
pub use source::Source;
pub use streams::{LongpollOptions, Streams};
pub use version::VERSION;
