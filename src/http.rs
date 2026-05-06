//! HTTP transport. Thin async wrapper around `reqwest::Client`.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::multipart::Form;
use reqwest::Client;
use serde_json::Value;

use crate::errors::AudDError;
use crate::user_agent::user_agent;

const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_READ_TIMEOUT_SECS: u64 = 60;
const ENTERPRISE_READ_TIMEOUT_SECS: u64 = 3600;

/// One HTTP response, normalized for the rest of the SDK.
#[derive(Debug, Clone)]
pub(crate) struct HttpResponse {
    /// Parsed JSON body (`None` when the body wasn't valid JSON).
    pub(crate) json_body: Option<Value>,
    /// HTTP status the server returned.
    pub(crate) http_status: u16,
    /// `x-request-id` header value, if any.
    pub(crate) request_id: Option<String>,
    /// Original response body for diagnostic plumbing.
    pub(crate) raw_text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HttpClient {
    inner: Client,
    /// Shared, mutable api_token. Wrapped so [`AudD::set_api_token`] can rotate
    /// it from `&self` while in-flight requests still see a stable snapshot.
    /// Cloning `HttpClient` clones the `Arc`, so all sibling clients (standard
    /// + enterprise) share one `RwLock` — one `set_api_token` updates them all.
    api_token: Arc<RwLock<String>>,
    /// Whether we own the inner `Client` (and should disregard explicit close
    /// when the caller injected their own).
    owned: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TimeoutProfile {
    Standard,
    Enterprise,
}

impl HttpClient {
    pub(crate) fn new(
        api_token: Arc<RwLock<String>>,
        profile: TimeoutProfile,
    ) -> Result<Self, AudDError> {
        let read_secs = match profile {
            TimeoutProfile::Standard => DEFAULT_READ_TIMEOUT_SECS,
            TimeoutProfile::Enterprise => ENTERPRISE_READ_TIMEOUT_SECS,
        };
        let mut builder = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(read_secs));
        if matches!(profile, TimeoutProfile::Enterprise) {
            builder = builder.tcp_keepalive(Duration::from_secs(30));
        }
        let inner = builder.build().map_err(|e| AudDError::Connection {
            message: format!("failed to construct reqwest::Client: {e}"),
            source: Some(Box::new(e)),
        })?;
        Ok(Self {
            inner,
            api_token,
            owned: true,
        })
    }

    pub(crate) fn from_client(api_token: Arc<RwLock<String>>, client: Client) -> Self {
        Self {
            inner: client,
            api_token,
            owned: false,
        }
    }

    /// Snapshot the current api_token. Acquires a brief read lock per call.
    fn token_snapshot(&self) -> String {
        self.api_token
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub(crate) async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, String)],
        attached: Option<Form>,
        per_call_timeout: Option<Duration>,
    ) -> Result<HttpResponse, AudDError> {
        let mut form = attached.unwrap_or_else(Form::new);
        form = form.text("api_token", self.token_snapshot());
        for (k, v) in fields {
            form = form.text((*k).to_string(), v.clone());
        }
        let mut req = self.inner.post(url).multipart(form);
        if let Some(t) = per_call_timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(map_reqwest_error)?;
        wrap(resp).await
    }

    pub(crate) async fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
        per_call_timeout: Option<Duration>,
    ) -> Result<HttpResponse, AudDError> {
        let mut q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        // Default api_token onto the query if not already there.
        if !q.iter().any(|(k, _)| k == "api_token") {
            q.push(("api_token".to_string(), self.token_snapshot()));
        }
        let mut req = self.inner.get(url).query(&q);
        if let Some(t) = per_call_timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(map_reqwest_error)?;
        wrap(resp).await
    }

    pub(crate) fn close(&self) {
        // reqwest::Client manages its connection pool internally; if we own it we
        // can simply let it drop. There's no explicit close API. We expose this for
        // parity with `Drop` / `AudD::close`.
        let _ = self.owned;
    }
}

async fn wrap(resp: reqwest::Response) -> Result<HttpResponse, AudDError> {
    let http_status = resp.status().as_u16();
    let request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let raw_text = resp.text().await.map_err(map_reqwest_error)?;
    let json_body: Option<Value> = if raw_text.is_empty() {
        None
    } else {
        serde_json::from_str(&raw_text).ok()
    };
    Ok(HttpResponse {
        json_body,
        http_status,
        request_id,
        raw_text,
    })
}

pub(crate) fn map_reqwest_error(e: reqwest::Error) -> AudDError {
    let msg = e.to_string();
    AudDError::Connection {
        message: msg,
        source: Some(Box::new(e)),
    }
}

/// A simple bare HTTP client used by the tokenless `LongpollConsumer` — no
/// auth-token plumbing, just GETs.
#[derive(Debug, Clone)]
pub(crate) struct BareHttpClient {
    pub(crate) inner: Arc<Client>,
    #[allow(dead_code)] // Reserved for explicit-close semantics.
    pub(crate) owned: bool,
}

impl BareHttpClient {
    pub(crate) fn new() -> Result<Self, AudDError> {
        let inner = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| AudDError::Connection {
                message: format!("failed to construct reqwest::Client: {e}"),
                source: Some(Box::new(e)),
            })?;
        Ok(Self {
            inner: Arc::new(inner),
            owned: true,
        })
    }

    pub(crate) fn from_client(client: Client) -> Self {
        Self {
            inner: Arc::new(client),
            owned: false,
        }
    }

    pub(crate) async fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<HttpResponse, AudDError> {
        let q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        let resp = self
            .inner
            .get(url)
            .query(&q)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        wrap(resp).await
    }
}
