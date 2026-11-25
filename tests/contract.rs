//! Validate the SDK's parsers against the canonical fixture set in
//! `audd-openapi/fixtures/`. Mirrors `audd-python/tests/contract/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use audd::{
    errors::raise_from_error_response_for_test, parse_callback, EnterpriseChunkResult,
    LongpollConsumer, LongpollIterateOptions, RecognitionResult,
};
use futures_util::{FutureExt, StreamExt};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixtures_dir() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("AUDD_OPENAPI_FIXTURES") {
        let p = PathBuf::from(env);
        if p.is_dir() {
            return Some(p);
        }
    }
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sibling = crate_dir.parent()?.join("audd-openapi").join("fixtures");
    if sibling.is_dir() {
        Some(sibling)
    } else {
        None
    }
}

fn load(name: &str) -> Option<Value> {
    let dir = fixtures_dir()?;
    let bytes = std::fs::read(dir.join(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

macro_rules! load_or_skip {
    ($name:expr) => {
        match load($name) {
            Some(v) => v,
            None => {
                eprintln!("skipping: fixture {} not found", $name);
                return;
            }
        }
    };
}

#[test]
fn recognize_basic() {
    let payload = load_or_skip!("recognize_basic.json");
    assert_eq!(payload["status"], "success");
    let result: RecognitionResult = serde_json::from_value(payload["result"].clone()).unwrap();
    assert!(result.artist.is_some());
    assert!(result.timecode.as_deref().is_some_and(|t| !t.is_empty()));
    assert!(result.is_public_match());
}

#[test]
fn recognize_with_metadata() {
    let payload = load_or_skip!("recognize_with_metadata.json");
    let result: RecognitionResult = serde_json::from_value(payload["result"].clone()).unwrap();
    assert!(result.apple_music.is_some());
    assert!(result.spotify.is_some() || result.musicbrainz.is_some());
}

#[test]
fn recognize_custom_match() {
    let payload = load_or_skip!("recognize_custom_match.json");
    let result: RecognitionResult = serde_json::from_value(payload["result"].clone()).unwrap();
    assert!(result.is_custom_match());
    assert!(result.audio_id.is_some());
    assert!(result.artist.is_none());
}

#[test]
fn enterprise_with_isrc_upc() {
    let payload = load_or_skip!("enterprise_with_isrc_upc.json");
    assert_eq!(payload["status"], "success");
    let chunks: Vec<EnterpriseChunkResult> =
        serde_json::from_value(payload["result"].clone()).unwrap();
    assert!(!chunks.is_empty());
    let songs = &chunks[0].songs;
    assert!(!songs.is_empty());
    let s = &songs[0];
    assert!(s.isrc.is_some());
    assert!(s.upc.is_some());
    assert!(s.score.is_none_or(|sc| sc >= 0));
}

#[test]
fn enterprise_custom_match() {
    let payload = load_or_skip!("enterprise_custom_match.json");
    assert_eq!(payload["status"], "success");
    let chunks: Vec<EnterpriseChunkResult> =
        serde_json::from_value(payload["result"].clone()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].songs.len(), 1);
    let s = &chunks[0].songs[0];
    assert_eq!(s.audio_id, Some(146));
    assert!(s.is_custom_match());
    assert!(!s.is_public_match());
    assert!(s.artist.is_none());
    assert!(!s.extras.contains_key("audio_id"));
}

#[test]
fn enterprise_public_match_has_no_audio_id() {
    let payload = load_or_skip!("enterprise_with_isrc_upc.json");
    let chunks: Vec<EnterpriseChunkResult> =
        serde_json::from_value(payload["result"].clone()).unwrap();
    let s = &chunks[0].songs[0];
    assert_eq!(s.audio_id, None);
    assert!(!s.is_custom_match());
    assert!(s.is_public_match());
}

#[test]
fn callback_with_result() {
    let payload = load_or_skip!("streams_callback_with_result.json");
    let ev = parse_callback(payload).unwrap();
    let m = ev.as_match().expect("should be a match");
    assert_eq!(m.radio_id, Some(7));
}

#[test]
fn callback_with_notification() {
    let payload = load_or_skip!("streams_callback_with_notification.json");
    let ev = parse_callback(payload).unwrap();
    let n = ev.as_notification().expect("should be a notification");
    assert_eq!(n.notification_code, Some(650));
}

#[test]
fn get_streams_empty() {
    let payload = load_or_skip!("getStreams_empty.json");
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["result"], serde_json::json!([]));
}

#[test]
fn longpoll_no_events() {
    let payload = load_or_skip!("longpoll_no_events.json");
    assert!(payload.get("timeout").is_some());
    assert!(payload.get("timestamp").is_some());
}

/// Serve `first` to the first longpoll request and `then` to every later one
/// through a tokenless consumer; return the poll handle and the mock server.
async fn longpoll_against(first: &Value, then: &Value) -> (audd::LongpollPoll, MockServer) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(first))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(then))
        .mount(&server)
        .await;
    let poll = LongpollConsumer::builder("abc123def")
        .base_url(format!("{}/longpoll/", server.uri()))
        .max_attempts(1)
        .build()
        .unwrap()
        .iterate(LongpollIterateOptions {
            timeout: 1,
            ..Default::default()
        });
    (poll, server)
}

/// `since_time` of the second longpoll request, once it has been sent.
async fn second_since_time(server: &MockServer) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let seen = server.received_requests().await.unwrap_or_default();
        if seen.len() >= 2 {
            return seen[1]
                .url
                .query_pairs()
                .find(|(k, _)| k == "since_time")
                .map(|(_, v)| v.into_owned());
        }
        assert!(Instant::now() < deadline, "no second longpoll request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn newest_event_timestamp(payload: &Value) -> i64 {
    payload["events"]
        .as_array()
        .expect("events list")
        .iter()
        .filter_map(|e| e["timestamp"].as_i64())
        .max()
        .expect("event timestamps")
}

#[tokio::test]
async fn longpoll_events_are_delivered() {
    let payload = load_or_skip!("longpoll_events.json");
    let quiet = json!({"timeout": "no events before timeout", "timestamp": 1});
    let expected: Vec<Value> = payload["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["data"]["result"].clone())
        .collect();
    let (mut poll, server) = longpoll_against(&payload, &quiet).await;
    for want in &expected {
        let got = tokio::time::timeout(Duration::from_secs(5), poll.matches.next())
            .await
            .expect("timed out")
            .expect("a match per event");
        assert_eq!(got.radio_id, want["radio_id"].as_i64());
        assert_eq!(
            got.song.artist.as_deref(),
            want["results"][0]["artist"].as_str()
        );
        assert_eq!(
            got.song.title.as_deref(),
            want["results"][0]["title"].as_str()
        );
    }
    assert_eq!(
        second_since_time(&server).await,
        Some(newest_event_timestamp(&payload).to_string())
    );
    poll.close().await;
}

#[tokio::test]
async fn longpoll_no_events_advances_since_time() {
    let payload = load_or_skip!("longpoll_no_events.json");
    let (mut poll, server) = longpoll_against(&payload, &payload).await;
    assert_eq!(
        second_since_time(&server).await,
        payload["timestamp"].as_i64().map(|t| t.to_string())
    );
    assert!(poll.matches.next().now_or_never().is_none());
    poll.close().await;
}

#[tokio::test]
async fn longpoll_error_is_an_invalid_request() {
    let payload = load_or_skip!("longpoll_error.json");
    let (mut poll, server) = longpoll_against(&payload, &payload).await;
    let err = tokio::time::timeout(Duration::from_secs(5), poll.errors.next())
        .await
        .expect("timed out")
        .expect("an error");
    assert!(err.is_invalid_request(), "got {err:?}");
    assert!(err.to_string().contains(payload["error"].as_str().unwrap()));
    assert!(poll.matches.next().await.is_none(), "polling stops");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn error_900_invalid_token() {
    let payload = load_or_skip!("error_900_invalid_token.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, false);
    assert!(e.is_authentication());
    assert_eq!(e.error_code(), Some(900));
}

#[test]
fn error_700_no_file() {
    let payload = load_or_skip!("error_700_no_file.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, false);
    assert!(e.is_invalid_request());
    assert_eq!(e.error_code(), Some(700));
}

#[test]
fn error_19_no_callback_url() {
    let payload = load_or_skip!("error_19_no_callback_url.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, false);
    assert!(e.is_blocked());
    assert_eq!(e.error_code(), Some(19));
}

#[test]
fn error_902_stream_limit() {
    let payload = load_or_skip!("error_902_stream_limit.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, false);
    assert!(e.is_quota());
    assert_eq!(e.error_code(), Some(902));
}

#[test]
fn error_904_enterprise_unauthorized() {
    let payload = load_or_skip!("error_904_enterprise_unauthorized.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, false);
    assert!(e.is_subscription());
    assert_eq!(e.error_code(), Some(904));
}

#[test]
fn error_904_custom_catalog_context() {
    let payload = load_or_skip!("error_904_enterprise_unauthorized.json");
    let e = raise_from_error_response_for_test(&payload, 200, None, /* custom_catalog */ true);
    assert!(e.is_custom_catalog_access());
}
