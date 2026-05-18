//! Source enum + per-attempt re-opener pattern (locked C1).
//!
//! `reqwest::multipart::Form` parts implement `Clone` for byte buffers but NOT
//! for streams; `reqwest` itself does NOT auto-rewind body streams across
//! attempts. So `prepare_source` returns a *re-opener* — a closure that yields
//! a fresh `Form` (or an empty form + URL field) on every retry attempt.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;

use reqwest::multipart::{Form, Part};
use tokio::io::AsyncRead;

use crate::errors::AudDError;

/// Audio source for [`AudD::recognize`](crate::AudD::recognize) and friends.
///
/// The HTTP layer sends URLs as `data["url"]=...` and bytes/paths/readers as a
/// `file` multipart part.
pub enum Source {
    /// `http://...` / `https://...` URL — sent as a `url` form field.
    Url(String),
    /// Local filesystem path — opened fresh on every retry attempt.
    Path(PathBuf),
    /// Raw bytes — cloned on every retry attempt.
    Bytes(Vec<u8>),
    /// Async reader — buffered into memory on first call (single-attempt safe).
    /// For very large files prefer [`Self::Path`].
    Reader(Box<dyn AsyncRead + Send + Unpin>),
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(u) => f.debug_tuple("Url").field(u).finish(),
            Self::Path(p) => f.debug_tuple("Path").field(p).finish(),
            Self::Bytes(b) => f.debug_tuple("Bytes").field(&b.len()).finish(),
            Self::Reader(_) => f.write_str("Reader(<async>)"),
        }
    }
}

impl From<&str> for Source {
    fn from(s: &str) -> Self {
        if s.starts_with("http://") || s.starts_with("https://") {
            Self::Url(s.to_string())
        } else {
            Self::Path(PathBuf::from(s))
        }
    }
}

impl From<String> for Source {
    fn from(s: String) -> Self {
        if s.starts_with("http://") || s.starts_with("https://") {
            Self::Url(s)
        } else {
            Self::Path(PathBuf::from(s))
        }
    }
}

impl From<PathBuf> for Source {
    fn from(p: PathBuf) -> Self {
        Self::Path(p)
    }
}

impl From<&std::path::Path> for Source {
    fn from(p: &std::path::Path) -> Self {
        Self::Path(p.to_path_buf())
    }
}

impl From<Vec<u8>> for Source {
    fn from(v: Vec<u8>) -> Self {
        Self::Bytes(v)
    }
}

/// What `prepare_source` yields per attempt.
pub(crate) enum Prepared {
    /// URL string to send as a form field.
    Url(String),
    /// Multipart `file` part with bytes payload.
    File { filename: String, bytes: Vec<u8> },
}

impl Prepared {
    /// Apply this prepared body to a [`Form`]. The caller adds `api_token` and
    /// any other fields *before* calling this.
    pub(crate) fn apply(self, form: Form) -> Form {
        match self {
            Self::Url(u) => form.text("url", u),
            Self::File { filename, bytes } => {
                // `mime_str` only fails on a malformed MIME literal, which our
                // hard-coded value cannot be — fall back to the bare part if
                // somehow it ever did.
                let part = match Part::bytes(bytes.clone())
                    .file_name(filename.clone())
                    .mime_str("application/octet-stream")
                {
                    Ok(p) => p,
                    Err(_) => Part::bytes(bytes).file_name(filename),
                };
                form.part("file", part)
            }
        }
    }
}

/// State shared between calls to a re-opener for the [`Source::Reader`] variant.
struct ReaderState {
    /// Buffered bytes after the first read; used on every retry attempt.
    buffered: Mutex<Option<Vec<u8>>>,
    /// The reader itself, taken on first read and never re-used.
    reader: Mutex<Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>>,
}

/// Build a re-opener from a [`Source`]. The returned closure yields a fresh
/// [`Prepared`] on each call.
///
/// # Errors
///
/// Returns [`AudDError::Source`] if a path doesn't exist or a reader is
/// retried after being consumed (we'd otherwise send zero bytes silently).
pub(crate) fn prepare_source(
    source: Source,
) -> Result<
    Box<
        dyn Fn() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Prepared, AudDError>> + Send>,
            > + Send
            + Sync,
    >,
    AudDError,
> {
    match source {
        Source::Url(u) => Ok(Box::new(move || {
            let u = u.clone();
            Box::pin(async move { Ok(Prepared::Url(u)) })
        })),

        Source::Path(p) => {
            // Validate path early so callers learn about typos before the first attempt.
            if !p.exists() {
                return Err(AudDError::Source(format!(
                    "{} is not an existing file path. Pass a URL (http:// or https://), a Path, or bytes.",
                    p.display()
                )));
            }
            let filename = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "upload.bin".to_string());
            Ok(Box::new(move || {
                let p = p.clone();
                let fname = filename.clone();
                Box::pin(async move {
                    let bytes = tokio::fs::read(&p).await.map_err(|e| {
                        AudDError::Source(format!("failed to read {}: {e}", p.display()))
                    })?;
                    Ok(Prepared::File {
                        filename: fname,
                        bytes,
                    })
                })
            }))
        }

        Source::Bytes(b) => Ok(Box::new(move || {
            let bytes = b.clone();
            Box::pin(async move {
                Ok(Prepared::File {
                    filename: "upload.bin".to_string(),
                    bytes,
                })
            })
        })),

        Source::Reader(r) => {
            let state = std::sync::Arc::new(ReaderState {
                buffered: Mutex::new(None),
                reader: Mutex::new(Some(Box::pin(r))),
            });
            Ok(Box::new(move || {
                let state = state.clone();
                Box::pin(async move {
                    {
                        let cached = state
                            .buffered
                            .lock()
                            .map_err(|_| AudDError::Source("source state poisoned".into()))?
                            .clone();
                        if let Some(bytes) = cached {
                            return Ok(Prepared::File {
                                filename: "upload.bin".to_string(),
                                bytes,
                            });
                        }
                    }
                    let mut maybe_reader = state
                        .reader
                        .lock()
                        .map_err(|_| AudDError::Source("source state poisoned".into()))?
                        .take()
                        .ok_or_else(|| {
                            AudDError::Source(
                                "Cannot retry an unbuffered reader — pass bytes (Source::Bytes) \
or a Path/URL instead."
                                    .to_string(),
                            )
                        })?;
                    let mut buf = Vec::new();
                    tokio::io::copy(&mut maybe_reader, &mut buf)
                        .await
                        .map_err(|e| AudDError::Source(format!("failed to read source: {e}")))?;
                    {
                        let mut guard = state
                            .buffered
                            .lock()
                            .map_err(|_| AudDError::Source("source state poisoned".into()))?;
                        *guard = Some(buf.clone());
                    }
                    Ok(Prepared::File {
                        filename: "upload.bin".to_string(),
                        bytes: buf,
                    })
                })
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn url_is_url() {
        let reopen = prepare_source(Source::Url("https://x".into())).unwrap();
        let p = reopen().await.unwrap();
        assert!(matches!(p, Prepared::Url(_)));
    }

    #[tokio::test]
    async fn bytes_clones_each_attempt() {
        let reopen = prepare_source(Source::Bytes(vec![1, 2, 3])).unwrap();
        let p1 = reopen().await.unwrap();
        let p2 = reopen().await.unwrap();
        match (p1, p2) {
            (Prepared::File { bytes: a, .. }, Prepared::File { bytes: b, .. }) => {
                assert_eq!(a, b);
                assert_eq!(a, vec![1, 2, 3]);
            }
            _ => panic!("expected File"),
        }
    }

    #[tokio::test]
    async fn nonexistent_path_errors_early() {
        // Use match instead of unwrap_err — the Ok variant (Box<dyn Fn>) doesn't impl Debug.
        match prepare_source(Source::Path(PathBuf::from("/no/such/file"))) {
            Ok(_) => panic!("expected error"),
            Err(err) => assert!(matches!(err, AudDError::Source(_))),
        }
    }

    #[tokio::test]
    async fn reader_buffers_for_retry() {
        let bytes: &[u8] = b"hello world";
        let cursor = std::io::Cursor::new(bytes.to_vec());
        let reader: Box<dyn AsyncRead + Send + Unpin> = Box::new(cursor);
        let reopen = prepare_source(Source::Reader(reader)).unwrap();
        let p1 = reopen().await.unwrap();
        let p2 = reopen().await.unwrap();
        match (p1, p2) {
            (Prepared::File { bytes: a, .. }, Prepared::File { bytes: b, .. }) => {
                assert_eq!(a, b);
                assert_eq!(a, b"hello world");
            }
            _ => panic!("expected File"),
        }
    }

    #[tokio::test]
    async fn from_str_detects_url_vs_path() {
        let s: Source = "https://example.com/x.mp3".into();
        assert!(matches!(s, Source::Url(_)));
        let s: Source = "/some/path".into();
        assert!(matches!(s, Source::Path(_)));
    }

    #[tokio::test]
    async fn path_reads_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"abc").unwrap();
        let reopen = prepare_source(Source::Path(tmp.path().to_path_buf())).unwrap();
        let p = reopen().await.unwrap();
        match p {
            Prepared::File { bytes, .. } => assert_eq!(bytes, b"abc"),
            Prepared::Url(_) => panic!("expected File"),
        }
    }
}
