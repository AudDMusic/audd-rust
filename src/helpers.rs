//! Pure helpers: longpoll category derivation, callback parsing, return-URL builder.
//! No HTTP or SDK state.

use serde_json::Value;
use url::Url;

use crate::errors::AudDError;
use crate::models::{CallbackEvent, StreamCallbackMatch, StreamCallbackNotification};

/// Compute the 9-char longpoll category locally from `(api_token, radio_id)`.
///
/// Formula (per docs.audd.io/streams.md): `MD5(MD5(api_token) || str(radio_id))[:9]`.
#[must_use]
pub fn derive_longpoll_category(api_token: &str, radio_id: i64) -> String {
    let inner = format!("{:x}", md5::compute(api_token.as_bytes()));
    let combined = format!("{inner}{radio_id}");
    let outer = format!("{:x}", md5::compute(combined.as_bytes()));
    outer[..9].to_string()
}

/// Parse raw callback POST bytes into a typed [`CallbackEvent`].
///
/// Recognition callbacks have an outer `result` block; lifecycle-notification
/// callbacks have a `notification` block. The discriminator is by-key.
///
/// Web frameworks differ on how to extract the request body — `axum`,
/// `actix-web`, `rocket`, `hyper`, etc. all expose it through different APIs.
/// Rather than depend on any one of them, this function takes raw bytes that
/// you've already extracted from your framework's request type. See the
/// `streams_callback_handler` example for an end-to-end skeleton.
///
/// # Errors
///
/// Returns [`AudDError::Serialization`] if the body isn't valid JSON, doesn't
/// match either shape, or has an empty `result.results` array.
pub fn handle_callback(body: impl AsRef<[u8]>) -> Result<CallbackEvent, AudDError> {
    let bytes = body.as_ref();
    let value: Value = serde_json::from_slice(bytes).map_err(|e| AudDError::Serialization {
        message: format!("callback body is not valid JSON: {e}"),
        raw_text: String::from_utf8_lossy(bytes).into_owned(),
    })?;
    parse_callback(value)
}

/// Parse an already-deserialized JSON callback body into a typed
/// [`CallbackEvent`].
///
/// Prefer [`handle_callback`] when you have the raw bytes from your framework
/// — it surfaces the original payload in error messages. Use this entry point
/// for unusual transports (queue consumers, replay tools).
///
/// # Errors
///
/// Returns [`AudDError::Serialization`] if the body doesn't match either the
/// recognition or notification shape.
pub fn parse_callback(body: Value) -> Result<CallbackEvent, AudDError> {
    if let Some(notif_val) = body.get("notification").cloned() {
        let mut notif: StreamCallbackNotification =
            serde_json::from_value(notif_val).map_err(|e| AudDError::Serialization {
                message: format!("callback notification: {e}"),
                raw_text: body.to_string(),
            })?;
        notif.time = body.get("time").and_then(Value::as_i64);
        notif.raw_response = body;
        return Ok(CallbackEvent::Notification(notif));
    }

    if let Some(result_val) = body.get("result").cloned() {
        let mut m: StreamCallbackMatch =
            serde_json::from_value(result_val).map_err(|e| AudDError::Serialization {
                message: format!("callback result: {e}"),
                raw_text: body.to_string(),
            })?;
        m.raw_response = body;
        return Ok(CallbackEvent::Match(m));
    }

    Err(AudDError::Serialization {
        message: "callback body has neither `result` nor `notification`".into(),
        raw_text: body.to_string(),
    })
}

/// Append `?return=<metadata>` (or merge as `&return=`) to a callback URL.
///
/// If `return_metadata` is `None`, returns the URL unchanged. If the URL
/// already carries a `return` query parameter, returns an
/// [`AudDError::Api`] with [`ErrorKind::InvalidRequest`][crate::errors::ErrorKind::InvalidRequest]
/// rather than silently overwriting.
///
/// # Errors
///
/// Returns [`AudDError::Source`] if the URL is unparseable, or
/// [`AudDError::Api`] (synthetic invalid-request) if the URL already has a
/// `return=` parameter.
pub fn add_return_to_url(
    url: &str,
    return_metadata: Option<&[String]>,
) -> Result<String, AudDError> {
    let metadata = match return_metadata {
        None => return Ok(url.to_string()),
        Some(parts) if parts.is_empty() => return Ok(url.to_string()),
        Some(parts) => parts.join(","),
    };

    let mut parsed = Url::parse(url)
        .map_err(|e| AudDError::Source(format!("could not parse callback URL `{url}`: {e}")))?;
    if parsed.query_pairs().any(|(k, _)| k == "return") {
        return Err(duplicate_return_error());
    }
    parsed.query_pairs_mut().append_pair("return", &metadata);
    Ok(parsed.to_string())
}

fn duplicate_return_error() -> AudDError {
    use std::collections::HashMap;
    AudDError::Api {
        code: 0,
        message: "URL already contains a `return` query parameter; pass return_metadata=None or remove the parameter from the URL — refusing to silently overwrite.".to_string(),
        kind: crate::errors::ErrorKind::InvalidRequest,
        http_status: 0,
        request_id: None,
        requested_params: HashMap::new(),
        request_method: None,
        branded_message: None,
        raw_response: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_category_is_nine_hex_chars() {
        let c = derive_longpoll_category("test", 7);
        assert_eq!(c.len(), 9);
        assert!(c.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn derive_category_is_deterministic() {
        let a = derive_longpoll_category("abc", 1);
        let b = derive_longpoll_category("abc", 1);
        assert_eq!(a, b);
        let c = derive_longpoll_category("abc", 2);
        assert_ne!(a, c);
    }

    #[test]
    fn add_return_appends() {
        let out = add_return_to_url(
            "https://example.com/cb",
            Some(&["apple_music".into(), "spotify".into()]),
        )
        .unwrap();
        assert!(
            out.contains("return=apple_music%2Cspotify")
                || out.contains("return=apple_music,spotify"),
            "got {out}"
        );
    }

    #[test]
    fn add_return_merges_with_existing_query() {
        let out =
            add_return_to_url("https://example.com/cb?utm=x", Some(&["spotify".into()])).unwrap();
        assert!(out.contains("utm=x"));
        assert!(out.contains("return=spotify"));
    }

    #[test]
    fn add_return_rejects_duplicate() {
        let err = add_return_to_url(
            "https://example.com/cb?return=apple_music",
            Some(&["spotify".into()]),
        )
        .unwrap_err();
        assert!(err.is_invalid_request());
    }

    #[test]
    fn add_return_no_metadata_passthrough() {
        let out = add_return_to_url("https://example.com/cb", None).unwrap();
        assert_eq!(out, "https://example.com/cb");
    }

    #[test]
    fn parse_callback_match() {
        let body = serde_json::json!({
            "status": "success",
            "result": {
                "radio_id": 7,
                "results": [{"artist": "X", "title": "Y", "score": 100}]
            }
        });
        let ev = parse_callback(body).unwrap();
        let m = ev.as_match().expect("should be a match");
        assert_eq!(m.radio_id, 7);
        assert_eq!(m.song.title, "Y");
        assert!(m.alternatives.is_empty());
    }

    #[test]
    fn parse_callback_notification() {
        let body = serde_json::json!({
            "status": "-",
            "notification": {
                "radio_id": 3,
                "stream_running": false,
                "notification_code": 650,
                "notification_message": "x"
            },
            "time": 1
        });
        let ev = parse_callback(body).unwrap();
        let n = ev.as_notification().expect("should be a notification");
        assert_eq!(n.radio_id, 3);
        assert_eq!(n.notification_code, 650);
        assert_eq!(n.time, Some(1));
    }

    #[test]
    fn handle_callback_parses_raw_bytes() {
        let bytes =
            br#"{"result":{"radio_id":1,"results":[{"artist":"X","title":"Y","score":50}]}}"#;
        let ev = handle_callback(bytes.as_slice()).unwrap();
        assert_eq!(ev.as_match().unwrap().song.score, 50);
    }

    #[test]
    fn handle_callback_invalid_json_is_serialization_error() {
        let bytes = b"not json";
        let err = handle_callback(bytes.as_slice()).unwrap_err();
        match err {
            AudDError::Serialization { message, raw_text } => {
                assert!(message.contains("not valid JSON"));
                assert_eq!(raw_text, "not json");
            }
            other => panic!("expected Serialization, got {other:?}"),
        }
    }

    #[test]
    fn parse_callback_neither_shape_errors() {
        let body = serde_json::json!({"status": "success"});
        let err = parse_callback(body).unwrap_err();
        assert!(matches!(err, AudDError::Serialization { .. }));
    }
}
