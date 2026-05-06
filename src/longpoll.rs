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
//! * Keepalive ticks (`{"timeout": "no events before timeout"}`) are silently
//!   absorbed — the cursor advances and the consumer never sees them.

use crate::errors::AudDError;
use crate::http::BareHttpClient;
use crate::retry::{RetryClass, RetryPolicy};
use crate::streams::{spawn_longpoll, LongpollDriver, LongpollPoll};

const LONGPOLL_URL: &str = "https://api.audd.io/longpoll/";

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

    /// Begin a longpoll subscription. Returns a [`LongpollPoll`] handle whose
    /// typed streams (matches / notifications / errors) are fed by a
    /// background tokio task. Drop the handle (or call `close().await`) to
    /// shut it down.
    ///
    /// Server keepalive ticks (`{"timeout": "no events before timeout"}`) are
    /// silently absorbed.
    #[must_use]
    pub fn iterate(&self, opts: LongpollIterateOptions) -> LongpollPoll {
        spawn_longpoll(LongpollDriver::Tokenless {
            http: self.http.clone(),
            url: self.url.clone(),
            policy: self.policy,
            category: self.category.clone(),
            since_time: opts.since_time,
            timeout: opts.timeout,
        })
    }

    /// Drop the underlying HTTP transport explicitly. Equivalent to dropping
    /// the [`LongpollConsumer`].
    pub fn close(self) {
        // Bare client owns its reqwest::Client when `owned`; let it drop.
        drop(self);
    }
}
