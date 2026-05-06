//! End-to-end-ish tests against a wiremock server.

use audd::client::EnterpriseOptions;
use audd::longpoll::LongpollIterateOptions;
use audd::streams::LongpollOptions;
use audd::{AudD, LongpollConsumer};
use futures_util::StreamExt;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn recognize_returns_match() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": {
                "artist": "Tears For Fears",
                "title": "Everybody Wants To Rule The World",
                "timecode": "00:56",
                "song_link": "https://lis.tn/NbkVb"
            }
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let r = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.artist.as_deref(), Some("Tears For Fears"));
}

#[tokio::test]
async fn recognize_no_match() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": null
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let r = audd.recognize("https://x.example/clip.mp3").await.unwrap();
    assert!(r.is_none());
}

#[tokio::test]
async fn recognize_authentication_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"error_code": 900, "error_message": "bad token"}
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("nope")
        .api_base(server.uri())
        .build()
        .unwrap();
    let e = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap_err();
    assert!(e.is_authentication());
}

#[tokio::test]
async fn recognize_5xx_with_html_is_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(502)
                .set_body_string("<html>bad gateway</html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .max_attempts(1)
        .build()
        .unwrap();
    let e = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap_err();
    match e {
        audd::AudDError::Server { http_status, .. } => assert_eq!(http_status, 502),
        other => panic!("not Server: {other:?}"),
    }
}

#[tokio::test]
async fn recognize_retries_on_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": {"artist": "X", "title": "Y", "timecode": "00:01"}
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .backoff_factor(0.0)
        .max_attempts(3)
        .build()
        .unwrap();
    let r = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.artist.as_deref(), Some("X"));
}

#[tokio::test]
async fn recognize_code_51_passes_through_with_warn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"error_code": 51, "error_message": "deprecated param X"},
            "result": {"artist": "Z", "title": "W", "timecode": "00:02"}
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let r = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.artist.as_deref(), Some("Z"));
}

#[tokio::test]
async fn streams_get_callback_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/getCallbackUrl/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": "https://example.com/cb"
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let url = audd.streams().get_callback_url().await.unwrap();
    assert_eq!(url, "https://example.com/cb");
}

#[tokio::test]
async fn streams_longpoll_preflight_no_callback_raises() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/getCallbackUrl/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"error_code": 19, "error_message": "no callback"}
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let res = audd
        .streams()
        .longpoll("cat", LongpollOptions::default())
        .await;
    let e = res.expect_err("should have failed");
    assert!(e.is_invalid_request(), "got {e:?}");
}

#[tokio::test]
async fn streams_longpoll_skip_check_runs_and_absorbs_keepalive() {
    // Server emits a keepalive then a real match. The keepalive must be
    // silently absorbed so only the match reaches `poll.matches`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeout": "no events before timeout",
            "timestamp": 1
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "radio_id": 9,
                "timestamp": "2026-05-04 10:31:43",
                "play_length": 30,
                "results": [{"artist": "X", "title": "Y", "score": 100}]
            },
            "timestamp": 2
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let mut poll = audd
        .streams()
        .longpoll(
            "cat",
            LongpollOptions::default()
                .skip_callback_check(true)
                .timeout(1),
        )
        .await
        .unwrap();
    let m = poll.matches.next().await.expect("should get a match");
    assert_eq!(m.radio_id, 9);
    assert_eq!(m.song.title, "Y");
    poll.close().await;
}

#[tokio::test]
async fn streams_longpoll_surfaces_notifications() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "notification": {
                "radio_id": 3,
                "stream_running": false,
                "notification_code": 650,
                "notification_message": "stream stopped"
            },
            "time": 1700000000,
            "timestamp": 1700000000
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let mut poll = audd
        .streams()
        .longpoll(
            "cat",
            LongpollOptions::default()
                .skip_callback_check(true)
                .timeout(1),
        )
        .await
        .unwrap();
    let n = poll
        .notifications
        .next()
        .await
        .expect("should get a notification");
    assert_eq!(n.notification_code, 650);
    assert_eq!(n.time, Some(1_700_000_000));
    poll.close().await;
}

#[tokio::test]
async fn enterprise_returns_matches() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": [
                {"songs": [{"score": 81, "timecode": "00:57", "artist": "X", "title": "Y", "isrc": "ABC", "upc": "123"}], "offset": "00:00"}
            ]
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .enterprise_base(server.uri())
        .build()
        .unwrap();
    let opts = EnterpriseOptions {
        limit: Some(1),
        ..Default::default()
    };
    let v = audd
        .recognize_enterprise("https://x.example/clip.mp3", opts)
        .await
        .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].score, 81);
}

#[tokio::test]
async fn tokenless_longpoll_absorbs_keepalive_and_yields_match() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timeout": "no events before timeout",
            "timestamp": 42
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": {
                "radio_id": 5,
                "results": [{"artist": "A", "title": "T", "score": 80}]
            },
            "timestamp": 43
        })))
        .mount(&server)
        .await;
    let consumer = LongpollConsumer::builder("abc")
        .base_url(format!("{}/longpoll/", server.uri()))
        .build()
        .unwrap();
    let mut poll = consumer.iterate(LongpollIterateOptions {
        timeout: 1,
        ..Default::default()
    });
    let m = poll.matches.next().await.expect("should get a match");
    assert_eq!(m.radio_id, 5);
    assert_eq!(m.song.artist, "A");
    poll.close().await;
}

#[tokio::test]
async fn tokenless_longpoll_500_raises_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/longpoll/"))
        .respond_with(ResponseTemplate::new(500).set_body_string("oh no"))
        .mount(&server)
        .await;
    let consumer = LongpollConsumer::builder("abc")
        .base_url(format!("{}/longpoll/", server.uri()))
        .max_attempts(1)
        .backoff_factor(0.0)
        .build()
        .unwrap();
    let mut poll = consumer.iterate(LongpollIterateOptions {
        timeout: 1,
        ..Default::default()
    });
    let e = poll.errors.next().await.expect("should surface an error");
    match e {
        audd::AudDError::Server { http_status, .. } => assert_eq!(http_status, 500),
        other => panic!("not Server: {other:?}"),
    }
    poll.close().await;
}

#[tokio::test]
async fn custom_catalog_904_overrides_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"error_code": 904, "error_message": "no access"}
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let e = audd
        .custom_catalog()
        .add(1, vec![1u8, 2, 3])
        .await
        .unwrap_err();
    assert!(e.is_custom_catalog_access());
    if let audd::AudDError::Api { message, .. } = e {
        assert!(message.contains("custom catalog"));
        assert!(message.contains("Server message: no access"));
    } else {
        panic!("expected Api");
    }
}

#[tokio::test]
async fn streams_set_callback_with_return_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/setCallbackUrl/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": "ok"
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    audd.streams()
        .set_callback_url(
            "https://example.com/cb",
            Some(&["apple_music".into(), "spotify".into()]),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn streams_set_callback_duplicate_return_rejected() {
    let audd = AudD::builder("test").build().unwrap();
    let e = audd
        .streams()
        .set_callback_url(
            "https://example.com/cb?return=apple_music",
            Some(&["spotify".into()]),
        )
        .await
        .unwrap_err();
    assert!(e.is_invalid_request());
}

#[tokio::test]
async fn advanced_find_lyrics() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/findLyrics/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": [
                {"artist": "X", "title": "Y", "lyrics": "..."},
            ]
        })))
        .mount(&server)
        .await;
    let audd = AudD::builder("test")
        .api_base(server.uri())
        .build()
        .unwrap();
    let v = audd.advanced().find_lyrics("rule").await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].artist, "X");
}

// ----- AudDEvent emission on real recognize round-trip -----

#[tokio::test]
async fn on_event_emits_request_then_response_around_recognize() {
    use audd::{AudDEvent, EventKind, OnEventHook};
    use std::sync::{Arc, Mutex};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "result": {"timecode": "00:01", "artist": "X", "title": "Y"}
        })))
        .mount(&server)
        .await;

    let captured: Arc<Mutex<Vec<AudDEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_hook = Arc::clone(&captured);
    let hook: OnEventHook = Arc::new(move |e: &AudDEvent| {
        captured_for_hook.lock().unwrap().push(e.clone());
    });
    let audd = AudD::builder("test-token-secret")
        .api_base(server.uri())
        .on_event(hook)
        .build()
        .unwrap();
    let r = audd
        .recognize("https://x.example/clip.mp3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.artist.as_deref(), Some("X"));

    let events = captured.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "expected exactly Request + Response, got {events:#?}"
    );
    assert_eq!(events[0].kind, EventKind::Request);
    assert_eq!(events[0].method, "recognize");
    assert!(events[0].url.starts_with(&server.uri()));

    assert_eq!(events[1].kind, EventKind::Response);
    assert_eq!(events[1].http_status, Some(200));

    // Token must NEVER appear in any captured event field.
    for e in events.iter() {
        let blob = format!("{e:?}");
        assert!(
            !blob.contains("test-token-secret"),
            "api_token leaked into AudDEvent: {blob}",
        );
    }
}
