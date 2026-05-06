//! `advanced.*` namespace — lyrics search and a generic raw-request escape hatch.

use serde_json::Value;

use crate::client::{decode_or_raise, AudDInner};
use crate::errors::{raise_from_error_response, AudDError};
use crate::http::HttpResponse;
use crate::models::LyricsResult;
use crate::retry::retry_async;

/// `advanced.*` namespace. Reach via [`crate::AudD::advanced`].
///
/// Uses the `Recognition` retry class: `find_lyrics` is metered and shouldn't
/// double-bill on a post-upload read timeout.
pub struct Advanced<'a> {
    inner: &'a AudDInner,
}

impl<'a> Advanced<'a> {
    pub(crate) fn new(inner: &'a AudDInner) -> Self {
        Self { inner }
    }

    /// Search the AudD lyrics index. Returns up to `limit` matches (server-side default 10).
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server/parse failures.
    pub async fn find_lyrics(&self, query: &str) -> Result<Vec<LyricsResult>, AudDError> {
        let body = self
            .raw_request("findLyrics", &[("q", query.to_string())])
            .await?;
        if body.get("status").and_then(Value::as_str) == Some("error") {
            return Err(raise_from_error_response(&body, 200, None, false));
        }
        let result = body.get("result").cloned().unwrap_or(Value::Null);
        if result.is_null() {
            return Ok(Vec::new());
        }
        let v: Vec<LyricsResult> =
            serde_json::from_value(result.clone()).map_err(|e| AudDError::Serialization {
                message: format!("could not parse findLyrics result: {e}"),
                raw_text: result.to_string(),
            })?;
        Ok(v)
    }

    /// Hit any AudD endpoint by method name and return the raw JSON body.
    ///
    /// Useful for endpoints not yet wrapped by typed methods on this SDK.
    /// Performs the same auth + retry + error-mapping as the typed methods.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server/parse failures.
    pub async fn raw_request(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<Value, AudDError> {
        let url = format!("{}/{method}/", self.inner.api_base);
        let http = self.inner.http.clone();
        let fields: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.clone())).collect();
        let resp: HttpResponse = retry_async(
            || {
                let http = http.clone();
                let url = url.clone();
                let fields = fields.clone();
                async move { http.post_form(&url, &fields, None, None).await }
            },
            self.inner.recognition_policy(),
        )
        .await?;
        // For raw_request we still distinguish HTTP-vs-JSON; but we don't unwrap to result,
        // we hand the body back to the caller (or let decode_or_raise map an error).
        let HttpResponse {
            json_body,
            http_status,
            request_id,
            raw_text,
        } = resp;
        if let Some(body) = json_body {
            return Ok(body);
        }
        if http_status >= 400 {
            return Err(AudDError::Server {
                http_status,
                message: format!("HTTP {http_status} with non-JSON response body"),
                request_id,
                raw_response: raw_text,
            });
        }
        Err(AudDError::Serialization {
            message: "Unparseable response".into(),
            raw_text,
        })
    }

    /// Hit any AudD endpoint by method name with full error decoding (i.e. raises on
    /// `status=error`). The non-`_strict` [`Self::raw_request`] returns the raw body
    /// even on `status=error` for callers that want to inspect it.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport/server/parse failures or AudD errors.
    pub async fn raw_request_strict(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<Value, AudDError> {
        let body = self.raw_request(method, params).await?;
        let resp = HttpResponse {
            json_body: Some(body),
            http_status: 200,
            request_id: None,
            raw_text: String::new(),
        };
        decode_or_raise(resp, false)
    }
}
