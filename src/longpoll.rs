//! Tokenless longpoll consumer for browser/widget/extension use cases.
//!
//! Carries no api_token. The category alone authorizes the subscription. The
//! user/server who derived the category is responsible for ensuring a callback
//! URL is set on their account (we can't preflight that without a token).
//!
//! Hardening:
//! * HTTP non-2xx → [`crate::AudDError::Server`] (not silent loop forever)
//! * JSON decode failure on 2xx → [`crate::AudDError::Serialization`]
//! * Retries (READ class) on 5xx + connection errors
//! * Configurable `max_attempts` / `backoff_factor` via the builder.

use std::pin::Pin;

use futures_core::Stream;
use serde_json::Value;

use crate::errors::AudDError;
use crate::http::{BareHttpClient, HttpResponse};
use crate::retry::{retry_async, RetryClass, RetryPolicy};

const LONGPOLL_URL: &str = "https://api.audd.io/longpoll/";
const HTTP_CLIENT_ERROR_FLOOR: u16 = 400;

/// Builder for [`LongpollConsumer`].
#[derive(Debug, Clone)]
pub struct LongpollConsumerBuilder {
    category: String,
    max_attempts: u32,
    backoff_factor: f64,
    reqwest_client: Option<reqwest::Client>,
    base_url: String,
}

impl LongpollConsumerBuilder {
    fn new(category: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            max_attempts: 3,
            backoff_factor: 0.5,
            reqwest_client: None,
            base_url: LONGPOLL_URL.to_string(),
        }
    }

    /// Maximum total attempts per long-poll iteration (default `3`).
    #[must_use]
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    /// Base backoff factor in seconds (default `0.5`).
    #[must_use]
    pub fn backoff_factor(mut self, f: f64) -> Self {
        self.backoff_factor = f;
        self
    }

    /// Inject a configured [`reqwest::Client`] — useful for proxies, custom
    /// CA bundles, etc.
    #[must_use]
    pub fn reqwest_client(mut self, client: reqwest::Client) -> Self {
        self.reqwest_client = Some(client);
        self
    }

    /// Override the longpoll endpoint URL — used by the test harness.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Build the consumer.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Connection`] if `reqwest::Client::build()` fails.
    pub fn build(self) -> Result<LongpollConsumer, AudDError> {
        let http = if let Some(c) = self.reqwest_client {
            BareHttpClient::from_client(c)
        } else {
            BareHttpClient::new()?
        };
        Ok(LongpollConsumer {
            category: self.category,
            http,
            policy: RetryPolicy::new(RetryClass::Read)
                .with_max_attempts(self.max_attempts)
                .with_backoff_factor(self.backoff_factor),
            url: self.base_url,
        })
    }
}

/// Tokenless long-poll consumer. Construct via [`LongpollConsumer::new`] or
/// [`LongpollConsumer::builder`].
#[derive(Debug, Clone)]
pub struct LongpollConsumer {
    category: String,
    http: BareHttpClient,
    policy: RetryPolicy,
    url: String,
}

/// Per-iteration knobs passed to [`LongpollConsumer::iterate`].
#[derive(Debug, Clone)]
pub struct LongpollIterateOptions {
    /// Cursor returned by the server on the previous tick.
    pub since_time: Option<i64>,
    /// Server-side long-poll wait in seconds (default 50, capped server-side).
    pub timeout: i64,
}

impl Default for LongpollIterateOptions {
    fn default() -> Self {
        Self {
            since_time: None,
            timeout: 50,
        }
    }
}

impl LongpollConsumer {
    /// Build a consumer with the standard defaults.
    ///
    /// # Panics
    ///
    /// Panics only if `reqwest::Client::build()` fails — exceedingly rare.
    #[must_use]
    pub fn new(category: impl Into<String>) -> Self {
        Self::builder(category)
            .build()
            .expect("default reqwest::Client should build")
    }

    /// Begin building a [`LongpollConsumer`].
    pub fn builder(category: impl Into<String>) -> LongpollConsumerBuilder {
        LongpollConsumerBuilder::new(category)
    }

    /// Yield successive longpoll responses as a [`Stream`]. The stream runs
    /// indefinitely until the caller drops it; cancel by dropping the future.
    ///
    /// # Errors
    ///
    /// The stream yields [`AudDError`] on transport, server, or parse failures.
    /// Specifically: HTTP non-2xx → [`AudDError::Server`]; 2xx with garbage
    /// JSON → [`AudDError::Serialization`].
    pub fn iterate(
        &self,
        opts: LongpollIterateOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<Value, AudDError>> + Send>> {
        let http = self.http.clone();
        let category = self.category.clone();
        let url = self.url.clone();
        let policy = self.policy;
        let mut cur_since = opts.since_time;
        let timeout = opts.timeout;
        let stream = async_stream::try_stream! {
            loop {
                let mut params: Vec<(&str, String)> = vec![
                    ("category", category.clone()),
                    ("timeout", timeout.to_string()),
                ];
                if let Some(t) = cur_since {
                    params.push(("since_time", t.to_string()));
                }
                let resp = retry_async(
                    || {
                        let http = http.clone();
                        let url = url.clone();
                        let params = params.clone();
                        async move {
                            http.get(&url, &params).await
                        }
                    },
                    policy,
                )
                .await?;
                let body = decode(resp)?;
                if let Some(ts) = body.get("timestamp").and_then(Value::as_i64) {
                    cur_since = Some(ts);
                }
                yield body;
            }
        };
        Box::pin(stream)
    }

    /// Drop the underlying HTTP transport explicitly. Equivalent to dropping
    /// the [`LongpollConsumer`].
    pub fn close(self) {
        // Bare client owns its reqwest::Client when `owned`; let it drop.
        drop(self);
    }
}

fn decode(resp: HttpResponse) -> Result<Value, AudDError> {
    let HttpResponse {
        json_body,
        http_status,
        request_id,
        raw_text,
    } = resp;
    if http_status >= HTTP_CLIENT_ERROR_FLOOR {
        return Err(AudDError::Server {
            http_status,
            message: format!("Longpoll endpoint returned HTTP {http_status}"),
            request_id,
            raw_response: raw_text,
        });
    }
    let body = json_body.ok_or_else(|| AudDError::Serialization {
        message: "Longpoll response was not a JSON object".into(),
        raw_text: raw_text.clone(),
    })?;
    if !body.is_object() {
        return Err(AudDError::Serialization {
            message: "Longpoll response was not a JSON object".into(),
            raw_text,
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_2xx_garbage_is_serialization() {
        let r = HttpResponse {
            json_body: None,
            http_status: 200,
            request_id: None,
            raw_text: "boom".into(),
        };
        let e = decode(r).unwrap_err();
        assert!(matches!(e, AudDError::Serialization { .. }));
    }

    #[test]
    fn decode_non_2xx_is_server() {
        let r = HttpResponse {
            json_body: None,
            http_status: 500,
            request_id: None,
            raw_text: "<html>".into(),
        };
        let e = decode(r).unwrap_err();
        match e {
            AudDError::Server { http_status, .. } => assert_eq!(http_status, 500),
            other => panic!("not Server: {other:?}"),
        }
    }
}
