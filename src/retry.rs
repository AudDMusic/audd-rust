//! Cost-aware retry policy.

use std::future::Future;
use std::time::Duration;

use crate::errors::AudDError;
use crate::http::HttpResponse;

/// Determines which conditions are retryable for a given endpoint.
///
/// * `Read` — idempotent reads (`streams.list`, `streams.get_callback_url`):
///   retry on 408/429/5xx + any connection error.
/// * `Recognition` — `recognize`, `recognize_enterprise`, `advanced.find_lyrics`,
///   `advanced.raw_request`: retry on pre-upload connection failures + 5xx.
///   Do NOT retry on read-timeout-after-upload (cost protection).
/// * `Mutating` — `streams.set_callback_url`, `streams.add`, `streams.delete`,
///   etc., `custom_catalog.add`: retry only on pre-upload connection failures.
///   Do NOT retry 5xx (the side effect may have happened).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// Idempotent reads.
    Read,
    /// Recognition / metered endpoints.
    Recognition,
    /// State-mutating endpoints.
    Mutating,
}

/// Retry policy configuration.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Retry behavior class.
    pub retry_class: RetryClass,
    /// Maximum total attempts (including the first).
    pub max_attempts: u32,
    /// Base backoff multiplier in seconds.
    pub backoff_factor: f64,
    /// Cap on individual backoff delays.
    pub backoff_max: f64,
}

impl RetryPolicy {
    /// Build a policy for the given class with the standard defaults (3 attempts,
    /// 0.5s backoff factor).
    #[must_use]
    pub fn new(retry_class: RetryClass) -> Self {
        Self {
            retry_class,
            max_attempts: 3,
            backoff_factor: 0.5,
            backoff_max: 30.0,
        }
    }

    /// Builder-style override for `max_attempts`.
    #[must_use]
    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    /// Builder-style override for `backoff_factor`.
    #[must_use]
    pub fn with_backoff_factor(mut self, f: f64) -> Self {
        self.backoff_factor = f;
        self
    }
}

const HTTP_REQUEST_TIMEOUT: u16 = 408;
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
const HTTP_SERVER_ERROR_FLOOR: u16 = 500;

/// Decide whether to retry given a response status code.
fn should_retry_response(status: u16, class: RetryClass) -> bool {
    match class {
        RetryClass::Read => {
            status == HTTP_REQUEST_TIMEOUT
                || status == HTTP_TOO_MANY_REQUESTS
                || status >= HTTP_SERVER_ERROR_FLOOR
        }
        RetryClass::Recognition => status >= HTTP_SERVER_ERROR_FLOOR,
        RetryClass::Mutating => false,
    }
}

/// Decide whether to retry given an error from the HTTP layer.
fn should_retry_error(err: &AudDError, class: RetryClass) -> bool {
    let AudDError::Connection { source, .. } = err else {
        return false;
    };
    let Some(src) = source else {
        // Connection error without an underlying source — treat as transient
        // for READ but conservative otherwise.
        return matches!(class, RetryClass::Read);
    };
    let Some(rerr) = src.downcast_ref::<reqwest::Error>() else {
        return matches!(class, RetryClass::Read);
    };
    match class {
        RetryClass::Read => true, // any reqwest error during a read is retryable
        RetryClass::Recognition | RetryClass::Mutating => is_pre_upload_connection_error(rerr),
    }
}

/// Heuristic for "the request body never made it to the server, so retrying is safe."
fn is_pre_upload_connection_error(err: &reqwest::Error) -> bool {
    // reqwest doesn't expose precise lifecycle telemetry, so use the public
    // boolean predicates plus a check for `is_request` (DNS / TCP / TLS errors
    // surface here in 0.12). is_timeout=true with is_connect=false signals a
    // post-upload read timeout — explicitly NOT safe for recognition retries.
    if err.is_connect() {
        return true;
    }
    if err.is_request() && !err.is_timeout() {
        return true;
    }
    false
}

fn backoff_delay(attempt: u32, policy: &RetryPolicy) -> Duration {
    // Deterministic-jitter approximation: use a small splitmix-style transform of
    // the attempt index to avoid pulling `rand`. Stays in the half-open
    // `[0.5, 1.5)` range like Python's `0.5 + random.random()` behavior.
    let mut x = u64::from(attempt)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x_BF58_476D_1CE4_E5B9);
    x ^= x >> 30;
    x = x.wrapping_mul(0x_BF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    // Truncate to f64-representable range; we only need a fractional in [0,1).
    #[allow(clippy::cast_precision_loss)]
    let frac = ((x >> 11) as f64) / ((1u64 << 53) as f64);
    let attempt_i = i32::try_from(attempt).unwrap_or(i32::MAX);
    let base = (policy.backoff_factor * 2f64.powi(attempt_i)).min(policy.backoff_max);
    let secs = base * (0.5 + frac.clamp(0.0, 1.0));
    Duration::from_secs_f64(secs.max(0.0))
}

/// Run an async HTTP closure under the given retry policy.
///
/// `fut_factory` should produce a fresh future on each attempt — the caller
/// composes it with the source re-opener.
pub(crate) async fn retry_async<F, Fut>(
    mut fut_factory: F,
    policy: RetryPolicy,
) -> Result<HttpResponse, AudDError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<HttpResponse, AudDError>> + Send,
{
    let mut last_err: Option<AudDError> = None;
    let mut last_resp: Option<HttpResponse> = None;
    for attempt in 0..policy.max_attempts {
        match fut_factory().await {
            Ok(resp) => {
                if !should_retry_response(resp.http_status, policy.retry_class) {
                    return Ok(resp);
                }
                let last_attempt = attempt + 1 >= policy.max_attempts;
                last_resp = Some(resp);
                last_err = None;
                if last_attempt {
                    return Ok(last_resp.expect("just set"));
                }
            }
            Err(e) => {
                if !should_retry_error(&e, policy.retry_class) {
                    return Err(e);
                }
                let last_attempt = attempt + 1 >= policy.max_attempts;
                last_err = Some(e);
                last_resp = None;
                if last_attempt {
                    return Err(last_err.expect("just set"));
                }
            }
        }
        tokio::time::sleep(backoff_delay(attempt, &policy)).await;
    }
    if let Some(r) = last_resp {
        return Ok(r);
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    // Unreachable because max_attempts ≥ 1 ⇒ we exited via one of the branches above.
    Err(AudDError::Connection {
        message: "retry loop exited without result (max_attempts=0?)".into(),
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_retries_on_5xx() {
        assert!(should_retry_response(503, RetryClass::Read));
        assert!(should_retry_response(429, RetryClass::Read));
        assert!(should_retry_response(408, RetryClass::Read));
        assert!(!should_retry_response(404, RetryClass::Read));
    }

    #[test]
    fn recognition_skips_429() {
        // 429 is NOT retried for recognition (cost concern; only 5xx).
        assert!(should_retry_response(503, RetryClass::Recognition));
        assert!(!should_retry_response(429, RetryClass::Recognition));
    }

    #[test]
    fn mutating_no_retry_on_response() {
        assert!(!should_retry_response(503, RetryClass::Mutating));
        assert!(!should_retry_response(429, RetryClass::Mutating));
    }

    #[test]
    fn backoff_grows() {
        let p = RetryPolicy::new(RetryClass::Read);
        let d0 = backoff_delay(0, &p);
        let d3 = backoff_delay(3, &p);
        assert!(d3 >= d0);
    }

    #[tokio::test]
    async fn retry_returns_first_success() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_c = attempts.clone();
        let mut policy = RetryPolicy::new(RetryClass::Read);
        policy.backoff_factor = 0.0;
        policy.backoff_max = 0.0;
        let resp = retry_async(
            move || {
                let n = attempts_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Ok(HttpResponse {
                            http_status: 503,
                            json_body: None,
                            request_id: None,
                            raw_text: String::new(),
                        })
                    } else {
                        Ok(HttpResponse {
                            http_status: 200,
                            json_body: Some(serde_json::json!({"status": "success"})),
                            request_id: None,
                            raw_text: String::new(),
                        })
                    }
                }
            },
            policy,
        )
        .await
        .unwrap();
        assert_eq!(resp.http_status, 200);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_respects_max_attempts() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_c = attempts.clone();
        let mut policy = RetryPolicy::new(RetryClass::Read);
        policy.max_attempts = 2;
        policy.backoff_factor = 0.0;
        policy.backoff_max = 0.0;
        let resp = retry_async(
            move || {
                attempts_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    Ok(HttpResponse {
                        http_status: 503,
                        json_body: None,
                        request_id: None,
                        raw_text: String::new(),
                    })
                }
            },
            policy,
        )
        .await
        .unwrap();
        assert_eq!(resp.http_status, 503);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
