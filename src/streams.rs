//! Streams namespace — set/get callback URL, addStream/listStreams/setStreamUrl/deleteStream,
//! longpoll with default-on preflight (and `skip_callback_check` opt-out),
//! `derive_longpoll_category`, `parse_callback`.

use std::pin::Pin;

use futures_core::Stream;
use serde_json::Value;

use crate::client::{decode_or_raise, AudDInner};
use crate::errors::{AudDError, ErrorKind};
use crate::helpers::{add_return_to_url, derive_longpoll_category, parse_callback};
use crate::http::HttpClient;
use crate::models::{Stream as StreamRow, StreamCallbackPayload};
use crate::retry::{retry_async, RetryPolicy};

/// Server returns error #19 from `getCallbackUrl` when no callback URL is
/// configured. We treat this specifically as the "no-callback-set" signal.
const NO_CALLBACK_ERROR_CODE: i32 = 19;

const PREFLIGHT_NO_CALLBACK_HINT: &str =
    "Longpoll won't deliver events because no callback URL is configured for this account. \
Set one first via streams.set_callback_url(...) — `https://audd.tech/empty/` is fine if \
you only want longpolling and don't need a real receiver. \
To skip this check, pass skip_callback_check=true.";

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

    /// Parse a callback POST body into a typed [`StreamCallbackPayload`].
    ///
    /// # Errors
    ///
    /// Returns [`AudDError::Serialization`] if the body doesn't deserialize.
    pub fn parse_callback(&self, body: Value) -> Result<StreamCallbackPayload, AudDError> {
        parse_callback(body)
    }

    /// Long-poll the AudD streams endpoint and yield events as a [`Stream`].
    ///
    /// On entry, performs a one-time `getCallbackUrl` preflight unless
    /// `opts.skip_callback_check == true`. If the server returns error #19
    /// (no callback URL configured), [`AudDError::Api`] is returned with kind
    /// [`ErrorKind::InvalidRequest`] explaining how to fix it.
    ///
    /// # Errors
    ///
    /// The returned stream yields [`AudDError`] on transport, server, or parse
    /// failures.
    pub async fn longpoll(
        &self,
        category: &str,
        opts: LongpollOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Value, AudDError>> + Send>>, AudDError> {
        if !opts.skip_callback_check {
            self.preflight_callback().await?;
        }
        Ok(longpoll_stream(
            self.inner.http.clone(),
            format!("{}/longpoll/", self.inner.api_base),
            category.to_string(),
            opts,
            self.inner.read_policy(),
        ))
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

fn longpoll_stream(
    http: HttpClient,
    url: String,
    category: String,
    opts: LongpollOptions,
    policy: RetryPolicy,
) -> Pin<Box<dyn Stream<Item = Result<Value, AudDError>> + Send>> {
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
                        http.get(&url, &params, None).await
                    }
                },
                policy,
            )
            .await?;
            let body = resp.json_body.ok_or_else(|| AudDError::Serialization {
                message: "longpoll response was not JSON".into(),
                raw_text: resp.raw_text.clone(),
            })?;
            let body_obj = body.as_object().ok_or_else(|| AudDError::Serialization {
                message: "longpoll response was not a JSON object".into(),
                raw_text: resp.raw_text.clone(),
            })?;
            if let Some(ts) = body_obj.get("timestamp").and_then(Value::as_i64) {
                cur_since = Some(ts);
            }
            yield body;
        }
    };
    Box::pin(stream)
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
}
