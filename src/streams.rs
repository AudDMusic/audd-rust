//! Streams namespace — set/get callback URL, addStream/listStreams/setStreamUrl/deleteStream,
//! longpoll with default-on preflight (and `skip_callback_check` opt-out),
//! `derive_longpoll_category`, `parse_callback`.

use std::pin::Pin;

use futures_core::Stream;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::{decode_or_raise, AudDInner};
use crate::errors::{AudDError, ErrorKind};
use crate::helpers::{add_return_to_url, derive_longpoll_category, parse_callback};
use crate::http::HttpClient;
use crate::models::{
    CallbackEvent, Stream as StreamRow, StreamCallbackMatch, StreamCallbackNotification,
};
use crate::retry::{retry_async, RetryPolicy};

/// Server returns error #19 from `getCallbackUrl` when no callback URL is
/// configured. We treat this specifically as the "no-callback-set" signal.
const NO_CALLBACK_ERROR_CODE: i32 = 19;

const HTTP_CLIENT_ERROR_FLOOR: u16 = 400;

const PREFLIGHT_NO_CALLBACK_HINT: &str =
    "Longpoll won't deliver events because no callback URL is configured for this account. \
Set one first via streams.set_callback_url(...) — `https://audd.tech/empty/` is fine if \
you only want longpolling and don't need a real receiver. \
To skip this check, pass skip_callback_check=true.";

/// Channel buffer for each of `matches` / `notifications` / `errors`. Small —
/// we want to apply backpressure to the poll loop when the consumer is slow.
const CHANNEL_BUFFER: usize = 16;

/// Boxed stream alias kept inline (avoid pulling `futures_util::stream::BoxStream`
/// just for the type name).
type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// Longpoll subscription configuration. Construct via [`Streams::longpoll`] and
/// chain builder-style overrides.
#[derive(Debug, Clone)]
pub struct LongpollOptions {
    since_time: Option<i64>,
    timeout: i64,
    skip_callback_check: bool,
}

impl Default for LongpollOptions {
    fn default() -> Self {
        Self {
            since_time: None,
            timeout: 50,
            skip_callback_check: false,
        }
    }
}

impl LongpollOptions {
    /// Set `since_time` (Unix-millis cursor returned by the server).
    #[must_use]
    pub fn since_time(mut self, t: i64) -> Self {
        self.since_time = Some(t);
        self
    }

    /// Set the per-request long-poll timeout in seconds (server-side cap; default 50).
    #[must_use]
    pub fn timeout(mut self, secs: i64) -> Self {
        self.timeout = secs;
        self
    }

    /// Skip the `getCallbackUrl` preflight check.
    #[must_use]
    pub fn skip_callback_check(mut self, skip: bool) -> Self {
        self.skip_callback_check = skip;
        self
    }
}

/// An active longpoll subscription. Three typed streams surface its output:
///
/// * [`Self::matches`] — recognition matches.
/// * [`Self::notifications`] — stream-lifecycle events.
/// * [`Self::errors`] — yields a single terminal error then closes; after an
///   error fires, `matches` and `notifications` close too.
///
/// Drop the [`LongpollPoll`] (or call [`Self::close`]) to tear down the
/// background poller.
pub struct LongpollPoll {
    /// Recognition matches.
    pub matches: BoxStream<StreamCallbackMatch>,
    /// Stream-lifecycle notifications (e.g. `stream stopped`, `can't connect`).
    pub notifications: BoxStream<StreamCallbackNotification>,
    /// Terminal-error stream — yields at most one error and closes.
    pub errors: BoxStream<AudDError>,

    shutdown: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LongpollPoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LongpollPoll").finish_non_exhaustive()
    }
}

impl LongpollPoll {
    /// Stop the background poll and wait for it to drain. Idempotent (only
    /// the first call performs the shutdown; subsequent calls are no-ops).
    pub async fn close(mut self) {
        self.close_internal().await;
    }

    async fn close_internal(&mut self) {
        // Drop the sender to signal shutdown.
        self.shutdown.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for LongpollPoll {
    fn drop(&mut self) {
        // On drop, take the sender so the background task exits its loop.
        // We don't await the join handle — best-effort cleanup. Callers who
        // want deterministic shutdown should call `close().await` first.
        self.shutdown.take();
        if let Some(handle) = self.join.take() {
            handle.abort();
        }
    }
}

/// Streams namespace. Reach via [`crate::AudD::streams`].
pub struct Streams<'a> {
    inner: &'a AudDInner,
}

impl<'a> Streams<'a> {
    pub(crate) fn new(inner: &'a AudDInner) -> Self {
        Self { inner }
    }

    /// Set the callback URL on the caller's account. If `return_metadata` is
    /// provided, it's appended as a `?return=...` query parameter to the URL.
    /// Refuses to silently overwrite an existing `return=` parameter.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport, server, or input-conflict failures.
    pub async fn set_callback_url(
        &self,
        url: &str,
        return_metadata: Option<&[String]>,
    ) -> Result<(), AudDError> {
        let url = add_return_to_url(url, return_metadata)?;
        post_form(
            &self.inner.http,
            &format!("{}/setCallbackUrl/", self.inner.api_base),
            &[("url", url)],
            self.inner.mutating_policy(),
        )
        .await
        .map(drop)
    }

    /// Read the currently-configured callback URL.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Api`] with code 19 if no callback URL is configured.
    pub async fn get_callback_url(&self) -> Result<String, AudDError> {
        let result = post_form(
            &self.inner.http,
            &format!("{}/getCallbackUrl/", self.inner.api_base),
            &[],
            self.inner.read_policy(),
        )
        .await?;
        Ok(result
            .as_str()
            .map_or_else(|| result.to_string(), str::to_string))
    }

    /// Add a stream subscription.
    ///
    /// `url` accepts direct stream URLs (DASH, Icecast, HLS, m3u/m3u8) and
    /// shortcuts like `twitch:<channel>`, `youtube:<video_id>`,
    /// `youtube-ch:<channel_id>`. Pass `callbacks=Some("before")` to deliver
    /// callbacks at song start instead of song end.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server failures.
    pub async fn add(
        &self,
        url: &str,
        radio_id: i64,
        callbacks: Option<&str>,
    ) -> Result<(), AudDError> {
        let mut fields: Vec<(&str, String)> =
            vec![("url", url.to_string()), ("radio_id", radio_id.to_string())];
        if let Some(cb) = callbacks {
            fields.push(("callbacks", cb.to_string()));
        }
        post_form(
            &self.inner.http,
            &format!("{}/addStream/", self.inner.api_base),
            &fields,
            self.inner.mutating_policy(),
        )
        .await
        .map(drop)
    }

    /// Update the URL of an existing stream subscription.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server failures.
    pub async fn set_url(&self, radio_id: i64, url: &str) -> Result<(), AudDError> {
        post_form(
            &self.inner.http,
            &format!("{}/setStreamUrl/", self.inner.api_base),
            &[("radio_id", radio_id.to_string()), ("url", url.to_string())],
            self.inner.mutating_policy(),
        )
        .await
        .map(drop)
    }

    /// Delete a stream subscription.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server failures.
    pub async fn delete(&self, radio_id: i64) -> Result<(), AudDError> {
        post_form(
            &self.inner.http,
            &format!("{}/deleteStream/", self.inner.api_base),
            &[("radio_id", radio_id.to_string())],
            self.inner.mutating_policy(),
        )
        .await
        .map(drop)
    }

    /// List all stream subscriptions on the caller's account.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server/parse failures.
    pub async fn list(&self) -> Result<Vec<StreamRow>, AudDError> {
        let result = post_form(
            &self.inner.http,
            &format!("{}/getStreams/", self.inner.api_base),
            &[],
            self.inner.read_policy(),
        )
        .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        let v: Vec<StreamRow> =
            serde_json::from_value(result.clone()).map_err(|e| AudDError::Serialization {
                message: format!("could not parse getStreams result: {e}"),
                raw_text: result.to_string(),
            })?;
        Ok(v)
    }

    /// Compute the 9-char longpoll category locally from `(api_token, radio_id)`.
    /// Pure function — no network call. Snapshots the live api_token, so it
    /// reflects any prior `AudD::set_api_token` rotation.
    #[must_use]
    pub fn derive_longpoll_category(&self, radio_id: i64) -> String {
        derive_longpoll_category(&self.inner.api_token(), radio_id)
    }

    /// Parse an already-deserialized callback POST body into a typed
    /// [`CallbackEvent`].
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Serialization`] if the body doesn't deserialize.
    pub fn parse_callback(&self, body: Value) -> Result<CallbackEvent, AudDError> {
        parse_callback(body)
    }

    /// Long-poll the AudD streams endpoint and return a [`LongpollPoll`]
    /// handle whose typed streams (matches / notifications / errors) are
    /// fed by a background tokio task.
    ///
    /// Server keepalive ticks (`{"timeout": "no events before timeout"}`) are
    /// silently absorbed — they advance the internal cursor and never reach
    /// the consumer.
    ///
    /// On entry, performs a one-time `getCallbackUrl` preflight unless
    /// `opts.skip_callback_check == true`. If the server returns error #19
    /// (no callback URL configured), [`AudDError::Api`] is returned with kind
    /// [`ErrorKind::InvalidRequest`] explaining how to fix it.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] from the preflight only. Fatal errors during
    /// polling surface on [`LongpollPoll::errors`].
    pub async fn longpoll(
        &self,
        category: &str,
        opts: LongpollOptions,
    ) -> Result<LongpollPoll, AudDError> {
        if !opts.skip_callback_check {
            self.preflight_callback().await?;
        }
        Ok(spawn_longpoll(LongpollDriver::Authenticated {
            http: self.inner.http.clone(),
            url: format!("{}/longpoll/", self.inner.api_base),
            policy: self.inner.read_policy(),
            category: category.to_string(),
            opts,
        }))
    }

    async fn preflight_callback(&self) -> Result<(), AudDError> {
        match self.get_callback_url().await {
            Ok(_) => Ok(()),
            Err(e) if e.error_code() == Some(NO_CALLBACK_ERROR_CODE) => {
                let (http_status, request_id) = match &e {
                    AudDError::Api {
                        http_status,
                        request_id,
                        ..
                    } => (*http_status, request_id.clone()),
                    _ => (0, None),
                };
                Err(AudDError::Api {
                    code: 0,
                    message: PREFLIGHT_NO_CALLBACK_HINT.to_string(),
                    kind: ErrorKind::InvalidRequest,
                    http_status,
                    request_id,
                    requested_params: std::collections::HashMap::new(),
                    request_method: None,
                    branded_message: None,
                    raw_response: Value::Null,
                })
            }
            Err(other) => Err(other),
        }
    }
}

/// Internal — describes a single longpoll fetch source so authenticated and
/// tokenless consumers share the same dispatch loop.
pub(crate) enum LongpollDriver {
    Authenticated {
        http: HttpClient,
        url: String,
        policy: RetryPolicy,
        category: String,
        opts: LongpollOptions,
    },
    Tokenless {
        http: crate::http::BareHttpClient,
        url: String,
        policy: RetryPolicy,
        category: String,
        since_time: Option<i64>,
        timeout: i64,
    },
}

impl LongpollDriver {
    fn category(&self) -> &str {
        match self {
            Self::Authenticated { category, .. } | Self::Tokenless { category, .. } => category,
        }
    }

    fn timeout(&self) -> i64 {
        match self {
            Self::Authenticated { opts, .. } => opts.timeout,
            Self::Tokenless { timeout, .. } => *timeout,
        }
    }

    fn since_time(&self) -> Option<i64> {
        match self {
            Self::Authenticated { opts, .. } => opts.since_time,
            Self::Tokenless { since_time, .. } => *since_time,
        }
    }

    async fn fetch(
        &self,
        params: &[(&str, String)],
    ) -> Result<crate::http::HttpResponse, AudDError> {
        match self {
            Self::Authenticated {
                http, url, policy, ..
            } => {
                let url = url.clone();
                let policy = *policy;
                let http = http.clone();
                let params: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.clone())).collect();
                retry_async(
                    || {
                        let http = http.clone();
                        let url = url.clone();
                        let params = params.clone();
                        async move { http.get(&url, &params, None).await }
                    },
                    policy,
                )
                .await
            }
            Self::Tokenless {
                http, url, policy, ..
            } => {
                let url = url.clone();
                let policy = *policy;
                let http = http.clone();
                let params: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.clone())).collect();
                retry_async(
                    || {
                        let http = http.clone();
                        let url = url.clone();
                        let params = params.clone();
                        async move { http.get(&url, &params).await }
                    },
                    policy,
                )
                .await
            }
        }
    }
}

/// Spawn the background poll task and wire up the three streams.
pub(crate) fn spawn_longpoll(driver: LongpollDriver) -> LongpollPoll {
    let (match_tx, match_rx) = mpsc::channel::<StreamCallbackMatch>(CHANNEL_BUFFER);
    let (notif_tx, notif_rx) = mpsc::channel::<StreamCallbackNotification>(CHANNEL_BUFFER);
    let (err_tx, err_rx) = mpsc::channel::<AudDError>(1);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

    let join = tokio::spawn(run_longpoll(
        driver,
        match_tx,
        notif_tx,
        err_tx,
        shutdown_rx,
    ));

    LongpollPoll {
        matches: Box::pin(channel_stream(match_rx)),
        notifications: Box::pin(channel_stream(notif_rx)),
        errors: Box::pin(channel_stream(err_rx)),
        shutdown: Some(shutdown_tx),
        join: Some(join),
    }
}

fn channel_stream<T: Send + 'static>(mut rx: mpsc::Receiver<T>) -> impl Stream<Item = T> + Send {
    async_stream::stream! {
        while let Some(item) = rx.recv().await {
            yield item;
        }
    }
}

/// Drive a single longpoll subscription: read responses, parse them, and
/// dispatch to the typed channels. Exits when the shutdown signal is dropped,
/// a fatal error fires, or all channels are closed.
async fn run_longpoll(
    driver: LongpollDriver,
    match_tx: mpsc::Sender<StreamCallbackMatch>,
    notif_tx: mpsc::Sender<StreamCallbackNotification>,
    err_tx: mpsc::Sender<AudDError>,
    mut shutdown_rx: mpsc::Receiver<()>,
) {
    let mut cur_since = driver.since_time();
    let timeout_secs = driver.timeout().to_string();
    let category = driver.category().to_string();

    loop {
        // Build params each iteration so since_time updates flow through.
        let mut params: Vec<(&str, String)> = vec![
            ("category", category.clone()),
            ("timeout", timeout_secs.clone()),
        ];
        if let Some(t) = cur_since {
            params.push(("since_time", t.to_string()));
        }

        // Race the fetch against shutdown so a quiescent caller can hang up
        // even mid-poll.
        let resp = tokio::select! {
            biased;
            _ = shutdown_rx.recv() => return,
            r = driver.fetch(&params) => r,
        };

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                let _ = err_tx.send(e).await;
                return;
            }
        };

        // Surface non-2xx as a Server error.
        if resp.http_status >= HTTP_CLIENT_ERROR_FLOOR {
            let _ = err_tx
                .send(AudDError::Server {
                    http_status: resp.http_status,
                    message: format!("Longpoll endpoint returned HTTP {}", resp.http_status),
                    request_id: resp.request_id,
                    raw_response: resp.raw_text,
                })
                .await;
            return;
        }

        let Some(body) = resp.json_body else {
            let _ = err_tx
                .send(AudDError::Serialization {
                    message: "Longpoll response was not a JSON object".into(),
                    raw_text: resp.raw_text,
                })
                .await;
            return;
        };

        // Silently absorb keepalive ticks.
        if is_longpoll_keepalive(&body) {
            if let Some(ts) = body.get("timestamp").and_then(Value::as_i64) {
                cur_since = Some(ts);
            }
            continue;
        }

        // Advance the cursor before parsing — even if parsing fails, we don't
        // want to re-poll the same window.
        if let Some(ts) = body.get("timestamp").and_then(Value::as_i64) {
            cur_since = Some(ts);
        }

        match parse_callback(body) {
            Ok(CallbackEvent::Match(m)) => {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => return,
                    res = match_tx.send(m) => {
                        if res.is_err() { return; }
                    }
                }
            }
            Ok(CallbackEvent::Notification(n)) => {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.recv() => return,
                    res = notif_tx.send(n) => {
                        if res.is_err() { return; }
                    }
                }
            }
            Err(e) => {
                let _ = err_tx.send(e).await;
                return;
            }
        }
    }
}

/// Reports whether `body` is a `{"timeout": "no events before timeout"}`
/// keepalive tick — the server emits one of these every `<timeout>` seconds
/// when no recognition or notification is queued. Mirrors audd-go's
/// `isLongpollKeepalive` helper.
pub(crate) fn is_longpoll_keepalive(body: &Value) -> bool {
    let Some(obj) = body.as_object() else {
        return false;
    };
    if obj.contains_key("result") || obj.contains_key("notification") {
        return false;
    }
    obj.contains_key("timeout")
}

/// Internal — POST a form body to a streams-namespace endpoint and return the
/// `result` field on success.
async fn post_form(
    http: &HttpClient,
    url: &str,
    fields: &[(&str, String)],
    policy: RetryPolicy,
) -> Result<Value, AudDError> {
    let url = url.to_string();
    let fields: Vec<(&str, String)> = fields.iter().map(|(k, v)| (*k, v.clone())).collect();
    let resp = retry_async(
        || {
            let http = http.clone();
            let url = url.clone();
            let fields = fields.clone();
            async move { http.post_form(&url, &fields, None, None).await }
        },
        policy,
    )
    .await?;
    let body = decode_or_raise(resp, false)?;
    Ok(body.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longpoll_options_default() {
        let o = LongpollOptions::default();
        assert_eq!(o.timeout, 50);
        assert!(!o.skip_callback_check);
    }

    #[test]
    fn longpoll_options_chain() {
        let o = LongpollOptions::default()
            .timeout(30)
            .since_time(123)
            .skip_callback_check(true);
        assert_eq!(o.timeout, 30);
        assert_eq!(o.since_time, Some(123));
        assert!(o.skip_callback_check);
    }

    #[test]
    fn keepalive_detection() {
        let kp = serde_json::json!({"timeout": "no events before timeout", "timestamp": 1});
        assert!(is_longpoll_keepalive(&kp));

        let with_result = serde_json::json!({
            "result": {"radio_id": 1, "results": []},
            "timeout": "no events"
        });
        assert!(!is_longpoll_keepalive(&with_result));

        let with_notif = serde_json::json!({
            "notification": {"radio_id": 1},
            "timeout": "x"
        });
        assert!(!is_longpoll_keepalive(&with_notif));

        let no_timeout = serde_json::json!({"timestamp": 1});
        assert!(!is_longpoll_keepalive(&no_timeout));

        let not_object = serde_json::json!([1, 2, 3]);
        assert!(!is_longpoll_keepalive(&not_object));
    }
}
