//! Typed error enum for the AudD SDK. Mirrors the audd-python exception hierarchy
//! projected into a single Rust enum.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

/// Mapping from AudD numeric error codes to a semantic [`ErrorKind`].
///
/// The kind tells callers what *category* of error they're looking at (auth,
/// quota, subscription, rate-limit, ...) without forcing them to remember
/// numeric codes. New codes default to [`ErrorKind::ServerError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Codes 900, 901, 903 — token problems.
    Authentication,
    /// Code 902 — quota / per-copy limit reached.
    Quota,
    /// Codes 904, 905 — endpoint not available with this token's plan.
    Subscription,
    /// Code 904 raised from `custom_catalog.*` specifically — overridden message.
    CustomCatalogAccess,
    /// Codes 50, 51, 600/601/602, 700/701/702, 906 — caller's input was bad.
    InvalidRequest,
    /// Codes 300, 400, 500 — caller's audio file is the problem.
    InvalidAudio,
    /// Code 611 (and HTTP 429) — rate limit hit.
    RateLimit,
    /// Code 610 — stream-slot subscription limit exhausted.
    StreamLimit,
    /// Code 907 — song hasn't been released yet.
    NotReleased,
    /// Codes 19 + 31337 — security / abuse / sanctions / IP ban / maintenance.
    Blocked,
    /// Code 20 — caller's app needs an update / paid version required.
    NeedsUpdate,
    /// Codes 100, 1000, unknown codes, generic upstream failures.
    ServerError,
}

impl ErrorKind {
    /// Map an AudD error code to its semantic kind. Unknown → [`Self::ServerError`].
    #[must_use]
    pub fn from_code(code: i32) -> Self {
        match code {
            900 | 901 | 903 => Self::Authentication,
            902 => Self::Quota,
            904 | 905 => Self::Subscription,
            50 | 51 | 600 | 601 | 602 | 700 | 701 | 702 | 906 => Self::InvalidRequest,
            300 | 400 | 500 => Self::InvalidAudio,
            610 => Self::StreamLimit,
            611 => Self::RateLimit,
            907 => Self::NotReleased,
            19 | 31337 => Self::Blocked,
            20 => Self::NeedsUpdate,
            _ => Self::ServerError,
        }
    }
}

/// Public alias for [`ErrorKind::from_code`].
#[must_use]
pub fn error_for_code(code: i32) -> ErrorKind {
    ErrorKind::from_code(code)
}

/// Errors raised by the AudD SDK.
#[derive(Debug, thiserror::Error)]
pub enum AudDError {
    /// Server returned `status=error`. Carries the AudD error code + the
    /// echo'd request fields.
    #[error("[#{code}] {message}")]
    Api {
        /// AudD numeric error code.
        code: i32,
        /// Server's human-readable error message.
        message: String,
        /// Semantic kind (derived from `code`, with custom-catalog override).
        kind: ErrorKind,
        /// HTTP status the server returned alongside the JSON body.
        http_status: u16,
        /// `x-request-id` header value, if the server emitted one.
        request_id: Option<String>,
        /// Server's redacted echo of the inputs (handles both `request_params`
        /// and `requested_params` field names — endpoints disagree).
        requested_params: HashMap<String, Value>,
        /// Server's `request_api_method` echo, informational.
        request_method: Option<String>,
        /// Branded artist/title text from `result` on security/abuse errors,
        /// surfaced here rather than as a recognition result.
        branded_message: Option<String>,
        /// Full unparsed payload for advanced inspection.
        raw_response: Value,
    },

    /// Server returned an HTTP non-2xx with a non-JSON body
    /// (e.g., 502 with an HTML error page from an upstream gateway).
    #[error("HTTP {http_status}: {message}")]
    Server {
        /// HTTP status from the upstream response.
        http_status: u16,
        /// Synthetic message ("HTTP 502 with non-JSON response body" etc).
        message: String,
        /// Request-ID header if present.
        request_id: Option<String>,
        /// Raw response body (often HTML on edge errors).
        raw_response: String,
    },

    /// Network / TLS / timeout — no response received.
    #[error("connection error: {message}")]
    Connection {
        /// Synthetic message describing the failure.
        message: String,
        /// Underlying source.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// 2xx response with malformed JSON (or unexpectedly-shaped JSON).
    #[error("could not parse response: {message}")]
    Serialization {
        /// Synthetic message describing the failure.
        message: String,
        /// Original response body, when available.
        raw_text: String,
    },

    /// Caller misuse of the SDK (bad source, retry against unseekable reader, ...).
    #[error("invalid source: {0}")]
    Source(String),

    /// Construction-time misconfiguration (e.g., no api_token supplied and
    /// `AUDD_API_TOKEN` unset; rotation called with an empty string).
    #[error("configuration error: {message}")]
    Configuration {
        /// Human-readable description of the configuration problem.
        message: String,
    },
}

impl AudDError {
    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::Authentication`].
    #[must_use]
    pub fn is_authentication(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::Authentication,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind [`ErrorKind::Quota`].
    #[must_use]
    pub fn is_quota(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::Quota,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::Subscription`] or [`ErrorKind::CustomCatalogAccess`].
    #[must_use]
    pub fn is_subscription(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::Subscription | ErrorKind::CustomCatalogAccess,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::CustomCatalogAccess`].
    #[must_use]
    pub fn is_custom_catalog_access(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::CustomCatalogAccess,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::InvalidRequest`].
    #[must_use]
    pub fn is_invalid_request(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::InvalidRequest,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::InvalidAudio`].
    #[must_use]
    pub fn is_invalid_audio(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::InvalidAudio,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::RateLimit`].
    #[must_use]
    pub fn is_rate_limit(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::RateLimit,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::StreamLimit`].
    #[must_use]
    pub fn is_stream_limit(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::StreamLimit,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::NotReleased`].
    #[must_use]
    pub fn is_not_released(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::NotReleased,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::Blocked`].
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::Blocked,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of kind
    /// [`ErrorKind::NeedsUpdate`].
    #[must_use]
    pub fn is_needs_update(&self) -> bool {
        matches!(
            self,
            Self::Api {
                kind: ErrorKind::NeedsUpdate,
                ..
            }
        )
    }

    /// `true` if this error is an [`AudDError::Api`] of any server-side category.
    #[must_use]
    pub fn is_api(&self) -> bool {
        matches!(self, Self::Api { .. })
    }

    /// AudD error code, if this is an API error.
    #[must_use]
    pub fn error_code(&self) -> Option<i32> {
        if let Self::Api { code, .. } = self {
            Some(*code)
        } else {
            None
        }
    }

    /// Server's `x-request-id` header value, if present.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } | Self::Server { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }
}

/// Build an [`AudDError::Api`] from a server `status: error` body. Exposed
/// at the crate root under a `_for_test` alias for integration tests.
#[doc(hidden)]
pub fn raise_from_error_response_for_test(
    body: &Value,
    http_status: u16,
    request_id: Option<String>,
    custom_catalog_context: bool,
) -> AudDError {
    raise_from_error_response(body, http_status, request_id, custom_catalog_context)
}

/// Build an [`AudDError::Api`] from a server `status: error` body.
pub(crate) fn raise_from_error_response(
    body: &Value,
    http_status: u16,
    request_id: Option<String>,
    custom_catalog_context: bool,
) -> AudDError {
    let err_obj = body.get("error").and_then(Value::as_object);
    let code = err_obj
        .and_then(|o| o.get("error_code"))
        .and_then(coerce_i32)
        .unwrap_or(0);
    let message = err_obj
        .and_then(|o| o.get("error_message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let requested_params = body
        .get("request_params")
        .or_else(|| body.get("requested_params"))
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let request_method = body
        .get("request_api_method")
        .and_then(Value::as_str)
        .map(String::from);
    let branded_message = branded_message(body.get("result"));

    let mut kind = ErrorKind::from_code(code);
    if custom_catalog_context && kind == ErrorKind::Subscription {
        kind = ErrorKind::CustomCatalogAccess;
    }

    let final_message = if kind == ErrorKind::CustomCatalogAccess {
        custom_catalog_message(&message)
    } else {
        message
    };

    AudDError::Api {
        code,
        message: final_message,
        kind,
        http_status,
        request_id,
        requested_params,
        request_method,
        branded_message,
        raw_response: body.clone(),
    }
}

fn coerce_i32(v: &Value) -> Option<i32> {
    if let Some(n) = v.as_i64() {
        return i32::try_from(n).ok();
    }
    if let Some(s) = v.as_str() {
        return s.parse().ok();
    }
    None
}

fn branded_message(result: Option<&Value>) -> Option<String> {
    let obj = result?.as_object()?;
    let artist = obj.get("artist").and_then(Value::as_str);
    let title = obj.get("title").and_then(Value::as_str);
    match (artist, title) {
        (Some(a), Some(t)) if !a.is_empty() && !t.is_empty() => Some(format!("{a} — {t}")),
        (Some(a), _) if !a.is_empty() => Some(a.to_string()),
        (_, Some(t)) if !t.is_empty() => Some(t.to_string()),
        _ => None,
    }
}

fn custom_catalog_message(server_message: &str) -> String {
    format!(
        "Adding songs to your custom catalog requires enterprise access that isn't \
enabled on your account.\n\n\
Note: the custom-catalog endpoint is for adding songs to your private \
fingerprint database, not for music recognition. If you intended to \
identify music, use recognize(...) (or recognize_enterprise(...) for \
files longer than 25 seconds) instead.\n\n\
To request custom-catalog access, contact api@audd.io.\n\n\
[Server message: {server_message}]"
    )
}

/// Convenience for short-formatting an `AudDError::Api`'s code+kind in test
/// output and logs.
impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Authentication => "authentication",
            Self::Quota => "quota",
            Self::Subscription => "subscription",
            Self::CustomCatalogAccess => "custom-catalog-access",
            Self::InvalidRequest => "invalid-request",
            Self::InvalidAudio => "invalid-audio",
            Self::RateLimit => "rate-limit",
            Self::StreamLimit => "stream-limit",
            Self::NotReleased => "not-released",
            Self::Blocked => "blocked",
            Self::NeedsUpdate => "needs-update",
            Self::ServerError => "server-error",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_to_kind() {
        assert_eq!(ErrorKind::from_code(900), ErrorKind::Authentication);
        assert_eq!(ErrorKind::from_code(902), ErrorKind::Quota);
        assert_eq!(ErrorKind::from_code(904), ErrorKind::Subscription);
        assert_eq!(ErrorKind::from_code(700), ErrorKind::InvalidRequest);
        assert_eq!(ErrorKind::from_code(400), ErrorKind::InvalidAudio);
        assert_eq!(ErrorKind::from_code(610), ErrorKind::StreamLimit);
        assert_eq!(ErrorKind::from_code(611), ErrorKind::RateLimit);
        assert_eq!(ErrorKind::from_code(907), ErrorKind::NotReleased);
        assert_eq!(ErrorKind::from_code(19), ErrorKind::Blocked);
        assert_eq!(ErrorKind::from_code(31337), ErrorKind::Blocked);
        assert_eq!(ErrorKind::from_code(20), ErrorKind::NeedsUpdate);
        assert_eq!(ErrorKind::from_code(100), ErrorKind::ServerError);
        assert_eq!(ErrorKind::from_code(99999), ErrorKind::ServerError);
    }

    #[test]
    fn raise_from_error_response_basic() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 900, "error_message": "bad token"},
            "request_params": {"api_token": "d***"},
            "request_api_method": "recognize"
        });
        let e = raise_from_error_response(&body, 200, None, false);
        assert!(e.is_authentication());
        assert_eq!(e.error_code(), Some(900));
        if let AudDError::Api {
            message,
            request_method,
            ..
        } = &e
        {
            assert_eq!(message, "bad token");
            assert_eq!(request_method.as_deref(), Some("recognize"));
        } else {
            panic!("not Api: {e:?}");
        }
    }

    #[test]
    fn raise_with_branded() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 19, "error_message": "blocked"},
            "result": {"artist": "ApiRequest failed", "title": "Sorry, your IP was banned"}
        });
        let e = raise_from_error_response(&body, 200, None, false);
        assert!(e.is_blocked());
        if let AudDError::Api {
            branded_message, ..
        } = &e
        {
            assert!(branded_message.is_some());
        }
    }

    #[test]
    fn custom_catalog_override() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 904, "error_message": "no access"}
        });
        let e = raise_from_error_response(&body, 200, None, true);
        assert!(e.is_custom_catalog_access());
        assert!(e.is_subscription());
        if let AudDError::Api { message, .. } = &e {
            assert!(message.contains("custom catalog"));
            assert!(message.contains("Server message: no access"));
        }
    }

    #[test]
    fn helpers() {
        let e = AudDError::Connection {
            message: "boom".into(),
            source: None,
        };
        assert!(!e.is_api());
        assert_eq!(e.error_code(), None);
    }

    #[test]
    fn coerce_string_code() {
        let body = json!({
            "status": "error",
            "error": {"error_code": "902", "error_message": "limit"},
        });
        let e = raise_from_error_response(&body, 200, None, false);
        assert_eq!(e.error_code(), Some(902));
        assert!(e.is_quota());
    }
}
