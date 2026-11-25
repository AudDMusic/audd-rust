//! Streams namespace — set/get callback URL, addStream/listStreams/setStreamUrl/deleteStream,
//! longpoll with default-on preflight (and `skip_callback_check` opt-out),
//! `derive_longpoll_category`, `parse_callback`.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::{decode_or_raise, AudDInner};
use crate::errors::{raise_from_error_response, AudDError, ErrorKind};
use crate::helpers::{add_return_to_url, derive_longpoll_category, parse_callback};
use crate::http::{HttpClient, HttpResponse};
use crate::models::{
    lenient_from_value, lenient_list, CallbackEvent, Stream as StreamRow, StreamCallbackMatch,
    StreamCallbackNotification, StreamCallbackSong,
};
use crate::retry::{retry_async, RetryPolicy};

/// Server returns error #19 with an "Internal error" message from
/// `getCallbackUrl` when no callback URL is configured. Code 19 also covers
/// real conditions (maintenance, blocked requests), so the preflight only
/// treats it as the no-callback signal when the message indicates it — see
/// [`indicates_no_callback_url`].
const NO_CALLBACK_ERROR_CODE: i32 = 19;

const HTTP_CLIENT_ERROR_FLOOR: u16 = 400;

/// Margin added on top of the poll timeout when sizing the per-request HTTP
/// deadline for longpoll GETs.
const LONGPOLL_TIMEOUT_MARGIN_SECS: u64 = 10;

/// Size the HTTP deadline for one longpoll request: the server-side poll
/// timeout plus a network margin, so poll timeouts above the standard 60s
/// aren't cut short by the transport.
pub(crate) fn longpoll_request_timeout(poll_timeout_secs: i64) -> std::time::Duration {
    let secs = u64::try_from(poll_timeout_secs).unwrap_or(0);
    std::time::Duration::from_secs(secs + LONGPOLL_TIMEOUT_MARGIN_SECS)
}

/// Reports whether a code-19 error message from `getCallbackUrl` is the
/// no-callback-URL signal ("Internal error") rather than a real server
/// condition (maintenance, blocked request, abuse, ...).
fn indicates_no_callback_url(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("internal") || lower.contains("callback")
}

const PREFLIGHT_NO_CALLBACK_HINT: &str =
    "Longpoll won't deliver events because no callback URL is configured for this account. \
Set one first via streams.set_callback_url(...) — `https://audd.tech/empty/` is fine if \
you only want longpolling and don't need a real receiver. \
To skip this check, pass skip_callback_check=true.";

/// Buffer for `matches`. Small: a slow consumer applies backpressure to the
/// poll loop, and undelivered events wait on the server.
const MATCH_BUFFER: usize = 16;

/// Buffer for `notifications`. When it is full (nobody is reading
/// notifications), further notifications are dropped so that matches keep
/// flowing.
const NOTIFICATION_BUFFER: usize = 64;

/// Pause after a longpoll response that moved the `since_time` cursor neither
/// way (an unrecognized body, an empty event list). Doubles on each further
/// such response, up to [`LONGPOLL_IDLE_WAIT_MAX`].
const LONGPOLL_IDLE_WAIT_START: Duration = Duration::from_secs(1);
const LONGPOLL_IDLE_WAIT_MAX: Duration = Duration::from_secs(30);

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
    /// Ask only for events newer than this server timestamp (Unix
    /// milliseconds), such as the `timestamp` of the last event a previous
    /// subscription handled.
    #[must_use]
    pub fn since_time(mut self, t: i64) -> Self {
        self.since_time = Some(t);
        self
    }

    /// Seconds the server holds each request open while waiting for events:
    /// 1 to 120, default 50. The server rejects other values, and the
    /// rejection arrives on [`LongpollPoll::errors`].
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
/// Read `errors` alongside `matches` (a `tokio::select!` loop does both): a
/// loop over `matches` alone ends without saying why.
///
/// A background task sends `GET /longpoll/` requests one after another. Each
/// response is one of:
///
/// * **Events.** Each event's `data` is the body a callback URL would
///   receive. It is parsed like [`crate::parse_callback`] and delivered in
///   order on `matches` or `notifications`. An event whose `data` doesn't
///   parse, and a recognition with an empty `results` list, is skipped. The
///   next request asks for events newer than the largest event `timestamp`.
/// * **Timeout** (`{"timeout": "no events before timeout", "timestamp": ...}`).
///   Nothing is delivered; the next request asks for events newer than its
///   `timestamp`.
/// * **Error** (`{"error": "..."}`, sent for a `timeout` outside 1 to 120 or
///   a bad `since_time`). It arrives once on `errors` as an
///   [`AudDError::Api`] of kind [`ErrorKind::InvalidRequest`] carrying the
///   server's text, and polling stops: the same request would get the same
///   answer.
///
/// A response that yields no new `timestamp` (an empty event list, a body in
/// an unrecognized shape, a non-JSON body) is followed by a pause before the
/// next request: 1 second, doubling up to 30 seconds while it repeats.
/// Connection failures and HTTP 408, 429 and 5xx are retried with backoff
/// within the client's `max_attempts` budget; a failure that outlasts it, or
/// any other HTTP error status, arrives on `errors` and ends the
/// subscription.
///
/// The requests carry no api_token: the category authorizes them.
///
/// Drop the [`LongpollPoll`] (or call [`Self::close`]) to tear down the
/// background poller; either one also abandons a request in flight.
pub struct LongpollPoll {
    /// Recognition matches.
    pub matches: BoxStream<StreamCallbackMatch>,
    /// Stream-lifecycle notifications (e.g. `stream stopped`, `can't connect`).
    /// Up to 64 unread notifications are buffered; beyond that, new ones are
    /// dropped so that `matches` keeps flowing for a consumer that doesn't
    /// read notifications.
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
        extra_parameters: Option<&HashMap<String, String>>,
    ) -> Result<(), AudDError> {
        let url = add_return_to_url(url, return_metadata)?;
        // extra_parameters first; typed fields (`url`) win on collision.
        let mut fields: Vec<(&str, String)> = Vec::new();
        if let Some(extras) = extra_parameters {
            for (k, v) in extras {
                fields.push((k.as_str(), v.clone()));
            }
        }
        fields.push(("url", url));
        post_form(
            self.inner,
            "setCallbackUrl",
            &format!("{}/setCallbackUrl/", self.inner.api_base),
            &fields,
            self.inner.mutating_policy(),
        )
        .await
        .map(drop)
    }

    /// Read the currently-configured callback URL.
    ///
    /// Returns `Ok(None)` when the server reports success with a `null`
    /// result (no URL value to return).
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Api`] with code 19 if no callback URL is configured.
    pub async fn get_callback_url(&self) -> Result<Option<String>, AudDError> {
        let result = post_form(
            self.inner,
            "getCallbackUrl",
            &format!("{}/getCallbackUrl/", self.inner.api_base),
            &[],
            self.inner.read_policy(),
        )
        .await?;
        if result.is_null() {
            return Ok(None);
        }
        Ok(Some(
            result
                .as_str()
                .map_or_else(|| result.to_string(), str::to_string),
        ))
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
        extra_parameters: Option<&HashMap<String, String>>,
    ) -> Result<(), AudDError> {
        // extra_parameters first; typed fields win on collision.
        let mut fields: Vec<(&str, String)> = Vec::new();
        if let Some(extras) = extra_parameters {
            for (k, v) in extras {
                fields.push((k.as_str(), v.clone()));
            }
        }
        fields.push(("url", url.to_string()));
        fields.push(("radio_id", radio_id.to_string()));
        if let Some(cb) = callbacks {
            fields.push(("callbacks", cb.to_string()));
        }
        post_form(
            self.inner,
            "addStream",
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
            self.inner,
            "setStreamUrl",
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
            self.inner,
            "deleteStream",
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
            self.inner,
            "getStreams",
            &format!("{}/getStreams/", self.inner.api_base),
            &[],
            self.inner.read_policy(),
        )
        .await?;
        // Rows that don't decode are skipped; a non-list result holds none.
        Ok(lenient_list(result))
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
    /// fed by a background tokio task. [`LongpollPoll`] describes how each
    /// response is handled.
    ///
    /// On entry, performs a one-time `getCallbackUrl` preflight unless
    /// `opts.skip_callback_check == true`. If the server's response indicates
    /// that no callback URL is configured, [`AudDError::Api`] is returned with
    /// kind [`ErrorKind::InvalidRequest`] explaining how to fix it; any other
    /// server error passes through unchanged.
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

    /// One-step longpoll for the common `(api_token, radio_id)` case: derives
    /// the 9-char category internally and delegates to [`Self::longpoll`].
    ///
    /// Use [`Self::longpoll`] directly for the tokenless / share-the-category
    /// flow (server derives, ships only the 9-char string to a browser /
    /// mobile / embedded client).
    ///
    /// # Errors
    ///
    /// Same as [`Self::longpoll`] — propagates any preflight error.
    pub async fn longpoll_by_radio_id(
        &self,
        radio_id: i64,
        opts: LongpollOptions,
    ) -> Result<LongpollPoll, AudDError> {
        let category = self.derive_longpoll_category(radio_id);
        self.longpoll(&category, opts).await
    }

    async fn preflight_callback(&self) -> Result<(), AudDError> {
        match self.get_callback_url().await {
            Ok(_) => Ok(()),
            Err(AudDError::Api {
                code: NO_CALLBACK_ERROR_CODE,
                message,
                http_status,
                request_id,
                ..
            }) if indicates_no_callback_url(&message) => Err(AudDError::Api {
                code: 0,
                message: PREFLIGHT_NO_CALLBACK_HINT.to_string(),
                kind: ErrorKind::InvalidRequest,
                http_status,
                request_id,
                requested_params: std::collections::HashMap::new(),
                request_method: None,
                branded_message: None,
                raw_response: Value::Null,
            }),
            // Any other error — including a #19 whose message indicates a real
            // server condition (maintenance, blocked, ...) — passes through.
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
        // Size each longpoll request's HTTP deadline to the poll timeout plus
        // a margin, so poll timeouts above the transport's default budget
        // aren't cut short mid-poll.
        let request_timeout = longpoll_request_timeout(self.timeout());
        match self {
            Self::Authenticated {
                http, url, policy, ..
            } => {
                let url = url.clone();
                let policy = *policy;
                let http = http.clone();
                let params: Vec<(&str, String)> =
                    params.iter().map(|(k, v)| (*k, v.clone())).collect();
                retry_async(
                    || {
                        let http = http.clone();
                        let url = url.clone();
                        let params = params.clone();
                        async move { http.get(&url, &params, Some(request_timeout)).await }
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
                let params: Vec<(&str, String)> =
                    params.iter().map(|(k, v)| (*k, v.clone())).collect();
                retry_async(
                    || {
                        let http = http.clone();
                        let url = url.clone();
                        let params = params.clone();
                        async move { http.get(&url, &params, Some(request_timeout)).await }
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
    let (match_tx, match_rx) = mpsc::channel::<StreamCallbackMatch>(MATCH_BUFFER);
    let (notif_tx, notif_rx) = mpsc::channel::<StreamCallbackNotification>(NOTIFICATION_BUFFER);
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
/// a fatal error fires, or the consumer's streams are dropped.
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
    let mut idle_wait = LONGPOLL_IDLE_WAIT_START;

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

        let (events, next_since) = match read_longpoll_body(resp) {
            LongpollBody::Rejected(e) => {
                let _ = err_tx.send(e).await;
                return;
            }
            LongpollBody::Batch { events, next_since } => (events, next_since),
        };

        for event in events {
            match event {
                CallbackEvent::Match(m) => {
                    tokio::select! {
                        biased;
                        _ = shutdown_rx.recv() => return,
                        res = match_tx.send(m) => {
                            if res.is_err() { return; }
                        }
                    }
                }
                CallbackEvent::Notification(n) => match notif_tx.try_send(n) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!(
                            target: "audd",
                            "longpoll: notification dropped; {NOTIFICATION_BUFFER} unread notifications are already buffered"
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                },
            }
        }

        let advanced = next_since.is_some() && next_since != cur_since;
        if next_since.is_some() {
            cur_since = next_since;
        }
        if advanced {
            idle_wait = LONGPOLL_IDLE_WAIT_START;
        } else {
            // Nothing moved the cursor, so asking again at once could only
            // repeat this answer: pause first.
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => return,
                () = tokio::time::sleep(idle_wait) => {}
            }
            idle_wait = (idle_wait * 2).min(LONGPOLL_IDLE_WAIT_MAX);
        }
    }
}

/// What one successful (2xx) longpoll response carries.
#[derive(Debug)]
enum LongpollBody {
    /// Payloads to deliver, in order, and the `since_time` for the next
    /// request when the body carried one.
    Batch {
        events: Vec<CallbackEvent>,
        next_since: Option<i64>,
    },
    /// The server rejected the request itself; sending it again would get
    /// the same answer.
    Rejected(AudDError),
}

impl LongpollBody {
    fn nothing() -> Self {
        Self::Batch {
            events: Vec::new(),
            next_since: None,
        }
    }
}

/// Classify a longpoll response body: an event list, a timeout, an error, or
/// a callback body at the top level (`result` / `notification`, with an
/// optional `timestamp`). Anything else yields nothing.
fn read_longpoll_body(resp: HttpResponse) -> LongpollBody {
    let HttpResponse {
        json_body,
        http_status,
        request_id,
        ..
    } = resp;
    let Some(body) = json_body else {
        tracing::warn!(target: "audd", "longpoll: response body is not JSON");
        return LongpollBody::nothing();
    };
    let Some(obj) = body.as_object() else {
        tracing::warn!(target: "audd", "longpoll: response body is not a JSON object");
        return LongpollBody::nothing();
    };

    if obj.get("status").and_then(Value::as_str) == Some("error") {
        return LongpollBody::Rejected(raise_from_error_response(
            &body,
            http_status,
            request_id,
            false,
        ));
    }

    if let Some(events) = obj.get("events") {
        let Some(list) = events.as_array() else {
            tracing::warn!(target: "audd", "longpoll: `events` is not a list");
            return LongpollBody::nothing();
        };
        let mut delivered = Vec::with_capacity(list.len());
        let mut newest: Option<i64> = None;
        for event in list {
            if let Some(ts) = event.get("timestamp").and_then(timestamp_of) {
                newest = Some(newest.map_or(ts, |n| n.max(ts)));
            }
            if let Some(parsed) = event.get("data").and_then(callback_event_from) {
                delivered.push(parsed);
            }
        }
        return LongpollBody::Batch {
            events: delivered,
            next_since: newest,
        };
    }

    if obj.contains_key("result") || obj.contains_key("notification") {
        return LongpollBody::Batch {
            events: callback_event_from(&body).into_iter().collect(),
            next_since: obj.get("timestamp").and_then(timestamp_of),
        };
    }

    if let Some(error) = obj.get("error") {
        let message = error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_string);
        return LongpollBody::Rejected(AudDError::Api {
            code: 0,
            message,
            kind: ErrorKind::InvalidRequest,
            http_status,
            request_id,
            requested_params: HashMap::new(),
            request_method: None,
            branded_message: None,
            raw_response: body,
        });
    }

    if obj.contains_key("timeout") {
        return LongpollBody::Batch {
            events: Vec::new(),
            next_since: obj.get("timestamp").and_then(timestamp_of),
        };
    }

    tracing::warn!(target: "audd", "longpoll: response body has an unrecognized shape");
    LongpollBody::nothing()
}

fn timestamp_of(v: &Value) -> Option<i64> {
    lenient_from_value(v.clone())
}

/// Parse one event's `data` (an object, or a JSON string holding one) with the
/// callback parser. Returns `None` for data that doesn't parse and for a
/// recognition without any song.
fn callback_event_from(data: &Value) -> Option<CallbackEvent> {
    let decoded;
    let data = if let Value::String(text) = data {
        decoded = serde_json::from_str::<Value>(text).ok();
        decoded.as_ref()?
    } else {
        data
    };
    match parse_callback(data.clone()) {
        Ok(CallbackEvent::Match(m))
            if m.song == StreamCallbackSong::default() && m.alternatives.is_empty() =>
        {
            tracing::debug!(target: "audd", "longpoll: skipped a recognition with no results");
            None
        }
        Ok(event) => Some(event),
        Err(e) => {
            tracing::warn!(target: "audd", "longpoll: skipped an event that doesn't parse: {e}");
            None
        }
    }
}

/// Internal — POST a form body to a streams-namespace endpoint and return the
/// `result` field on success. Emits request/response/exception lifecycle
/// events for the registered `on_event` hook.
async fn post_form(
    inner: &AudDInner,
    method: &str,
    url: &str,
    fields: &[(&str, String)],
    policy: RetryPolicy,
) -> Result<Value, AudDError> {
    let http = &inner.http;
    let url = url.to_string();
    let fields: Vec<(&str, String)> = fields.iter().map(|(k, v)| (*k, v.clone())).collect();
    let started = inner.emit_request(method, &url);
    let resp = retry_async(
        || {
            let http = http.clone();
            let url = url.clone();
            let fields = fields.clone();
            async move { http.post_form(&url, &fields, None, None).await }
        },
        policy,
    )
    .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            inner.emit_exception(method, &url, started, &e);
            return Err(e);
        }
    };
    inner.emit_response(method, &url, started, &resp);
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
    fn longpoll_request_timeout_sizes_above_poll_timeout() {
        assert_eq!(
            longpoll_request_timeout(50),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            longpoll_request_timeout(300),
            std::time::Duration::from_secs(310)
        );
        // Negative (invalid) timeouts degrade to just the margin.
        assert_eq!(
            longpoll_request_timeout(-1),
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn no_callback_url_signal_detection() {
        assert!(indicates_no_callback_url("Internal error"));
        assert!(indicates_no_callback_url("no callback url set"));
        assert!(!indicates_no_callback_url(
            "Scheduled maintenance, try again later"
        ));
        assert!(!indicates_no_callback_url("request blocked"));
    }

    fn ok(body: Value) -> HttpResponse {
        HttpResponse {
            raw_text: body.to_string(),
            json_body: Some(body),
            http_status: 200,
            request_id: None,
        }
    }

    fn batch(body: LongpollBody) -> (Vec<CallbackEvent>, Option<i64>) {
        match body {
            LongpollBody::Batch { events, next_since } => (events, next_since),
            LongpollBody::Rejected(e) => panic!("unexpected rejection: {e:?}"),
        }
    }

    fn recognition(radio_id: i64, title: &str) -> Value {
        serde_json::json!({
            "status": "success",
            "result": {
                "radio_id": radio_id,
                "results": [{"artist": "A", "title": title, "score": 100}]
            }
        })
    }

    #[test]
    fn events_are_parsed_in_order_and_newest_timestamp_wins() {
        let body = serde_json::json!({"events": [
            {"timestamp": 20, "category": "c", "data": recognition(1, "first")},
            {"timestamp": 10, "category": "c", "data": {
                "status": "-",
                "notification": {"radio_id": 1, "notification_code": 650},
                "time": 5
            }},
            {"timestamp": 30, "category": "c", "data": recognition(1, "third").to_string()},
        ]});
        let (events, next) = batch(read_longpoll_body(ok(body)));
        assert_eq!(next, Some(30));
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[0].as_match().unwrap().song.title.as_deref(),
            Some("first")
        );
        assert_eq!(
            events[1].as_notification().unwrap().notification_code,
            Some(650)
        );
        // `data` sent as a JSON string is decoded first.
        assert_eq!(
            events[2].as_match().unwrap().song.title.as_deref(),
            Some("third")
        );
    }

    #[test]
    fn unparseable_and_empty_events_are_skipped_but_advance_the_cursor() {
        let body = serde_json::json!({"events": [
            {"timestamp": 1, "data": {"status": "success", "result": {"radio_id": 1, "results": []}}},
            {"timestamp": 2, "data": {"status": "success"}},
            {"timestamp": 3, "data": "not json"},
            {"timestamp": "4", "data": 17},
            {"data": recognition(2, "kept")},
        ]});
        let (events, next) = batch(read_longpoll_body(ok(body)));
        assert_eq!(next, Some(4));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_match().unwrap().radio_id, Some(2));
    }

    #[test]
    fn empty_event_list_yields_nothing() {
        let (events, next) = batch(read_longpoll_body(ok(serde_json::json!({"events": []}))));
        assert_eq!(events, Vec::<CallbackEvent>::new());
        assert_eq!(next, None);
    }

    #[test]
    fn timeout_body_advances_the_cursor() {
        let body = serde_json::json!({"timeout": "no events before timeout", "timestamp": 77});
        let (events, next) = batch(read_longpoll_body(ok(body)));
        assert_eq!(events, Vec::<CallbackEvent>::new());
        assert_eq!(next, Some(77));
    }

    #[test]
    fn error_body_is_an_invalid_request_with_the_server_text() {
        let body = serde_json::json!({"error": "Invalid timeout arg.  Must be 1-120."});
        match read_longpoll_body(ok(body)) {
            LongpollBody::Rejected(e) => {
                assert!(e.is_invalid_request(), "{e:?}");
                match e {
                    AudDError::Api { message, .. } => {
                        assert_eq!(message, "Invalid timeout arg.  Must be 1-120.");
                    }
                    other => panic!("expected Api, got {other:?}"),
                }
            }
            other @ LongpollBody::Batch { .. } => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn standard_error_envelope_is_rejected_with_its_code() {
        let body = serde_json::json!({
            "status": "error",
            "error": {"error_code": 900, "error_message": "bad token"}
        });
        match read_longpoll_body(ok(body)) {
            LongpollBody::Rejected(e) => assert!(e.is_authentication(), "{e:?}"),
            other @ LongpollBody::Batch { .. } => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn top_level_callback_body_is_one_event() {
        let mut body = recognition(9, "legacy");
        body["timestamp"] = serde_json::json!(5);
        let (events, next) = batch(read_longpoll_body(ok(body)));
        assert_eq!(next, Some(5));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_match().unwrap().radio_id, Some(9));

        let (events, next) = batch(read_longpoll_body(ok(recognition(9, "no ts"))));
        assert_eq!(next, None);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn junk_bodies_yield_nothing() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({"events": "nope"}),
            serde_json::json!([1, 2, 3]),
            serde_json::json!("text"),
            serde_json::json!({"timeout": "no events before timeout"}),
        ] {
            let (events, next) = batch(read_longpoll_body(ok(body)));
            assert_eq!(events, Vec::<CallbackEvent>::new());
            assert_eq!(next, None);
        }
        let not_json = HttpResponse {
            json_body: None,
            http_status: 200,
            request_id: None,
            raw_text: "<html>".into(),
        };
        let (events, next) = batch(read_longpoll_body(not_json));
        assert_eq!(events, Vec::<CallbackEvent>::new());
        assert_eq!(next, None);
    }
}
