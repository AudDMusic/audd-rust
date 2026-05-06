# audd

[![CI](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/ci.yml)
[![Contract](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml/badge.svg)](https://github.com/AudDMusic/audd-rust/actions/workflows/contract.yml)
[![Crates.io](https://img.shields.io/crates/v/audd.svg)](https://crates.io/crates/audd)
[![docs.rs](https://docs.rs/audd/badge.svg)](https://docs.rs/audd)

Official Rust SDK for the [AudD](https://audd.io) music recognition API. Async only — built on `tokio` + `reqwest`.

## Quickstart

```toml
[dependencies]
audd = "1.4"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust,no_run
use audd::AudD;

#[tokio::main]
async fn main() -> Result<(), audd::AudDError> {
    let audd = AudD::new("test"); // use your token from https://dashboard.audd.io
    if let Some(r) = audd.recognize("https://audd.tech/example.mp3").await? {
        println!("{} — {}", r.artist.as_deref().unwrap_or(""), r.title.as_deref().unwrap_or(""));
    }
    Ok(())
}
```

`cargo run --example recognize_url` reproduces the hello-world end-to-end against the live API.

## Capabilities

| What | How |
|---|---|
| Recognize a short clip (≤25s) | `audd.recognize(source).await?` |
| Recognize a long file (hours, days) | `audd.recognize_enterprise(source, opts).await?` |
| Manage real-time stream recognition | `audd.streams().add(url, radio_id, None).await?` etc. |

`source` accepts `&str` (URL or path), `PathBuf`, `Vec<u8>`, or any `Source` variant — auto-detected via `Into<Source>`.

## Source variants

```rust,no_run
use audd::{AudD, Source};
use std::path::PathBuf;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {

// URL — sent as `data["url"]=...`
audd.recognize("https://audd.tech/example.mp3").await?;

// File path — opened fresh on every retry attempt
audd.recognize(Source::Path(PathBuf::from("clip.mp3"))).await?;

// Raw bytes
audd.recognize(Source::Bytes(b"...".to_vec())).await?;

// Async reader (buffered into memory; for very large sources use Path)
let reader = tokio::fs::File::open("clip.mp3").await.unwrap();
audd.recognize(Source::Reader(Box::new(reader))).await?;
# Ok(()) }
```

## Errors

Every server error becomes a typed [`AudDError`] variant. Use the helper predicates rather than matching on numeric codes:

```rust,no_run
use audd::{AudD, AudDError};

# async fn run() -> Result<(), audd::AudDError> {
match AudD::new("bad").recognize("https://x.mp3").await {
    Ok(_) => {}
    Err(e) if e.is_authentication() => println!("check your token: {}", e),
    Err(e) if e.is_subscription() => println!("this endpoint isn't enabled on your plan"),
    Err(e) => return Err(e),
}
# Ok(()) }
```

The helpers are: `is_authentication`, `is_quota`, `is_subscription`, `is_custom_catalog_access`, `is_invalid_request`, `is_invalid_audio`, `is_rate_limit`, `is_stream_limit`, `is_not_released`, `is_blocked`, `is_needs_update`, `is_api`. Every `AudDError::Api` variant carries `code`, `message`, `kind`, `http_status`, `request_id`, `requested_params`, `request_method`, `branded_message`, and `raw_response`.

## Forward compatibility

Models accept and round-trip unknown server fields via `extras`:

```rust,no_run
# use audd::AudD;
# async fn run() -> Result<(), audd::AudDError> {
let audd = AudD::new("test");
let result = audd.recognize("https://example.mp3").await?.unwrap();
println!("{:?}", result.apple_music);              // typed
println!("{:?}", result.extras.get("tidal"));       // anything new the server adds
# Ok(()) }
```

If AudD adds a new metadata block tomorrow, you can read it as `result.extras["tidal"]` *today* — no SDK release needed.

Every public model derives `Serialize` + `Deserialize`, so you can round-trip recognition results into your own logs / queues / databases:

```rust,no_run
# use audd::RecognitionResult;
# fn run(r: RecognitionResult) -> Result<(), serde_json::Error> {
let bytes = serde_json::to_vec(&r)?;        // → write to Kafka, S3, Postgres jsonb, …
let back: RecognitionResult = serde_json::from_slice(&bytes)?;  // → typed again, with extras intact
# Ok(()) }
```

## Configuration

```rust,no_run
use audd::AudD;
let client = reqwest::Client::builder()
    .proxy(reqwest::Proxy::all("http://corp-proxy:8080").unwrap())
    .build().unwrap();
let audd = AudD::builder("...")
    .max_attempts(3)            // retry budget per call (default 3)
    .backoff_factor(0.5)        // initial backoff seconds, jittered (default 0.5)
    .reqwest_client(client)     // inject a configured client (proxy, mTLS, etc.)
    .build()
    .unwrap();
```

Default timeouts: 30s connect / 60s read for standard endpoints, 30s connect / **1 hour** read for the enterprise endpoint. Pass `EnterpriseOptions { timeout: Some(_), .. }` per call to override.

## Choosing a TLS backend

The default build uses [`rustls`] with the Mozilla CA bundle — pure-Rust, no system dependencies, the right choice for most users. If you need OpenSSL (corp environments with custom CA trust stores, OpenSSL FIPS, regulatory constraints), opt into the `native-tls` feature instead:

```toml
[dependencies]
audd = { version = "1.4", default-features = false, features = ["native-tls"] }
```

`native-tls` links against the system OpenSSL (via `libssl-dev` on Debian/Ubuntu, `openssl-devel` on Fedora). If the build host lacks those development headers but you still need the OpenSSL TLS stack at runtime, use `vendored-openssl` instead — it statically links a vendored copy of OpenSSL:

```toml
[dependencies]
audd = { version = "1.4", default-features = false, features = ["vendored-openssl"] }
```

Available features:

| Feature | Default? | What it does |
|---|---|---|
| `rustls-tls` | yes | Pure-Rust TLS via `rustls` + Mozilla CA bundle |
| `native-tls` | no | Platform-native TLS (OpenSSL on Linux, SecureTransport on macOS, SChannel on Windows) |
| `vendored-openssl` | no | OpenSSL via `native-tls`, statically linked from a vendored source build |

Pick exactly one TLS backend; mixing `rustls-tls` with `native-tls` works (reqwest accepts both) but ships duplicate transport stacks.

[`rustls`]: https://github.com/rustls/rustls

## Discriminating public vs custom matches

```rust,no_run
use audd::{RecognitionMatch, RecognitionResult};
# fn run(r: RecognitionResult) {
match RecognitionMatch::from(r) {
    RecognitionMatch::Public(p) => println!("public: {} — {}", p.artist.unwrap(), p.title.unwrap()),
    RecognitionMatch::Custom(c) => println!("custom audio_id={}", c.audio_id.unwrap()),
}
# }
```

## Custom catalog (advanced — not for music recognition)

> ⚠ **The custom-catalog endpoint is NOT how you submit audio for music recognition.**
> For recognition, use [`AudD::recognize`] or [`AudD::recognize_enterprise`]. The custom-catalog
> endpoint adds songs to your private fingerprint database for *your* account.
> Requires special access — contact api@audd.io if you need it.

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
audd.custom_catalog().add(42, "https://my.song.mp3").await?;
# Ok(()) }
```

## Advanced

For lyrics search and a generic raw-request escape hatch:

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let hits = audd.advanced().find_lyrics("rule the world").await?;
let raw = audd.advanced().raw_request("someNewMethod", &[("q", "x".into())]).await?;
# Ok(()) }
```

## Streams

Real-time recognition over an arbitrary streaming URL — the SDK manages add/list/delete and exposes a callback parser plus two longpoll modes.

```rust,no_run
# use audd::AudD;
# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
audd.streams().add("https://stream.example.com/live", 1234, None).await?;
# Ok(()) }
```

### Longpoll

```rust,no_run
# use audd::{AudD, LongpollOptions};
use futures_util::StreamExt;

# async fn run(audd: AudD) -> Result<(), audd::AudDError> {
let category = audd.streams().derive_longpoll_category(1234);
let mut events = audd.streams().longpoll(&category, LongpollOptions::default()).await?;
while let Some(ev) = events.next().await {
    println!("{:?}", ev?);
}
# Ok(()) }
```

### Tokenless longpoll

For browser widgets, Twitch extensions, and other contexts where shipping the api_token would leak it:

```rust,no_run
use audd::{LongpollConsumer, LongpollIterateOptions};
use futures_util::StreamExt;

# async fn run() -> Result<(), audd::AudDError> {
// `category` is derived server-side via `audd.streams().derive_longpoll_category(radio_id)`,
// then shared with the browser/widget. The consumer carries no api_token.
let consumer = LongpollConsumer::new("abc123def");
let mut events = consumer.iterate(LongpollIterateOptions { timeout: 30, ..Default::default() });
while let Some(ev) = events.next().await {
    println!("{:?}", ev?);
}
# Ok(()) }
```

The consumer treats HTTP non-2xx responses as `AudDError::Server` (with the status preserved) instead of looping silently — important for browser deployments where the upstream might be misconfigured.

## Spec contract

This SDK builds against the [`audd-openapi`](https://github.com/AudDMusic/audd-openapi) spec. The contract tests in `tests/contract.rs` validate the parser against the canonical fixture set on every push, on a daily cron, and on every spec update.

## License

MIT — see [LICENSE](./LICENSE).

## Support

- Documentation: <https://docs.audd.io>
- Tokens: <https://dashboard.audd.io>
- Email: api@audd.io
