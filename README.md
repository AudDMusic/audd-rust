# audd

[![CI](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml)
[![Contract](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml)
[![Crates.io](https://img.shields.io/crates/v/audd.svg)](https://crates.io/crates/audd)
[![docs.rs](https://docs.rs/audd/badge.svg)](https://docs.rs/audd)

Official Rust crate for [AudD](https://audd.io) — music recognition from a short audio clip, a long audio file, or a live stream.

The [API itself](https://docs.audd.io) is so simple that it can be easily used even without an SDK.

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
    let audd = AudD::new("test");
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
    let audd = AudD::new("test");
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
    let audd = AudD::new("test");
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
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let return_ = ["apple_music".into(), "spotify".into()];
let Some(r) = audd
    .recognize_with("https://audd.tech/example.mp3", Some(&return_), None, None)
    .await? else { return Ok(()) };

if let Some(am) = r.apple_music.as_ref() {
    println!("apple_music: {} ({:?})", am.url.as_deref().unwrap_or(""), am.isrc);
}
if let Some(sp) = r.spotify.as_ref() {
    println!("spotify uri: {:?}", sp.uri);
}
# Ok(()) }
```

Valid `return` values: `apple_music`, `spotify`, `deezer`, `napster`, `musicbrainz`. The corresponding fields (`r.apple_music`, `r.spotify`, `r.deezer`, `r.napster`, `r.musicbrainz`) are `None` when not requested.

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

Real-time recognition over an arbitrary streaming URL — the SDK manages add/list/delete and exposes a callback parser plus longpoll consumers.

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
audd.streams().add("https://stream.example.com/live", 1234, None).await?;
let rows = audd.streams().list().await?;
for s in rows { println!("{} -> {} (running={})", s.radio_id, s.url, s.stream_running); }
# Ok(()) }
```

`audd.streams().parse_callback(body)` turns the JSON body POSTed to your callback URL into a typed `StreamCallbackPayload` (recognition result *or* notification — discriminated).

### Longpoll

Pull recognition events instead of receiving callbacks. `derive_longpoll_category` produces the per-stream category that the longpoll endpoint listens on:

```rust,no_run
# use audd::{AudD, LongpollOptions};
use futures_util::StreamExt;

# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let category = audd.streams().derive_longpoll_category(1234);
let mut events = audd.streams().longpoll(&category, LongpollOptions::default()).await?;
while let Some(ev) = events.next().await { println!("{:?}", ev?); }
# Ok(()) }
```

#### Tokenless longpoll

For browser widgets, Twitch extensions, and other clients where shipping the api_token would leak it. The category is derived server-side and shared with the client; the consumer carries no token:

```rust,no_run
use audd::{LongpollConsumer, LongpollIterateOptions};
use futures_util::StreamExt;

# async fn run() -> Result<(), audd::AudDError> {
let consumer = LongpollConsumer::new("abc123def");
let mut events = consumer.iterate(LongpollIterateOptions { timeout: 30, ..Default::default() });
while let Some(ev) = events.next().await { println!("{:?}", ev?); }
# Ok(()) }
```

The consumer surfaces upstream HTTP non-2xx as `AudDError::Server` (with status preserved) instead of looping silently — important for browser deployments where the upstream might be misconfigured.

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
