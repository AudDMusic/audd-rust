//! Custom-catalog endpoint. **NOT for music recognition** — see [`CustomCatalog::add`].

use crate::client::{decode_or_raise, AudDInner};
use crate::errors::AudDError;
use crate::retry::{retry_async, RetryClass, RetryPolicy};
use crate::source::{prepare_source, Source};

/// `custom_catalog.*` namespace. Reach via [`crate::AudD::custom_catalog`].
pub struct CustomCatalog<'a> {
    inner: &'a AudDInner,
}

impl<'a> CustomCatalog<'a> {
    pub(crate) fn new(inner: &'a AudDInner) -> Self {
        Self { inner }
    }

    /// **This is NOT how you submit audio for music recognition.**
    ///
    /// For music recognition, use [`crate::AudD::recognize`] (or
    /// [`crate::AudD::recognize_enterprise`] for files longer than 25 seconds).
    /// This method adds a song to your **private fingerprint catalog** so
    /// AudD's recognition can later identify *your own* tracks for *your
    /// account only*. Requires special access — contact api@audd.io if you
    /// need it enabled.
    ///
    /// Calling this again with the same `audio_id` re-fingerprints that slot.
    /// There is no public list/delete endpoint; track `audio_id` ↔ song
    /// mappings on your side.
    ///
    /// # Retry policy
    ///
    /// Custom-catalog uploads are metered — every attempt that reaches the
    /// server is billed. To avoid double-charging on transient failures, this
    /// method does **not** retry: a single attempt is made and any error
    /// (transport, 5xx, etc.) surfaces immediately so you can decide how to
    /// recover. This is stricter than the per-client `max_attempts` builder
    /// setting, which is intentionally ignored here.
    ///
    /// # Errors
    ///
    /// Returns [`AudDError`] for transport, server, or parse failures. Code 904
    /// from this endpoint is mapped to [`crate::ErrorKind::CustomCatalogAccess`]
    /// with an override message explaining the special-access requirement.
    pub async fn add(&self, audio_id: i64, source: impl Into<Source>) -> Result<(), AudDError> {
        let reopen = prepare_source(source.into())?;
        let url = format!("{}/upload/", self.inner.api_base);
        let http = self.inner.http.clone();
        // Custom-catalog upload is metered; a transient transport failure
        // could double-charge if we retried. Force a single attempt,
        // overriding the client's `max_attempts` setting.
        let policy = RetryPolicy::new(RetryClass::Mutating).with_max_attempts(1);
        let audio_id_s = audio_id.to_string();

        let resp = retry_async(
            || {
                let reopen = &reopen;
                let url = url.clone();
                let http = http.clone();
                let audio_id_s = audio_id_s.clone();
                async move {
                    let prepared = reopen().await?;
                    let form = prepared.apply(reqwest::multipart::Form::new());
                    http.post_form(&url, &[("audio_id", audio_id_s)], Some(form), None)
                        .await
                }
            },
            policy,
        )
        .await?;
        decode_or_raise(resp, /* custom_catalog_context = */ true)?;
        Ok(())
    }
}
