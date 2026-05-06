//! Pure helpers: longpoll category derivation, callback parsing, return-URL builder.
//! No HTTP or SDK state.

use serde_json::Value;
use url::Url;

use crate::errors::AudDError;
use crate::models::StreamCallbackPayload;

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

/// Parse a callback POST body that AudD sent to your webhook. Returns a typed
/// payload that's either a recognition result or a notification.
///
/// # Errors
///
/// Returns [`AudDError::Serialization`] if the inner objects don't deserialize
/// into the typed shapes.
pub fn parse_callback(body: Value) -> Result<StreamCallbackPayload, AudDError> {
    StreamCallbackPayload::parse(body).map_err(|e| AudDError::Serialization {
        message: format!("invalid callback payload: {e}"),
        raw_text: String::new(),
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
    fn parse_callback_works() {
        let body = serde_json::json!({
            "status": "success",
            "result": {
                "radio_id": 7,
                "results": [{"artist": "X", "title": "Y", "score": 100}]
            }
        });
        let p = parse_callback(body).unwrap();
        assert!(p.is_result());
    }
}
