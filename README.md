# audd-rust

[![CI](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml)
[![Contract](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml)
[![Crates.io](https://img.shields.io/crates/v/audd.svg)](https://crates.io/crates/audd)
[![docs.rs](https://docs.rs/audd/badge.svg)](https://docs.rs/audd)

Official Rust crate for [music recognition API](https://audd.io): identify music from a short audio clip, a long audio file, or a live stream.

The API itself is so simple that it can easily be used even without an SDK: [docs.audd.io](https://docs.audd.io).

## Quickstart

```toml
[dependencies]
audd = "1.4"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Get your API token at [dashboard.audd.io](https://dashboard.audd.io).

Recognize from a URL:

```rust,no_run
use audd::AudD;

#[tokio::main]
async fn main() -> Result<(), audd::AudDError> {
    let audd = AudD::new("your-api-token");
    if let Some(r) = audd.recognize("https://audd.tech/example.mp3").await? {
        println!("{} — {}",
            r.artist.as_deref().unwrap_or(""),
            r.title.as_deref().unwrap_or(""));
    }
    Ok(())
}
```

Recognize from a local file:

```rust,no_run
use audd::{AudD, Source};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), audd::AudDError> {
    let audd = AudD::new("your-api-token");
    let result = audd.recognize(Source::Path(PathBuf::from("clip.mp3"))).await?;
    println!("{result:?}");
    Ok(())
}
```

`recognize` takes anything `Into<Source>` — `&str` (URL or path, auto-detected), `PathBuf`, `Vec<u8>`, or `Source::Reader(Box<dyn AsyncRead + ...>)` for an arbitrary async reader. It returns `Ok(None)` when the server reports no match.

`cargo run --example recognize_url` exercises the URL hello-world end-to-end against the live API.

## Authentication

Pass the token as a string literal:

```rust,no_run
# use audd::AudD;
let audd = AudD::new("your-api-token");
# let _ = audd;
```

Or set `AUDD_API_TOKEN` in the environment and let the SDK pick it up:

```rust,no_run
# use audd::AudD;
let audd = AudD::from_env()?;       // errors if AUDD_API_TOKEN is unset
# Ok::<_, audd::AudDError>(())
```

For long-running services that rotate credentials, swap the token in place — the standard and enterprise transports both pick up the new value on their next request:

```rust,no_run
# use audd::AudD;
# fn run(audd: AudD, fresh: String) -> Result<(), audd::AudDError> {
audd.set_api_token(fresh)?;
# Ok(()) }
```

## What you get back

`recognize` returns `Option<RecognitionResult>`. `None` means no match; `Some(r)` carries the typed metadata. By default you get the core tags plus AudD's universal song link — no metadata-block opt-in needed:

```rust,no_run
use audd::{AudD, StreamingProvider};

#[tokio::main]
async fn main() -> Result<(), audd::AudDError> {
    let audd = AudD::new("your-api-token");
    let Some(r) = audd.recognize("https://audd.tech/example.mp3").await? else { return Ok(()) };

    // Core tags
    println!("{} — {}",  r.artist.as_deref().unwrap_or(""), r.title.as_deref().unwrap_or(""));
    println!("album:    {}", r.album.as_deref().unwrap_or(""));
    println!("released: {}", r.release_date.as_deref().unwrap_or(""));
    println!("label:    {}", r.label.as_deref().unwrap_or(""));
    println!("song_link {}", r.song_link.as_deref().unwrap_or(""));

    // Helpers — driven off song_link, work without any return opt-in
    if let Some(thumb) = r.thumbnail_url() { println!("cover art: {thumb}"); }
    if let Some(prev)  = r.preview_url()   { println!("preview:   {prev}"); }
    if let Some(url)   = r.streaming_url(StreamingProvider::Spotify) {
        println!("open in Spotify: {url}");
    }
    Ok(())
}
```

If you need provider-specific metadata blocks, opt in per call. Request only what you need — each provider you ask for adds latency:

```rust,no_run
# use audd::{AudD, RecognizeOptions};
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let return_metadata = ["apple_music".into(), "spotify".into()];
let Some(r) = audd
    .recognize_with("https://audd.tech/example.mp3", RecognizeOptions {
        return_metadata: Some(&return_metadata),
        ..Default::default()
    })
    .await? else { return Ok(()) };

if let Some(am) = r.apple_music.as_ref() {
    println!("apple_music: {} ({:?})", am.url.as_deref().unwrap_or(""), am.isrc);
}
if let Some(sp) = r.spotify.as_ref() {
    println!("spotify uri: {:?}", sp.uri);
}
# Ok(()) }
```

Valid `return_metadata` values: `apple_music`, `spotify`, `deezer`, `napster`, `musicbrainz`. The corresponding fields (`r.apple_music`, `r.spotify`, `r.deezer`, `r.napster`, `r.musicbrainz`) are `None` when not requested.

For additional form fields the typed options don't cover — undocumented parameters, beta features — every options struct (`RecognizeOptions`, `EnterpriseOptions`) carries an `extra_parameters: Option<&HashMap<String, String>>` field. Typed fields win on collision.

For long files (hours, days), use `recognize_enterprise` — it returns every match across every chunk:

```rust,no_run
# use audd::{AudD, EnterpriseOptions};
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let matches = audd.recognize_enterprise(
    "podcast.mp3",
    EnterpriseOptions { limit: Some(10), ..Default::default() },
).await?;
for m in matches { println!("{}: {} — {}", m.timecode,
    m.artist.as_deref().unwrap_or(""), m.title.as_deref().unwrap_or("")); }
# Ok(()) }
```

Each `EnterpriseMatch` carries the same core tags plus `score`, `start_offset`, `end_offset`, `isrc`, `upc`. Access to `isrc`, `upc`, and `score` requires a Startup plan or higher — [contact us](mailto:api@audd.io) for enterprise features.

## Reading additional metadata

Every typed model carries an `extras: HashMap<String, serde_json::Value>` populated via `#[serde(flatten)]`. It's the supported way to read undocumented metadata, beta fields, and any provider blocks the typed shape doesn't yet expose:

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let Some(r) = audd.recognize("https://audd.tech/example.mp3").await? else { return Ok(()) };
if let Some(tidal) = r.extras.get("tidal") { println!("tidal: {tidal}"); }
# Ok(()) }
```

Because every public model derives `Serialize` and `Deserialize`, you can round-trip recognition results through your own logs, queues, or columnar stores without losing typed fields *or* extras:

```rust,no_run
# use audd::RecognitionResult;
# fn run(r: RecognitionResult) -> Result<(), serde_json::Error> {
let bytes = serde_json::to_vec(&r)?;                     // → Kafka, S3, Postgres jsonb, …
let back: RecognitionResult = serde_json::from_slice(&bytes)?;  // typed again, extras intact
# Ok(()) }
```

## Errors

Every fallible method returns `Result<T, AudDError>`. The error type is a single enum with five variants — `Api`, `Server`, `Connection`, `Serialization`, `Source`, `Configuration` — propagate with `?` and dispatch on category via the helper predicates rather than matching on numeric error codes:

```rust,no_run
use audd::AudD;

# async fn run() -> Result<(), audd::AudDError> {
match AudD::new("bad").recognize("https://example.com/clip.mp3").await {
    Ok(_) => {}
    Err(e) if e.is_authentication() => eprintln!("check your token: {e}"),
    Err(e) if e.is_quota()          => eprintln!("monthly quota exhausted"),
    Err(e) if e.is_subscription()   => eprintln!("endpoint not enabled on your plan"),
    Err(e) if e.is_rate_limit()     => eprintln!("backing off — {e}"),
    Err(e) if e.is_invalid_audio()  => eprintln!("bad input audio — {e}"),
    Err(e) => return Err(e),
}
# Ok(()) }
```

The full predicate set: `is_authentication`, `is_quota`, `is_subscription`, `is_custom_catalog_access`, `is_invalid_request`, `is_invalid_audio`, `is_rate_limit`, `is_stream_limit`, `is_not_released`, `is_blocked`, `is_needs_update`, `is_api`. `AudDError::Api` carries `code`, `message`, `kind`, `http_status`, `request_id`, `requested_params`, `request_method`, `branded_message`, and the unparsed `raw_response` for advanced inspection.

## Configuration

`AudD::new` covers the common case. Use `AudD::builder` to tune retries, swap in a configured `reqwest::Client` (corp proxies, mTLS, custom CA bundles, observability sidecars), or override base URLs for testing:

```rust,no_run
use audd::AudD;

let client = reqwest::Client::builder()
    .proxy(reqwest::Proxy::all("http://corp-proxy:8080").unwrap())
    .build().unwrap();

let audd = AudD::builder("your-token")
    .max_attempts(3)        // retry budget per call (default 3; set to 1 to disable)
    .backoff_factor(0.5)    // initial backoff seconds, jittered (default 0.5)
    .reqwest_client(client) // shared by standard + enterprise endpoints
    .build()
    .unwrap();
# let _ = audd;
```

Default timeouts: 30 s connect / 60 s read on standard endpoints, 30 s connect / 1 h read on the enterprise endpoint. Override per call with `EnterpriseOptions { timeout: Some(_), .. }`.

## Choosing a TLS backend

The default build is `rustls` with the Mozilla CA bundle — pure-Rust, no system deps, the right choice for almost everyone. Switch to `native-tls` when you need OpenSSL (custom CA trust stores, OpenSSL FIPS, regulated environments) or `vendored-openssl` when the build host lacks `libssl-dev` / `openssl-devel` but you still want the OpenSSL stack at runtime.

| Feature | Default | What it does |
|---|---|---|
| `rustls-tls` | yes | Pure-Rust TLS via [`rustls`](https://github.com/rustls/rustls) + Mozilla roots |
| `native-tls` | no | Platform-native TLS — OpenSSL on Linux, SecureTransport on macOS, SChannel on Windows |
| `vendored-openssl` | no | OpenSSL via `native-tls`, statically linked from a vendored source build |

Pick exactly one. To opt out of `rustls` and use OpenSSL:

```toml
[dependencies]
audd = { version = "1.4", default-features = false, features = ["native-tls"] }
```

## Streams

Stream recognition turns AudD into a continuous monitor for an audio stream (internet radio, Twitch, YouTube live, raw HLS/Icecast) and notifies you for every recognized song. Set up streams once, then either receive matches via a callback URL or poll for them.

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let streams = audd.streams();

// 1. Tell AudD where to POST recognition results for your account.
streams.set_callback_url(
    "https://your.app/audd/callback",
    Some(&["apple_music".into(), "spotify".into()]),
).await?;

// 2. Add streams to monitor.
streams.add("https://example.com/radio.m3u8", 1, None).await?;
streams.add("twitch:somechannel", 2, None).await?;

// 3. Inspect what you have configured.
for s in streams.list().await? {
    println!("{} -> {} (running={})", s.radio_id, s.url, s.stream_running);
}
# Ok(()) }
```

Inside your callback receiver, parse the POST body into a typed event. The SDK is web-framework-agnostic — pull the body bytes from your framework of choice (`axum`, `actix-web`, `rocket`, `hyper`, …) and pass them to `handle_callback`:

```rust,no_run
use audd::{handle_callback, CallbackEvent};

# fn run(body: &[u8]) -> Result<(), audd::AudDError> {
match handle_callback(body)? {
    CallbackEvent::Match(m) => {
        println!("matched: {} — {}", m.song.artist, m.song.title);
        for alt in &m.alternatives {
            println!("  alt: {} — {}", alt.artist, alt.title);
        }
    }
    CallbackEvent::Notification(n) => {
        println!("notification: {}", n.notification_message);
    }
}
# Ok(()) }
```

`handle_callback(bytes)` accepts anything `AsRef<[u8]>`. If you already have parsed JSON (queue consumer, replay tool), use `parse_callback(value)` instead.

See [`examples/streams_callback_handler`](./examples/streams_callback_handler.rs) and [`examples/streams_setup`](./examples/streams_setup.rs) for runnable code.

### Receiving events without a callback URL (longpoll)

If hosting a callback receiver isn't an option, longpoll for events from the client side. The poll handle exposes three typed `Stream`s — matches, notifications, errors — drive them with a `tokio::select!` loop. Server keepalive ticks are silently absorbed:

```rust,no_run
# use audd::{AudD, LongpollOptions};
use futures_util::StreamExt;

# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let radio_id = 1i64; // any integer you choose — your handle for this stream
let mut poll = audd.streams().longpoll_by_radio_id(radio_id, LongpollOptions::default()).await?;

loop {
    tokio::select! {
        biased;
        Some(err) = poll.errors.next() => { eprintln!("longpoll error: {err}"); break; }
        Some(n)   = poll.notifications.next() => println!("notification: {}", n.notification_message),
        Some(m)   = poll.matches.next() => println!("matched: {} — {}", m.song.artist, m.song.title),
        else => break,
    }
}
poll.close().await;
# Ok(()) }
```

For browser widgets, embedded UIs, or anywhere you need to consume a category without leaking the API token, use the tokenless variant — same `LongpollPoll` handle, same select loop:

```rust,no_run
use audd::{LongpollConsumer, LongpollIterateOptions};
use futures_util::StreamExt;

# async fn run() -> Result<(), audd::AudDError> {
let consumer = LongpollConsumer::new("abc123def");
let mut poll = consumer.iterate(LongpollIterateOptions::default());
while let Some(m) = poll.matches.next().await {
    println!("matched: {} — {}", m.song.artist, m.song.title);
}
# Ok(()) }
```

`audd::derive_longpoll_category(token, radio_id)` is also available as a free function for computing categories on a server and shipping them to a frontend that runs `LongpollConsumer`.

## Custom catalog (advanced)

> **The custom-catalog endpoint is not how you submit audio for recognition.**
> For recognition, use [`AudD::recognize`] or [`AudD::recognize_enterprise`]. The custom-catalog
> endpoint adds songs to your private fingerprint database for *your* account, and requires
> separate access — contact api@audd.io if you need it.

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
audd.custom_catalog().add(42, "https://my.song.mp3").await?;
# Ok(()) }
```

## License

MIT — see [LICENSE](./LICENSE).

## Support

- Documentation: <https://docs.audd.io>
- Tokens: <https://dashboard.audd.io>
- Email: api@audd.io
