//! Public `AudD` client and its builder.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::advanced::Advanced;
use crate::custom_catalog::CustomCatalog;
use crate::errors::{raise_from_error_response, AudDError};
use crate::http::{HttpClient, HttpResponse, TimeoutProfile};
use crate::models::{EnterpriseChunkResult, EnterpriseMatch, RecognitionResult};
use crate::retry::{retry_async, RetryClass, RetryPolicy};
use crate::source::{prepare_source, Source};
use crate::streams::Streams;

/// Lifecycle stage an [`AudDEvent`] reports on. Ordered by typical occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Emitted just before the SDK dispatches a request.
    Request,
    /// Emitted on a successful HTTP round-trip (carries `http_status`,
    /// `request_id`, and `elapsed`). Note: an HTTP 4xx/5xx still produces a
    /// `Response` event — the SDK treats body-level `status=error` as the
    /// only "exception" path.
    Response,
    /// Emitted when the request never produced an HTTP response (transport
    /// failure, timeout). `http_status` and `request_id` will be `None`.
    Exception,
}

/// Inspection event emitted by the SDK request lifecycle.
///
/// Plain data; never carries the api_token or request/response body bytes.
/// Hook authors who need extra context can stash provider-specific fields in
/// [`Self::extras`] (the SDK leaves it empty).
///
/// Implements `Serialize` / `Deserialize` so observability hooks can write
/// events straight to a log/queue. `elapsed` round-trips as a `Duration` (a
/// `{secs, nanos}` object).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudDEvent {
    /// Lifecycle stage.
    pub kind: EventKind,
    /// AudD method name (e.g. `"recognize"`, `"recognize_enterprise"`).
    pub method: String,
    /// Endpoint URL the SDK targeted (no `api_token` query parameter).
    pub url: String,
    /// `x-request-id` header from the response, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// HTTP status the server returned. Set on [`EventKind::Response`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Wall-clock time spent on this request (request kinds report `0`).
    pub elapsed: Duration,
    /// AudD numeric error code, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i32>,
    /// Reserved for SDK consumers; the SDK itself ships an empty map.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extras: HashMap<String, Value>,
}

/// Signature for the [`AudDBuilder::on_event`] inspection hook. Hook panics
/// are caught by the SDK so observability never breaks the request path.
pub type OnEventHook = Arc<dyn Fn(&AudDEvent) + Send + Sync + 'static>;

const API_BASE: &str = "https://api.audd.io";
const ENTERPRISE_BASE: &str = "https://enterprise.audd.io";
const HTTP_CLIENT_ERROR_FLOOR: u16 = 400;
const DEPRECATED_PARAMS_CODE: i32 = 51;

/// Environment variable consulted when an api_token is not passed explicitly.
pub(crate) const TOKEN_ENV_VAR: &str = "AUDD_API_TOKEN";

/// Resolve an api_token from `(explicit arg or "")` → `AUDD_API_TOKEN` → error.
///
/// Used by [`AudDBuilder::build`] and [`AudD::from_env`]. Empty strings count
/// as "not supplied" — same shape as the audd-go and audd-python SDKs.
fn resolve_token(explicit: &str) -> Result<String, AudDError> {
    if !explicit.is_empty() {
        return Ok(explicit.to_string());
    }
    if let Ok(env) = std::env::var(TOKEN_ENV_VAR) {
        if !env.is_empty() {
            return Ok(env);
        }
    }
    Err(AudDError::Configuration {
        message: format!(
            "AudD api_token not supplied and {TOKEN_ENV_VAR} env var is unset. \
Get a token at https://dashboard.audd.io and pass it to AudD::new(...) or \
set {TOKEN_ENV_VAR} and call AudD::from_env()."
        ),
    })
}

/// Async client for the AudD music recognition API.
///
/// Construct with [`AudD::new`] for the common case, or [`AudD::builder`] to
/// override retries, timeouts, base URLs, or supply a configured
/// [`reqwest::Client`].
///
/// `AudD` holds an internal connection pool. Drop it (or call
/// [`AudD::close`]) once you're done. Cloning `AudD` is cheap.
#[derive(Debug, Clone)]
pub struct AudD {
    inner: AudDInner,
}

#[derive(Clone)]
pub(crate) struct AudDInner {
    /// Shared, mutable token. The same `Arc<RwLock<String>>` is held by the
    /// standard + enterprise [`HttpClient`]s, so `AudD::set_api_token` updates
    /// all three points in one swap.
    pub(crate) api_token: Arc<RwLock<String>>,
    pub(crate) http: HttpClient,
    pub(crate) enterprise_http: HttpClient,
    pub(crate) max_attempts: u32,
    pub(crate) backoff_factor: f64,
    pub(crate) api_base: String,
    pub(crate) enterprise_base: String,
    /// Optional inspection hook. See [`AudDBuilder::on_event`].
    pub(crate) on_event: Option<OnEventHook>,
}

impl std::fmt::Debug for AudDInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `OnEventHook` is `dyn Fn`, which doesn't implement `Debug`. Show
        // whether one is registered without trying to render it.
        f.debug_struct("AudDInner")
            .field("api_token", &"<redacted>")
            .field("http", &self.http)
            .field("enterprise_http", &self.enterprise_http)
            .field("max_attempts", &self.max_attempts)
            .field("backoff_factor", &self.backoff_factor)
            .field("api_base", &self.api_base)
            .field("enterprise_base", &self.enterprise_base)
            .field(
                "on_event",
                &self.on_event.as_ref().map(|_| "Fn(&AudDEvent)"),
            )
            .finish()
    }
}

impl AudDInner {
    /// Snapshot the current api_token under a brief read lock.
    pub(crate) fn api_token(&self) -> String {
        self.api_token
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    /// Invoke the registered [`OnEventHook`] (if any) with the given event,
    /// catching panics so a misbehaved hook never breaks the request path.
    pub(crate) fn emit_event(&self, event: &AudDEvent) {
        let Some(hook) = self.on_event.as_ref() else {
            return;
        };
        // `catch_unwind` requires `UnwindSafe`; the hook is `Fn` over a borrow
        // and may close over arbitrary state, so wrap with `AssertUnwindSafe`.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| hook(event)));
    }

    pub(crate) fn read_policy(&self) -> RetryPolicy {
        RetryPolicy::new(RetryClass::Read)
            .with_max_attempts(self.max_attempts)
            .with_backoff_factor(self.backoff_factor)
    }

    pub(crate) fn recognition_policy(&self) -> RetryPolicy {
        RetryPolicy::new(RetryClass::Recognition)
            .with_max_attempts(self.max_attempts)
            .with_backoff_factor(self.backoff_factor)
    }

    pub(crate) fn mutating_policy(&self) -> RetryPolicy {
        RetryPolicy::new(RetryClass::Mutating)
            .with_max_attempts(self.max_attempts)
            .with_backoff_factor(self.backoff_factor)
    }
}

/// Builder for [`AudD`]. Construct via [`AudD::builder`].
#[derive(Clone)]
pub struct AudDBuilder {
    api_token: String,
    max_attempts: u32,
    backoff_factor: f64,
    reqwest_client: Option<reqwest::Client>,
    api_base: String,
    enterprise_base: String,
    on_event: Option<OnEventHook>,
}

impl std::fmt::Debug for AudDBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudDBuilder")
            .field("api_token", &"<redacted>")
            .field("max_attempts", &self.max_attempts)
            .field("backoff_factor", &self.backoff_factor)
            .field(
                "reqwest_client",
                &self.reqwest_client.as_ref().map(|_| "<custom>"),
            )
            .field("api_base", &self.api_base)
            .field("enterprise_base", &self.enterprise_base)
            .field(
                "on_event",
                &self.on_event.as_ref().map(|_| "Fn(&AudDEvent)"),
            )
            .finish()
    }
}

impl AudDBuilder {
    fn new(api_token: impl Into<String>) -> Self {
        Self {
            api_token: api_token.into(),
            max_attempts: 3,
            backoff_factor: 0.5,
            reqwest_client: None,
            api_base: API_BASE.to_string(),
            enterprise_base: ENTERPRISE_BASE.to_string(),
            on_event: None,
        }
    }

    /// Set the maximum number of attempts per call (including the first attempt).
    /// Default is `3`. `1` disables retries.
    #[must_use]
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    /// Set the base backoff factor (in seconds) for retries. Default is `0.5`.
    #[must_use]
    pub fn backoff_factor(mut self, f: f64) -> Self {
        self.backoff_factor = f;
        self
    }

    /// Inject a configured [`reqwest::Client`]. Use this for corporate proxies,
    /// mTLS, custom CA bundles, observability sidecars, etc.
    ///
    /// When you supply your own client we don't override its timeouts; configure
    /// timeouts via `reqwest::ClientBuilder` before passing it in. The standard
    /// and enterprise endpoints will share the client.
    #[must_use]
    pub fn reqwest_client(mut self, client: reqwest::Client) -> Self {
        self.reqwest_client = Some(client);
        self
    }

    /// Override the base URL for standard endpoints — useful for testing
    /// against `wiremock`.
    #[must_use]
    pub fn api_base(mut self, url: impl Into<String>) -> Self {
        self.api_base = url.into();
        self
    }

    /// Override the base URL for the enterprise endpoint.
    #[must_use]
    pub fn enterprise_base(mut self, url: impl Into<String>) -> Self {
        self.enterprise_base = url.into();
        self
    }

    /// Register an inspection hook that receives request / response /
    /// exception lifecycle events. The hook is invoked synchronously inside
    /// the request future; **panics raised by the hook are caught and
    /// suppressed** so observability never breaks the request path.
    ///
    /// Events never carry the `api_token` or request / response body bytes —
    /// hook authors who want body access should layer their own
    /// `reqwest::Client` and inject it via [`Self::reqwest_client`]. See spec
    /// §7.7a.
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use audd::{AudD, AudDEvent, EventKind};
    /// let audd = AudD::builder("test")
    ///     .on_event(Arc::new(|e: &AudDEvent| {
    ///         eprintln!("audd[{:?}] {} -> {:?}", e.kind, e.method, e.http_status);
    ///     }))
    ///     .build()
    ///     .unwrap();
    /// # let _ = audd;
    /// ```
    #[must_use]
    pub fn on_event(mut self, hook: OnEventHook) -> Self {
        self.on_event = Some(hook);
        self
    }

    /// Build the [`AudD`] client.
    ///
    /// If the builder was constructed with an empty token, the SDK falls back
    /// to the `AUDD_API_TOKEN` environment variable. If neither is set, returns
    /// [`AudDError::Configuration`] pointing at <https://dashboard.audd.io>.
    ///
    /// # Errors
    ///
    /// * [`AudDError::Configuration`] when no token can be resolved.
    /// * [`AudDError::Connection`] if `reqwest::Client::build()` fails.
    pub fn build(self) -> Result<AudD, AudDError> {
        let token = resolve_token(&self.api_token)?;
        // One `Arc<RwLock<String>>` shared across the standard + enterprise
        // transports — `AudD::set_api_token` rotates them in lock-step.
        let token = Arc::new(RwLock::new(token));
        let (http, enterprise_http) = if let Some(client) = self.reqwest_client {
            (
                HttpClient::from_client(Arc::clone(&token), client.clone()),
                HttpClient::from_client(Arc::clone(&token), client),
            )
        } else {
            (
                HttpClient::new(Arc::clone(&token), TimeoutProfile::Standard)?,
                HttpClient::new(Arc::clone(&token), TimeoutProfile::Enterprise)?,
            )
        };
        Ok(AudD {
            inner: AudDInner {
                api_token: token,
                http,
                enterprise_http,
                max_attempts: self.max_attempts,
                backoff_factor: self.backoff_factor,
                api_base: self.api_base,
                enterprise_base: self.enterprise_base,
                on_event: self.on_event,
            },
        })
    }
}

impl AudD {
    /// Build a client with the standard defaults. Pass `""` to fall back on the
    /// `AUDD_API_TOKEN` environment variable.
    ///
    /// # Panics
    ///
    /// Panics if no token is resolvable (use [`Self::try_new`] /
    /// [`Self::from_env`] for a `Result`-returning sibling) or if
    /// `reqwest::Client::build()` fails — exceedingly rare on a system with a
    /// working TLS stack.
    #[must_use]
    pub fn new(api_token: impl Into<String>) -> Self {
        Self::builder(api_token)
            .build()
            .expect("api_token must be supplied or AUDD_API_TOKEN must be set")
    }

    /// Result-returning sibling of [`Self::new`]: build a client with defaults,
    /// falling back to `AUDD_API_TOKEN` when `api_token` is empty.
    ///
    /// # Errors
    ///
    /// * [`AudDError::Configuration`] when no token can be resolved.
    /// * [`AudDError::Connection`] if `reqwest::Client::build()` fails.
    pub fn try_new(api_token: impl Into<String>) -> Result<Self, AudDError> {
        Self::builder(api_token).build()
    }

    /// Build a client whose token comes from the `AUDD_API_TOKEN` environment
    /// variable. Equivalent to [`Self::try_new`] with an empty argument.
    ///
    /// # Errors
    ///
    /// * [`AudDError::Configuration`] if `AUDD_API_TOKEN` is unset or empty.
    /// * [`AudDError::Connection`] if `reqwest::Client::build()` fails.
    pub fn from_env() -> Result<Self, AudDError> {
        Self::builder("").build()
    }

    /// Begin building an [`AudD`].
    pub fn builder(api_token: impl Into<String>) -> AudDBuilder {
        AudDBuilder::new(api_token)
    }

    /// `streams.*` namespace.
    pub fn streams(&self) -> Streams<'_> {
        Streams::new(&self.inner)
    }

    /// `custom_catalog.*` namespace. Read the warning on
    /// [`CustomCatalog::add`] before using.
    pub fn custom_catalog(&self) -> CustomCatalog<'_> {
        CustomCatalog::new(&self.inner)
    }

    /// `advanced.*` namespace — lyrics search and a generic raw-request escape
    /// hatch.
    pub fn advanced(&self) -> Advanced<'_> {
        Advanced::new(&self.inner)
    }

    /// Recognize a song from a URL, file path, raw bytes, or async reader.
    ///
    /// Returns `Ok(None)` when the server returns `status=success` with
    /// `result=null` — i.e. no match was found.
    ///
    /// `return_` accepts a slice of metadata service names (e.g.
    /// `&["apple_music".into(), "spotify".into()]`).
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport, server, or parse failures.
    pub async fn recognize(
        &self,
        source: impl Into<Source>,
    ) -> Result<Option<RecognitionResult>, AudDError> {
        self.recognize_with(source, None, None, None).await
    }

    /// Like [`Self::recognize`] but with the optional knobs explicit.
    ///
    /// # Errors
    ///
    /// Same as [`Self::recognize`].
    pub async fn recognize_with(
        &self,
        source: impl Into<Source>,
        return_: Option<&[String]>,
        market: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<Option<RecognitionResult>, AudDError> {
        let reopen = prepare_source(source.into())?;
        let return_str = return_.map(|v| v.join(","));
        let market = market.map(str::to_string);
        let url = format!("{}/", self.inner.api_base);
        let http = self.inner.http.clone();

        let started = Instant::now();
        self.inner.emit_event(&AudDEvent {
            kind: EventKind::Request,
            method: "recognize".into(),
            url: url.clone(),
            request_id: None,
            http_status: None,
            elapsed: Duration::from_secs(0),
            error_code: None,
            extras: HashMap::new(),
        });

        let resp = match retry_async(
            || {
                let reopen = &reopen;
                let return_str = return_str.clone();
                let market = market.clone();
                let url = url.clone();
                let http = http.clone();
                async move {
                    let prepared = reopen().await?;
                    let form = prepared.apply(reqwest::multipart::Form::new());
                    let mut fields: Vec<(&str, String)> = Vec::new();
                    if let Some(r) = return_str.as_ref() {
                        fields.push(("return", r.clone()));
                    }
                    if let Some(m) = market.as_ref() {
                        fields.push(("market", m.clone()));
                    }
                    http.post_form(&url, &fields, Some(form), timeout).await
                }
            },
            self.inner.recognition_policy(),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                self.inner.emit_event(&AudDEvent {
                    kind: EventKind::Exception,
                    method: "recognize".into(),
                    url,
                    request_id: e.request_id().map(str::to_string),
                    http_status: None,
                    elapsed: started.elapsed(),
                    error_code: e.error_code(),
                    extras: HashMap::new(),
                });
                return Err(e);
            }
        };

        self.inner.emit_event(&AudDEvent {
            kind: EventKind::Response,
            method: "recognize".into(),
            url,
            request_id: resp.request_id.clone(),
            http_status: Some(resp.http_status),
            elapsed: started.elapsed(),
            error_code: None,
            extras: HashMap::new(),
        });
        decode_recognize(resp)
    }

    /// Recognize a long file via the enterprise endpoint. Returns all matches
    /// across all chunks. Default read/write timeout is 1 hour.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport, server, or parse failures.
    pub async fn recognize_enterprise(
        &self,
        source: impl Into<Source>,
        opts: EnterpriseOptions<'_>,
    ) -> Result<Vec<EnterpriseMatch>, AudDError> {
        let reopen = prepare_source(source.into())?;
        let return_str = opts.return_.map(|v| v.join(","));
        let url = format!("{}/", self.inner.enterprise_base);
        let http = self.inner.enterprise_http.clone();
        let extra: Vec<(&str, String)> = build_enterprise_fields(return_str.as_deref(), &opts);

        let started = Instant::now();
        self.inner.emit_event(&AudDEvent {
            kind: EventKind::Request,
            method: "recognize_enterprise".into(),
            url: url.clone(),
            request_id: None,
            http_status: None,
            elapsed: Duration::from_secs(0),
            error_code: None,
            extras: HashMap::new(),
        });

        let resp = match retry_async(
            || {
                let reopen = &reopen;
                let extra = extra.clone();
                let url = url.clone();
                let http = http.clone();
                async move {
                    let prepared = reopen().await?;
                    let form = prepared.apply(reqwest::multipart::Form::new());
                    http.post_form(&url, &extra, Some(form), opts.timeout).await
                }
            },
            self.inner.recognition_policy(),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                self.inner.emit_event(&AudDEvent {
                    kind: EventKind::Exception,
                    method: "recognize_enterprise".into(),
                    url,
                    request_id: e.request_id().map(str::to_string),
                    http_status: None,
                    elapsed: started.elapsed(),
                    error_code: e.error_code(),
                    extras: HashMap::new(),
                });
                return Err(e);
            }
        };

        self.inner.emit_event(&AudDEvent {
            kind: EventKind::Response,
            method: "recognize_enterprise".into(),
            url,
            request_id: resp.request_id.clone(),
            http_status: Some(resp.http_status),
            elapsed: started.elapsed(),
            error_code: None,
            extras: HashMap::new(),
        });
        decode_enterprise(resp)
    }

    /// Drop the underlying HTTP transport explicitly. Equivalent to dropping
    /// the `AudD` value (provided here for parity with the sibling SDKs'
    /// context-manager protocol).
    pub async fn close(self) {
        self.inner.http.close();
        self.inner.enterprise_http.close();
    }

    /// Snapshot the in-effect api_token (after any rotations).
    ///
    /// Returns an owned `String` because the token sits behind an
    /// `Arc<RwLock>` to support [`Self::set_api_token`].
    #[must_use]
    pub fn api_token(&self) -> String {
        self.inner.api_token()
    }

    /// Atomically rotate the api_token used for subsequent requests.
    ///
    /// In-flight requests continue with the previous snapshot — there's no
    /// abort. Thread-safe across concurrent `recognize`, `streams.*`, etc.
    /// calls; backed by a shared `Arc<RwLock<String>>` so the standard and
    /// enterprise transports both pick up the new token immediately.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Configuration`] when `new_token` is empty.
    pub fn set_api_token(&self, new_token: impl Into<String>) -> Result<(), AudDError> {
        let new_token = new_token.into();
        if new_token.is_empty() {
            return Err(AudDError::Configuration {
                message: "set_api_token requires a non-empty token".to_string(),
            });
        }
        // Both `HttpClient`s hold clones of the same `Arc<RwLock<String>>`,
        // so a single `write()` here propagates to both transports.
        let mut guard = self
            .inner
            .api_token
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *guard = new_token;
        Ok(())
    }
}

/// Optional parameters for [`AudD::recognize_enterprise`].
#[derive(Debug, Default)]
pub struct EnterpriseOptions<'a> {
    /// Metadata services to return (`apple_music`, `deezer`, `napster`,
    /// `musicbrainz`).
    pub return_: Option<&'a [String]>,
    /// Skip the first N seconds of the source.
    pub skip: Option<i64>,
    /// Sample every N seconds.
    pub every: Option<i64>,
    /// Maximum number of matches.
    pub limit: Option<i64>,
    /// Skip the first N seconds (same as `skip`, kept separately for the
    /// upstream parameter name).
    pub skip_first_seconds: Option<i64>,
    /// Whether to attach timecodes to matches.
    pub use_timecode: Option<bool>,
    /// Whether to compute precise per-segment offsets.
    pub accurate_offsets: Option<bool>,
    /// Per-call timeout override.
    pub timeout: Option<Duration>,
}

fn build_enterprise_fields(
    return_str: Option<&str>,
    opts: &EnterpriseOptions<'_>,
) -> Vec<(&'static str, String)> {
    let mut fields: Vec<(&'static str, String)> = Vec::new();
    if let Some(r) = return_str {
        fields.push(("return", r.to_string()));
    }
    if let Some(v) = opts.skip {
        fields.push(("skip", v.to_string()));
    }
    if let Some(v) = opts.every {
        fields.push(("every", v.to_string()));
    }
    if let Some(v) = opts.limit {
        fields.push(("limit", v.to_string()));
    }
    if let Some(v) = opts.skip_first_seconds {
        fields.push(("skip_first_seconds", v.to_string()));
    }
    if let Some(v) = opts.use_timecode {
        fields.push((
            "use_timecode",
            if v { "true".into() } else { "false".into() },
        ));
    }
    if let Some(v) = opts.accurate_offsets {
        fields.push((
            "accurate_offsets",
            if v { "true".into() } else { "false".into() },
        ));
    }
    fields
}

fn decode_recognize(resp: HttpResponse) -> Result<Option<RecognitionResult>, AudDError> {
    let body = decode_or_raise(resp, /* custom_catalog_context = */ false)?;
    let result = body.get("result").cloned().unwrap_or(Value::Null);
    if result.is_null() {
        return Ok(None);
    }
    let rr: RecognitionResult =
        serde_json::from_value(result.clone()).map_err(|e| AudDError::Serialization {
            message: format!("could not parse recognize result: {e}"),
            raw_text: result.to_string(),
        })?;
    Ok(Some(rr))
}

fn decode_enterprise(resp: HttpResponse) -> Result<Vec<EnterpriseMatch>, AudDError> {
    let body = decode_or_raise(resp, /* custom_catalog_context = */ false)?;
    let chunks_value = body.get("result").cloned().unwrap_or(Value::Null);
    if chunks_value.is_null() {
        return Ok(Vec::new());
    }
    let chunks: Vec<EnterpriseChunkResult> =
        serde_json::from_value(chunks_value.clone()).map_err(|e| AudDError::Serialization {
            message: format!("could not parse enterprise result: {e}"),
            raw_text: chunks_value.to_string(),
        })?;
    Ok(chunks.into_iter().flat_map(|c| c.songs).collect())
}

/// Inspect a response and either return its body or raise the appropriate
/// typed error. Implements the audd-python `_decode_or_raise` semantics.
pub(crate) fn decode_or_raise(
    resp: HttpResponse,
    custom_catalog_context: bool,
) -> Result<Value, AudDError> {
    let HttpResponse {
        json_body,
        http_status,
        request_id,
        raw_text,
    } = resp;

    let Some(mut body) = json_body else {
        if http_status >= HTTP_CLIENT_ERROR_FLOOR {
            return Err(AudDError::Server {
                http_status,
                message: format!("HTTP {http_status} with non-JSON response body"),
                request_id,
                raw_response: raw_text,
            });
        }
        return Err(AudDError::Serialization {
            message: "Unparseable response".to_string(),
            raw_text,
        });
    };

    maybe_warn_and_strip(&mut body);

    let status = body.get("status").and_then(Value::as_str);
    if status == Some("error") {
        return Err(raise_from_error_response(
            &body,
            http_status,
            request_id,
            custom_catalog_context,
        ));
    }
    if status == Some("success") {
        return Ok(body);
    }
    Err(AudDError::Server {
        http_status,
        message: format!("Unexpected response status: {status:?}"),
        request_id,
        raw_response: body.to_string(),
    })
}

/// If the body carries a code-51 deprecation warning + a usable result, emit a
/// `tracing::warn!` and rewrite the body to look like a normal success
/// response.
fn maybe_warn_and_strip(body: &mut Value) {
    let code_matches = body
        .get("error")
        .and_then(|e| e.get("error_code"))
        .and_then(|c| c.as_i64())
        .is_some_and(|c| i32::try_from(c).ok() == Some(DEPRECATED_PARAMS_CODE));
    let result_is_usable = body.get("result").is_some_and(|r| !r.is_null());
    let is_pass_through = code_matches && result_is_usable;
    if !is_pass_through {
        return;
    }
    let msg = body
        .get("error")
        .and_then(|e| e.get("error_message"))
        .and_then(Value::as_str)
        .unwrap_or("Deprecated parameter used")
        .to_string();
    tracing::warn!(target: "audd", code = DEPRECATED_PARAMS_CODE, "{msg}");
    if let Some(obj) = body.as_object_mut() {
        obj.remove("error");
        obj.insert("status".into(), Value::String("success".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resp_ok(body: Value) -> HttpResponse {
        HttpResponse {
            json_body: Some(body.clone()),
            http_status: 200,
            request_id: None,
            raw_text: body.to_string(),
        }
    }

    #[test]
    fn decode_recognize_success() {
        let body = json!({
            "status": "success",
            "result": {"timecode": "00:01", "artist": "X", "title": "Y"}
        });
        let r = decode_recognize(resp_ok(body)).unwrap().unwrap();
        assert_eq!(r.artist.as_deref(), Some("X"));
    }

    #[test]
    fn decode_recognize_no_match() {
        let body = json!({"status": "success", "result": null});
        let r = decode_recognize(resp_ok(body)).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn decode_recognize_error() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 900, "error_message": "bad"}
        });
        let e = decode_recognize(resp_ok(body)).unwrap_err();
        assert!(e.is_authentication());
    }

    #[test]
    fn http_5xx_with_html_is_server_error() {
        let r = HttpResponse {
            json_body: None,
            http_status: 502,
            request_id: None,
            raw_text: "<html>bad gateway</html>".to_string(),
        };
        let e = decode_or_raise(r, false).unwrap_err();
        match e {
            AudDError::Server { http_status, .. } => assert_eq!(http_status, 502),
            other => panic!("not Server: {other:?}"),
        }
    }

    #[test]
    fn http_2xx_with_garbage_is_serialization_error() {
        let r = HttpResponse {
            json_body: None,
            http_status: 200,
            request_id: None,
            raw_text: "not json".to_string(),
        };
        let e = decode_or_raise(r, false).unwrap_err();
        assert!(matches!(e, AudDError::Serialization { .. }));
    }

    #[test]
    fn code_51_passes_through_with_result() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 51, "error_message": "deprecated param X"},
            "result": {"timecode": "00:01", "artist": "X", "title": "Y"}
        });
        let r = decode_recognize(resp_ok(body)).unwrap().unwrap();
        assert_eq!(r.artist.as_deref(), Some("X"));
    }

    #[test]
    fn code_51_without_result_raises() {
        let body = json!({
            "status": "error",
            "error": {"error_code": 51, "error_message": "deprecated"},
            "result": null
        });
        let e = decode_recognize(resp_ok(body)).unwrap_err();
        assert!(e.is_invalid_request());
    }

    #[test]
    fn enterprise_decode() {
        let body = json!({
            "status": "success",
            "result": [
                {"songs": [{"score": 80, "timecode": "00:01"}], "offset": "00:00"}
            ]
        });
        let v = decode_enterprise(resp_ok(body)).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].score, 80);
    }

    /// Serializes env-var-mutating tests in this module so they don't race
    /// against each other (or against any other env-touching test that grows
    /// up around them). `std::env::set_var` is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the lock, snapshot `AUDD_API_TOKEN`, set it (or unset it), and
    /// restore on drop — keeps test ordering hygienic.
    struct EnvGuard {
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(value: Option<&str>) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var(TOKEN_ENV_VAR).ok();
            // SAFETY: We hold the cross-test mutex, so no other audd test can
            // observe a half-mutated env in this process.
            unsafe {
                if let Some(v) = value {
                    std::env::set_var(TOKEN_ENV_VAR, v);
                } else {
                    std::env::remove_var(TOKEN_ENV_VAR);
                }
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: Mirror of the set in `new`.
            unsafe {
                if let Some(v) = self.prev.take() {
                    std::env::set_var(TOKEN_ENV_VAR, v);
                } else {
                    std::env::remove_var(TOKEN_ENV_VAR);
                }
            }
        }
    }

    #[test]
    fn from_env_picks_up_audd_api_token() {
        let _g = EnvGuard::new(Some("env-tok"));
        let audd = AudD::from_env().expect("AUDD_API_TOKEN was set");
        assert_eq!(audd.api_token(), "env-tok");
    }

    #[test]
    fn try_new_falls_back_to_env_when_empty() {
        let _g = EnvGuard::new(Some("env-tok-2"));
        let audd = AudD::try_new("").expect("env fallback should succeed");
        assert_eq!(audd.api_token(), "env-tok-2");
    }

    #[test]
    fn try_new_uses_explicit_over_env() {
        let _g = EnvGuard::new(Some("env-tok-3"));
        let audd = AudD::try_new("explicit").expect("explicit token wins");
        assert_eq!(audd.api_token(), "explicit");
    }

    #[test]
    fn from_env_errors_when_missing() {
        let _g = EnvGuard::new(None);
        let err = AudD::from_env().expect_err("no token => Configuration error");
        match err {
            AudDError::Configuration { message } => {
                assert!(
                    message.contains("dashboard.audd.io"),
                    "expected dashboard URL hint in: {message}"
                );
                assert!(message.contains(TOKEN_ENV_VAR));
            }
            other => panic!("expected Configuration, got {other:?}"),
        }
    }

    // ----- set_api_token thread-safe rotation -----

    #[test]
    fn set_api_token_rotates() {
        let audd = AudD::new("orig");
        assert_eq!(audd.api_token(), "orig");
        audd.set_api_token("new").unwrap();
        assert_eq!(audd.api_token(), "new");
    }

    #[test]
    fn set_api_token_rejects_empty() {
        let audd = AudD::new("orig");
        let err = audd.set_api_token("").unwrap_err();
        match err {
            AudDError::Configuration { message } => {
                assert!(message.to_lowercase().contains("non-empty"));
            }
            other => panic!("expected Configuration, got {other:?}"),
        }
        // Token unchanged on rejection.
        assert_eq!(audd.api_token(), "orig");
    }

    #[test]
    fn set_api_token_concurrent_does_not_panic() {
        // Smoke test: a few threads racing on rotate + read shouldn't deadlock
        // or panic. Final value must be one of the rotations.
        use std::sync::Arc as StdArc;
        use std::thread;
        let audd = StdArc::new(AudD::new("t0"));
        let mut handles = Vec::new();
        for i in 0..8 {
            let a = StdArc::clone(&audd);
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    a.set_api_token(format!("t{i}")).unwrap();
                    let _ = a.api_token();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let final_tok = audd.api_token();
        assert!(
            final_tok.starts_with('t') && final_tok.len() <= 3,
            "unexpected final token: {final_tok}"
        );
    }

    // ----- AudDEvent / on_event hook -----

    #[test]
    fn emit_event_invokes_registered_hook() {
        use std::sync::Mutex;
        let captured: Arc<Mutex<Vec<AudDEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_hook = Arc::clone(&captured);
        let hook: OnEventHook = Arc::new(move |e: &AudDEvent| {
            captured_for_hook.lock().unwrap().push(e.clone());
        });
        let audd = AudD::builder("test").on_event(hook).build().unwrap();
        let ev = AudDEvent {
            kind: EventKind::Request,
            method: "recognize".into(),
            url: "https://api.audd.io/".into(),
            request_id: None,
            http_status: None,
            elapsed: Duration::from_millis(0),
            error_code: None,
            extras: HashMap::new(),
        };
        audd.inner.emit_event(&ev);
        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].method, "recognize");
        assert_eq!(got[0].kind, EventKind::Request);
    }

    #[test]
    fn emit_event_swallows_panic_in_hook() {
        let hook: OnEventHook = Arc::new(|_e: &AudDEvent| panic!("hook panicked"));
        let audd = AudD::builder("test").on_event(hook).build().unwrap();
        let ev = AudDEvent {
            kind: EventKind::Response,
            method: "recognize".into(),
            url: "https://api.audd.io/".into(),
            request_id: Some("req-1".into()),
            http_status: Some(200),
            elapsed: Duration::from_millis(5),
            error_code: None,
            extras: HashMap::new(),
        };
        // Must not panic the test runner.
        audd.inner.emit_event(&ev);
    }

    #[test]
    fn audd_event_no_token_field() {
        // Sanity: AudDEvent has no field that would carry the api_token. Asserting
        // on its Debug rendering ensures we don't accidentally surface secrets via
        // the inspection path.
        let ev = AudDEvent {
            kind: EventKind::Request,
            method: "recognize".into(),
            url: "https://api.audd.io/".into(),
            request_id: None,
            http_status: None,
            elapsed: Duration::from_secs(0),
            error_code: None,
            extras: HashMap::new(),
        };
        let s = format!("{ev:?}");
        assert!(!s.to_lowercase().contains("api_token"));
        assert!(!s.to_lowercase().contains("token"));
    }
}
