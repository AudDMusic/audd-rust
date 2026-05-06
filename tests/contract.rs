//! Validate the SDK's parsers against the canonical fixture set in
//! `audd-openapi/fixtures/`. Mirrors `audd-python/tests/contract/`.

use std::path::{Path, PathBuf};

use audd::{
    errors::raise_from_error_response_for_test, parse_callback, EnterpriseChunkResult,
    RecognitionResult,
};
use serde_json::Value;

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
    assert!(!result.timecode.is_empty());
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
    assert!(s.score >= 0);
}

#[test]
fn callback_with_result() {
    let payload = load_or_skip!("streams_callback_with_result.json");
    let ev = parse_callback(payload).unwrap();
    let m = ev.as_match().expect("should be a match");
    assert_eq!(m.radio_id, 7);
}

#[test]
fn callback_with_notification() {
    let payload = load_or_skip!("streams_callback_with_notification.json");
    let ev = parse_callback(payload).unwrap();
    let n = ev.as_notification().expect("should be a notification");
    assert_eq!(n.notification_code, 650);
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
