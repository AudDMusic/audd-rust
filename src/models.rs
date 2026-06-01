//! Typed models with `#[serde(flatten)] extras` on every type so unknown server
//! fields round-trip without an SDK release.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// Streaming providers reachable via the `lis.tn` redirect helper.
///
/// Use with [`RecognitionResult::streaming_url`] /
/// [`RecognitionResult::streaming_urls`] / [`EnterpriseMatch::streaming_url`]
/// /  [`EnterpriseMatch::streaming_urls`] to produce a direct or redirect URL
/// pointing the listener at the provider's page for the matched track.
///
/// Serializes to its wire-name (`"spotify"`, `"apple_music"`, …) for
/// round-trip into logs / queues. Deserializes from the same wire-names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamingProvider {
    /// Spotify.
    #[serde(rename = "spotify")]
    Spotify,
    /// Apple Music.
    #[serde(rename = "apple_music")]
    AppleMusic,
    /// Deezer.
    #[serde(rename = "deezer")]
    Deezer,
    /// Napster.
    #[serde(rename = "napster")]
    Napster,
    /// YouTube. Only the `lis.tn` redirect path applies — there is no YouTube
    /// metadata block to pull a direct URL from.
    #[serde(rename = "youtube")]
    YouTube,
}

impl StreamingProvider {
    /// Wire-name for the provider, matching the `lis.tn?<provider>` query
    /// parameter and the `return=` field on [`crate::AudD::recognize`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spotify => "spotify",
            Self::AppleMusic => "apple_music",
            Self::Deezer => "deezer",
            Self::Napster => "napster",
            Self::YouTube => "youtube",
        }
    }
}

impl std::fmt::Display for StreamingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Iteration order for [`RecognitionResult::streaming_urls`] /
/// [`EnterpriseMatch::streaming_urls`]. Matches the audd-go and audd-python
/// reference order.
const ALL_STREAMING_PROVIDERS: [StreamingProvider; 5] = [
    StreamingProvider::Spotify,
    StreamingProvider::AppleMusic,
    StreamingProvider::Deezer,
    StreamingProvider::Napster,
    StreamingProvider::YouTube,
];

/// Build `"<song_link>?<provider>"` only when `song_link.host_str() == "lis.tn"`.
///
/// Returns `None` for non-`lis.tn` hosts (e.g. YouTube song-links) and when
/// `song_link` is absent. `lis.tn` 302-redirects each provider query to the
/// matched track's page on that provider.
fn lis_tn_streaming_url(song_link: Option<&str>, provider: &str) -> Option<String> {
    let link = song_link?;
    let parsed = Url::parse(link).ok()?;
    if parsed.host_str() != Some("lis.tn") {
        return None;
    }
    let sep = if parsed.query().is_some() { '&' } else { '?' };
    Some(format!("{link}{sep}{provider}"))
}

/// Apple Music metadata returned by the `apple_music` block.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppleMusicMetadata {
    /// Track title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Artist name as Apple labels it.
    #[serde(
        default,
        rename = "artistName",
        skip_serializing_if = "Option::is_none"
    )]
    pub artist_name: Option<String>,
    /// Album name as Apple labels it.
    #[serde(default, rename = "albumName", skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Apple Music URL for the track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Track length in milliseconds.
    #[serde(
        default,
        rename = "durationInMillis",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_in_millis: Option<i64>,
    /// International Standard Recording Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// Track number on the disc.
    #[serde(
        default,
        rename = "trackNumber",
        skip_serializing_if = "Option::is_none"
    )]
    pub track_number: Option<i32>,
    /// Composer credits.
    #[serde(
        default,
        rename = "composerName",
        skip_serializing_if = "Option::is_none"
    )]
    pub composer_name: Option<String>,
    /// Disc number on a multi-disc release.
    #[serde(
        default,
        rename = "discNumber",
        skip_serializing_if = "Option::is_none"
    )]
    pub disc_number: Option<i32>,
    /// Apple's release date for the track.
    #[serde(
        default,
        rename = "releaseDate",
        skip_serializing_if = "Option::is_none"
    )]
    pub release_date: Option<String>,
    /// Forward-compat: any unknown fields the server returned.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// Spotify metadata returned by the `spotify` block.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SpotifyMetadata {
    /// Spotify track ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Track name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Track length in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Whether the track is flagged explicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    /// Spotify popularity score (0–100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popularity: Option<i32>,
    /// Track number on the disc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<i32>,
    /// Spotify object type (typically `track`).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub object_type: Option<String>,
    /// Spotify URI (e.g., `spotify:track:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Forward-compat: any unknown fields.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// Deezer metadata returned by the `deezer` block.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DeezerMetadata {
    /// Deezer track ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Track title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Track length in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Web link to the track on Deezer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// Napster metadata returned by the `napster` block.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NapsterMetadata {
    /// Napster track ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Track name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// ISRC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// Artist name.
    #[serde(
        default,
        rename = "artistName",
        skip_serializing_if = "Option::is_none"
    )]
    pub artist_name: Option<String>,
    /// Album name.
    #[serde(default, rename = "albumName", skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// MusicBrainz match entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MusicBrainzEntry {
    /// MusicBrainz recording ID.
    #[serde(default)]
    pub id: String,
    /// Match score, often a number but sometimes serialized as a string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<Value>,
    /// Recording title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Recording length in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<i64>,
    /// Forward-compat: any unknown fields.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// A recognition result.
///
/// `timecode` is always present on a match. All other typed fields are optional.
/// Public-catalog matches set `artist`/`title`/etc.; custom-catalog matches set
/// `audio_id` only. Use [`Self::is_custom_match`] / [`Self::is_public_match`]
/// to discriminate, or [`RecognitionMatch::from`] for a sealed sum view.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RecognitionResult {
    /// Position in the source where the match starts (e.g., `"00:56"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<String>,
    /// Set on custom-catalog matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_id: Option<i64>,
    /// Artist name on a public-catalog match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Track title on a public-catalog match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Album name on a public-catalog match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Track release date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Record label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// AudD-hosted song-link URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_link: Option<String>,
    /// ISRC (International Standard Recording Code). Available on Startup plan or higher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// UPC (Universal Product Code) of the release. Available on Startup plan or higher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upc: Option<String>,
    /// Apple Music metadata if requested via `return=apple_music`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_music: Option<AppleMusicMetadata>,
    /// Spotify metadata if requested via `return=spotify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotify: Option<SpotifyMetadata>,
    /// Deezer metadata if requested via `return=deezer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deezer: Option<DeezerMetadata>,
    /// Napster metadata if requested via `return=napster`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub napster: Option<NapsterMetadata>,
    /// MusicBrainz matches if requested via `return=musicbrainz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz: Option<Vec<MusicBrainzEntry>>,
    /// Forward-compat: any unknown fields the server returned.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

impl RecognitionResult {
    /// `true` when `audio_id` is set (custom-catalog match).
    #[must_use]
    pub fn is_custom_match(&self) -> bool {
        self.audio_id.is_some()
    }

    /// `true` when `artist` or `title` is set (public-catalog match).
    #[must_use]
    pub fn is_public_match(&self) -> bool {
        self.audio_id.is_none() && (self.artist.is_some() || self.title.is_some())
    }

    /// Cover-art URL for `lis.tn`-hosted song-links, else `None`.
    ///
    /// AudD's `?thumb` image endpoint exists only on `lis.tn` song-links;
    /// YouTube and other hosts return `None`.
    #[must_use]
    pub fn thumbnail_url(&self) -> Option<String> {
        lis_tn_streaming_url(self.song_link.as_deref(), "thumb")
    }

    /// Direct or redirect URL for a streaming provider, with smart fallback.
    ///
    /// Resolution order:
    ///
    /// 1. **Direct URL from the metadata block** when the user requested that
    ///    provider via `return=` (e.g. `apple_music.url`,
    ///    `spotify.external_urls.spotify` / `spotify.uri`, `deezer.link`,
    ///    `napster.href`). Direct = no redirect, faster for clients.
    /// 2. **`lis.tn` redirect** `<song_link>?<provider>` when `song_link` is a
    ///    `lis.tn` URL. Works regardless of whether `return=` was set.
    /// 3. `None` when neither path resolves (e.g. YouTube `song_link` and the
    ///    user didn't request the provider's metadata block).
    ///
    /// [`StreamingProvider::YouTube`] only resolves through the `lis.tn`
    /// redirect path — there's no YouTube metadata block.
    #[must_use]
    pub fn streaming_url(&self, provider: StreamingProvider) -> Option<String> {
        if let Some(direct) = self.direct_streaming_url(provider) {
            return Some(direct);
        }
        lis_tn_streaming_url(self.song_link.as_deref(), provider.as_str())
    }

    /// Pull a direct URL out of the corresponding metadata block, if present.
    /// Reads from `extras` (forward-compat) when the typed field is absent or
    /// empty — covers e.g. `spotify.external_urls.spotify`, which we don't
    /// type because the server's shape varies.
    fn direct_streaming_url(&self, provider: StreamingProvider) -> Option<String> {
        match provider {
            StreamingProvider::AppleMusic => self
                .apple_music
                .as_ref()
                .and_then(|am| non_empty(am.url.as_deref())),
            StreamingProvider::Spotify => {
                if let Some(sp) = self.spotify.as_ref() {
                    if let Some(u) = sp
                        .extras
                        .get("external_urls")
                        .and_then(|v| v.get("spotify"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        return Some(u.to_string());
                    }
                    if let Some(u) = non_empty(sp.uri.as_deref()) {
                        return Some(u);
                    }
                }
                None
            }
            StreamingProvider::Deezer => self
                .deezer
                .as_ref()
                .and_then(|d| non_empty(d.link.as_deref())),
            StreamingProvider::Napster => self.napster.as_ref().and_then(|n| {
                n.extras
                    .get("href")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }),
            // YouTube: no metadata block; lis.tn redirect is the only path.
            StreamingProvider::YouTube => None,
        }
    }

    /// Map of every provider with a resolvable URL — direct or via `lis.tn`
    /// redirect. Empty when neither path resolves for any provider.
    ///
    /// Iteration order matches the audd-go / audd-python reference:
    /// Spotify, Apple Music, Deezer, Napster, YouTube.
    #[must_use]
    pub fn streaming_urls(&self) -> HashMap<StreamingProvider, String> {
        let mut out = HashMap::new();
        for p in ALL_STREAMING_PROVIDERS {
            if let Some(u) = self.streaming_url(p) {
                out.insert(p, u);
            }
        }
        out
    }

    /// First available 30-second audio preview URL, in priority order.
    ///
    /// Picks the first non-empty URL from `apple_music.previews[0].url`, then
    /// `spotify.preview_url`, then `deezer.preview`. Returns `None` if no
    /// metadata block carries a preview.
    ///
    /// **Note:** previews are governed by their respective providers' terms
    /// of use (Apple Music, Spotify, Deezer). The SDK consumer is responsible
    /// for honoring those terms — including caching restrictions, attribution
    /// requirements, and any redistribution constraints.
    #[must_use]
    pub fn preview_url(&self) -> Option<String> {
        // Apple Music: previews is a list of {"url": "..."} entries — kept in
        // extras since AudD's `apple_music` block ships it without our typing.
        if let Some(am) = self.apple_music.as_ref() {
            if let Some(u) = am
                .extras
                .get("previews")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|first| first.get("url"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(u.to_string());
            }
        }
        // Spotify: preview_url field directly (in extras since untyped).
        if let Some(sp) = self.spotify.as_ref() {
            if let Some(u) = sp
                .extras
                .get("preview_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(u.to_string());
            }
        }
        // Deezer: preview field directly (in extras since untyped).
        if let Some(dz) = self.deezer.as_ref() {
            if let Some(u) = dz
                .extras
                .get("preview")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(u.to_string());
            }
        }
        None
    }
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.filter(|v| !v.is_empty()).map(String::from)
}

/// Sealed-style view over [`RecognitionResult`] for callers who want
/// exhaustive `match`.
///
/// Serializes as a serde-tagged enum (`{"kind": "public", "result": …}` /
/// `{"kind": "custom", "result": …}`) so callers can write the discriminator
/// straight to a log/queue and pattern-match on it later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum RecognitionMatch {
    /// AudD public-catalog match. Carries the canonical struct.
    Public(RecognitionResult),
    /// Custom-catalog match. Carries the canonical struct.
    Custom(RecognitionResult),
}

impl From<RecognitionResult> for RecognitionMatch {
    fn from(r: RecognitionResult) -> Self {
        if r.is_custom_match() {
            Self::Custom(r)
        } else {
            Self::Public(r)
        }
    }
}

impl RecognitionMatch {
    /// Borrow the underlying [`RecognitionResult`].
    #[must_use]
    pub fn result(&self) -> &RecognitionResult {
        match self {
            Self::Public(r) | Self::Custom(r) => r,
        }
    }

    /// Consume the wrapper, returning the underlying [`RecognitionResult`].
    #[must_use]
    pub fn into_result(self) -> RecognitionResult {
        match self {
            Self::Public(r) | Self::Custom(r) => r,
        }
    }
}

/// One match in an enterprise-recognition response.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct EnterpriseMatch {
    /// Match score (0–100). Absent on some enterprise matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    /// Position in the source where the match starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<String>,
    /// Artist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Release date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// ISRC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// UPC of the album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upc: Option<String>,
    /// AudD-hosted song-link URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_link: Option<String>,
    /// Raw fragment-relative start offset, in milliseconds within the 12s
    /// fragment this match was found in. Use [`Self::start_seconds`] for the
    /// position in the user's file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_offset: Option<i64>,
    /// Raw fragment-relative end offset, in milliseconds within the 12s
    /// fragment. Use [`Self::end_seconds`] for the position in the user's file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_offset: Option<i64>,
    /// Where the match starts in the user's file, in seconds: the chunk's
    /// `offset` plus the fragment-relative `start_offset`. Computed by the SDK
    /// (not a wire field). `None` when the chunk carries no parseable offset.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<f64>,
    /// Where the match ends in the user's file, in seconds: the chunk's
    /// `offset` plus the fragment-relative `end_offset`. Computed by the SDK
    /// (not a wire field). `None` when the chunk carries no parseable offset.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub end_seconds: Option<f64>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// Parse a chunk `offset` string into seconds.
///
/// Accepts `"SS"`, `"MM:SS"`, `"HH:MM:SS"`, or a bare number. Returns `None`
/// for any unparseable shape. Never panics.
fn offset_to_seconds(o: Option<&str>) -> Option<f64> {
    let s = o?.trim();
    if s.is_empty() {
        return None;
    }
    // Bare number (e.g. "60" or "60.5"): parse directly.
    if !s.contains(':') {
        return s.parse::<f64>().ok();
    }
    // Colon-separated: SS, MM:SS, or HH:MM:SS.
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for p in &parts {
        let n: f64 = p.trim().parse().ok()?;
        total = total * 60.0 + n;
    }
    Some(total)
}

impl EnterpriseMatch {
    /// Cover-art URL for `lis.tn`-hosted song-links, else `None`.
    #[must_use]
    pub fn thumbnail_url(&self) -> Option<String> {
        lis_tn_streaming_url(self.song_link.as_deref(), "thumb")
    }

    /// `lis.tn`-redirect URL for a streaming provider, or `None` when
    /// `song_link` isn't a `lis.tn` URL. `EnterpriseMatch` doesn't carry the
    /// per-provider metadata blocks, so only the redirect path applies.
    /// Mirrors the behaviour of [`RecognitionResult::streaming_url`] minus
    /// the direct-URL fallback.
    #[must_use]
    pub fn streaming_url(&self, provider: StreamingProvider) -> Option<String> {
        lis_tn_streaming_url(self.song_link.as_deref(), provider.as_str())
    }

    /// All providers' redirect URLs — see [`Self::streaming_url`]. Empty when
    /// `song_link` isn't a `lis.tn` URL.
    #[must_use]
    pub fn streaming_urls(&self) -> HashMap<StreamingProvider, String> {
        let mut out = HashMap::new();
        for p in ALL_STREAMING_PROVIDERS {
            if let Some(u) = self.streaming_url(p) {
                out.insert(p, u);
            }
        }
        out
    }
}

/// One chunk from `recognize_enterprise` (enterprise responses come in chunks).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct EnterpriseChunkResult {
    /// Songs matched in this chunk.
    #[serde(default)]
    pub songs: Vec<EnterpriseMatch>,
    /// Offset of this chunk in the source (e.g., `"00:00"`). This is the
    /// chunk's position in the user's file — the anchor for each song's
    /// file-relative seconds.
    #[serde(default)]
    pub offset: String,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

impl EnterpriseChunkResult {
    /// Stamp each song's [`EnterpriseMatch::start_seconds`] /
    /// [`EnterpriseMatch::end_seconds`] from this chunk's `offset` (the
    /// chunk's position in the user's file) plus the song's fragment-relative
    /// `start_offset` / `end_offset` (milliseconds). Leaves them `None` when
    /// the chunk's offset is absent or unparseable.
    pub(crate) fn anchor_song_offsets(&mut self) {
        let Some(base) = offset_to_seconds(Some(self.offset.as_str())) else {
            return;
        };
        // ms → seconds. Offsets are small integers; the f64 cast is exact for
        // every realistic value, so the precision-loss lint doesn't apply.
        #[allow(clippy::cast_precision_loss)]
        for s in &mut self.songs {
            s.start_seconds = Some(base + s.start_offset.unwrap_or(0) as f64 / 1000.0);
            s.end_seconds = Some(base + s.end_offset.unwrap_or(0) as f64 / 1000.0);
        }
    }
}

/// One row in the streams list.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Stream {
    /// Caller-chosen integer ID for the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_id: Option<i64>,
    /// Source URL the stream is reading from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether AudD is currently consuming and recognizing the stream.
    #[serde(default)]
    pub stream_running: bool,
    /// Server-generated longpoll category for sharing with browser/widget consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longpoll_category: Option<String>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// One candidate song in a stream-recognition match.
///
/// Almost every match has exactly one song; multiple candidates only appear
/// when the same fingerprint resolves to several near-identical catalog
/// records (e.g. variant releases). When alternatives are present they may
/// have a different artist or title from the top song — see
/// [`StreamCallbackMatch::alternatives`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StreamCallbackSong {
    /// Artist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Match score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    /// Album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Release date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Record label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// AudD-hosted song-link URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_link: Option<String>,
    /// ISRC. Available on Startup plan or higher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// UPC. Available on Startup plan or higher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upc: Option<String>,
    /// Apple Music metadata if requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_music: Option<AppleMusicMetadata>,
    /// Spotify metadata if requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotify: Option<SpotifyMetadata>,
    /// Deezer metadata if requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deezer: Option<DeezerMetadata>,
    /// Napster metadata if requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub napster: Option<NapsterMetadata>,
    /// MusicBrainz matches if requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz: Option<Vec<MusicBrainzEntry>>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

/// One recognition event from a stream callback or longpoll envelope.
///
/// Carries the top match in [`Self::song`]; rare extra candidates live in
/// [`Self::alternatives`]. Alternatives may have a different artist or title
/// from the top song — variant catalog releases or near-duplicates.
///
/// Deserializes from the wire shape `{"radio_id": ..., "timestamp": ...,
/// "play_length": ..., "results": [<song>, <alt>, <alt>, ...]}`. The first
/// `results[]` entry becomes [`Self::song`], the remainder land in
/// [`Self::alternatives`]. Round-trips through [`Serialize`] back into the same
/// wire shape.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamCallbackMatch {
    /// Stream the callback is for.
    pub radio_id: Option<i64>,
    /// Wall-clock timestamp the recognition fired at.
    pub timestamp: Option<String>,
    /// Length of the recognized segment, in seconds.
    pub play_length: Option<i64>,
    /// Top match. Always present.
    pub song: StreamCallbackSong,
    /// Additional candidate matches, when the fingerprint resolves to
    /// multiple near-identical catalog records. Entries here may have a
    /// different artist or title from the top [`Self::song`] — variant
    /// releases or near-duplicates.
    pub alternatives: Vec<StreamCallbackSong>,
    /// Forward-compat: any unknown keys on the recognition object the server
    /// returned. `radio_id` / `timestamp` / `play_length` / `results` are
    /// pulled out as typed fields; everything else lands here.
    pub extras: HashMap<String, Value>,
    /// Original parsed JSON body (the entire callback envelope, not just the
    /// `result` block) — preserved so consumers can inspect untyped fields.
    pub raw_response: Value,
}

/// `notification` payload of a stream-management callback.
///
/// The outer envelope's `time` field (sibling of `notification`) is hoisted
/// onto this type's [`Self::time`] for convenience.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StreamCallbackNotification {
    /// Stream the notification is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_id: Option<i64>,
    /// Whether the stream is currently running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_running: Option<bool>,
    /// Numeric notification code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_code: Option<i32>,
    /// Human-readable notification text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_message: Option<String>,
    /// Outer-envelope `time` field (server-emitted unix-seconds), if present.
    /// Sibling of `notification` in the wire payload — hoisted here for
    /// convenience. Skipped on serialize when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    /// Forward-compat: unknown keys on the `notification` object.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
    /// Original parsed JSON body (the entire callback envelope), preserved
    /// for advanced inspection. Skipped on serialize when `Null`.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub raw_response: Value,
}

// ------ StreamCallbackMatch (de)serialize ------

impl<'de> Deserialize<'de> for StreamCallbackMatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Read the entire `result` object as an arbitrary map, split off the
        // typed fields, and route the rest into `extras`.
        let mut map: serde_json::Map<String, Value> = Deserialize::deserialize(deserializer)?;

        // A successful response must never fail to parse on a missing or
        // wrong-typed field. `radio_id` / `timestamp` / `play_length` decode
        // leniently: absent, null, or an unexpected type all yield `None`
        // rather than a hard error.
        let radio_id = match map.remove("radio_id") {
            Some(v) => serde_json::from_value(v).ok(),
            None => None,
        };
        let timestamp = match map.remove("timestamp") {
            Some(Value::Null) | None => None,
            Some(v) => serde_json::from_value(v).ok(),
        };
        let play_length = match map.remove("play_length") {
            Some(Value::Null) | None => None,
            Some(v) => serde_json::from_value(v).ok(),
        };
        let results: Vec<StreamCallbackSong> = match map.remove("results") {
            // A missing or non-array `results` deserializes to an empty Vec
            // rather than erroring.
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Vec::new(),
        };
        let mut iter = results.into_iter();
        // Empty `results` is tolerated: `song` defaults to an empty
        // `StreamCallbackSong` (all fields optional) instead of erroring.
        let song = iter.next().unwrap_or_default();
        let alternatives = iter.collect();

        // Remaining keys are extras.
        let extras: HashMap<String, Value> = map.into_iter().collect();
        Ok(Self {
            radio_id,
            timestamp,
            play_length,
            song,
            alternatives,
            extras,
            raw_response: Value::Null,
        })
    }
}

impl Serialize for StreamCallbackMatch {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut len = 1; // results
        if self.timestamp.is_some() {
            len += 1;
        }
        if self.play_length.is_some() {
            len += 1;
        }
        if self.radio_id.is_some() {
            len += 1;
        }
        len += self.extras.len();
        let mut map = serializer.serialize_map(Some(len))?;
        if let Some(r) = &self.radio_id {
            map.serialize_entry("radio_id", r)?;
        }
        if let Some(t) = &self.timestamp {
            map.serialize_entry("timestamp", t)?;
        }
        if let Some(p) = &self.play_length {
            map.serialize_entry("play_length", p)?;
        }
        // Reconstruct results = [song, ...alternatives].
        let mut results: Vec<&StreamCallbackSong> = Vec::with_capacity(1 + self.alternatives.len());
        results.push(&self.song);
        for a in &self.alternatives {
            results.push(a);
        }
        map.serialize_entry("results", &results)?;
        for (k, v) in &self.extras {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// Discriminated payload yielded by [`crate::parse_callback`] /
/// [`crate::handle_callback`]. A single callback is either a recognition match
/// or a lifecycle notification — never both.
///
/// `Match` is intentionally larger than `Notification` (a recognition payload
/// carries the full per-track metadata blocks); we keep both variants
/// unboxed so consumers can `match` and bind the inner value without an extra
/// dereference.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CallbackEvent {
    /// Recognition match.
    Match(StreamCallbackMatch),
    /// Stream lifecycle notification (e.g. `stream stopped`, `can't connect`).
    Notification(StreamCallbackNotification),
}

impl CallbackEvent {
    /// Borrow the inner match, if this is a [`Self::Match`].
    #[must_use]
    pub fn as_match(&self) -> Option<&StreamCallbackMatch> {
        if let Self::Match(m) = self {
            Some(m)
        } else {
            None
        }
    }

    /// Borrow the inner notification, if this is a [`Self::Notification`].
    #[must_use]
    pub fn as_notification(&self) -> Option<&StreamCallbackNotification> {
        if let Self::Notification(n) = self {
            Some(n)
        } else {
            None
        }
    }
}

/// One result from `findLyrics`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LyricsResult {
    /// Artist name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Song title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Lyrics text, if AudD has them indexed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    /// Internal song identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_id: Option<i64>,
    /// Embed/media URL when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    /// Server-rendered "Artist – Title" string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_title: Option<String>,
    /// Internal artist identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<i64>,
    /// AudD-hosted song-link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_link: Option<String>,
    /// Forward-compat.
    #[serde(flatten)]
    pub extras: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognition_basic() {
        let v = json!({
            "artist": "Tears For Fears",
            "title": "Everybody Wants To Rule The World",
            "timecode": "00:56",
            "song_link": "https://lis.tn/NbkVb"
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert!(r.is_public_match());
        assert!(!r.is_custom_match());
        assert_eq!(
            r.thumbnail_url().as_deref(),
            Some("https://lis.tn/NbkVb?thumb")
        );
    }

    #[test]
    fn custom_match() {
        let v = json!({"timecode": "01:45", "audio_id": 146});
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert!(r.is_custom_match());
        assert!(!r.is_public_match());
        assert_eq!(r.thumbnail_url(), None);
    }

    #[test]
    fn match_enum() {
        let r = RecognitionResult {
            timecode: Some("x".into()),
            audio_id: Some(1),
            ..Default::default()
        };
        match RecognitionMatch::from(r) {
            RecognitionMatch::Custom(_) => {}
            RecognitionMatch::Public(_) => panic!("expected Custom"),
        }
    }

    #[test]
    fn extras_round_trip() {
        let v = json!({
            "timecode": "00:01",
            "artist": "X",
            "tidal": {"id": "abc"}
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.extras.get("tidal").unwrap().get("id").unwrap(), "abc");
    }

    #[test]
    fn youtube_link_no_thumb() {
        let r = RecognitionResult {
            timecode: Some("00:01".into()),
            song_link: Some("https://www.youtube.com/watch?v=abc".into()),
            ..Default::default()
        };
        assert_eq!(r.thumbnail_url(), None);
    }

    #[test]
    fn thumb_with_existing_query() {
        let r = RecognitionResult {
            timecode: Some("00:01".into()),
            song_link: Some("https://lis.tn/abc?utm=x".into()),
            ..Default::default()
        };
        assert_eq!(
            r.thumbnail_url().as_deref(),
            Some("https://lis.tn/abc?utm=x&thumb")
        );
    }

    #[test]
    fn stream_callback_match_deserialize_splits_song_and_alternatives() {
        let v = json!({
            "radio_id": 7,
            "timestamp": "2020-04-13 10:31:43",
            "play_length": 111,
            "results": [
                {"artist": "X", "title": "Y", "score": 100},
                {"artist": "X", "title": "Y (Remix)", "score": 95}
            ]
        });
        let m: StreamCallbackMatch = serde_json::from_value(v).unwrap();
        assert_eq!(m.radio_id, Some(7));
        assert_eq!(m.timestamp.as_deref(), Some("2020-04-13 10:31:43"));
        assert_eq!(m.play_length, Some(111));
        assert_eq!(m.song.title.as_deref(), Some("Y"));
        assert_eq!(m.alternatives.len(), 1);
        assert_eq!(m.alternatives[0].title.as_deref(), Some("Y (Remix)"));
    }

    #[test]
    fn stream_callback_match_extras_capture_unknown_keys() {
        let v = json!({
            "radio_id": 1,
            "results": [{"artist": "X", "title": "Y", "score": 100}],
            "futuristic_field": {"a": 1}
        });
        let m: StreamCallbackMatch = serde_json::from_value(v).unwrap();
        assert_eq!(
            m.extras
                .get("futuristic_field")
                .and_then(|v| v.get("a"))
                .and_then(Value::as_i64),
            Some(1)
        );
    }

    #[test]
    fn stream_callback_match_empty_results_parses_to_default_song() {
        // A successful callback with an empty `results` array must never error;
        // `song` defaults to an empty StreamCallbackSong and there are no
        // alternatives.
        let v = json!({"radio_id": 1, "results": []});
        let m: StreamCallbackMatch = serde_json::from_value(v).unwrap();
        assert_eq!(m.radio_id, Some(1));
        assert_eq!(m.song, StreamCallbackSong::default());
        assert!(m.alternatives.is_empty());
    }

    #[test]
    fn stream_callback_match_round_trip_serialize() {
        let v = json!({
            "radio_id": 9,
            "timestamp": "2026-05-04 10:31:43",
            "play_length": 60,
            "results": [
                {"artist": "X", "title": "Y", "score": 100, "song_link": "https://lis.tn/abc"}
            ]
        });
        let original: StreamCallbackMatch = serde_json::from_value(v).unwrap();
        let bytes = serde_json::to_vec(&original).unwrap();
        let back: StreamCallbackMatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.radio_id, Some(9));
        assert_eq!(back.song.title.as_deref(), Some("Y"));
        assert_eq!(back.song.song_link.as_deref(), Some("https://lis.tn/abc"));
    }

    #[test]
    fn stream_callback_notification_round_trip() {
        let n = StreamCallbackNotification {
            radio_id: Some(3),
            stream_running: Some(false),
            notification_code: Some(650),
            notification_message: Some("x".into()),
            time: Some(1),
            extras: HashMap::new(),
            raw_response: Value::Null,
        };
        let bytes = serde_json::to_vec(&n).unwrap();
        let back: StreamCallbackNotification = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.radio_id, Some(3));
        assert_eq!(back.notification_code, Some(650));
        assert_eq!(back.time, Some(1));
    }

    #[test]
    fn offset_to_seconds_parses_all_shapes() {
        assert_eq!(offset_to_seconds(Some("60")), Some(60.0));
        assert_eq!(offset_to_seconds(Some("01:04")), Some(64.0));
        assert_eq!(offset_to_seconds(Some("00:01:00")), Some(60.0));
        assert_eq!(offset_to_seconds(Some("01:02:03")), Some(3723.0));
        assert_eq!(offset_to_seconds(Some("60.5")), Some(60.5));
        // Unparseable / absent → None, never a panic.
        assert_eq!(offset_to_seconds(Some("not-a-time")), None);
        assert_eq!(offset_to_seconds(Some("")), None);
        assert_eq!(offset_to_seconds(Some("1:2:3:4")), None);
        assert_eq!(offset_to_seconds(None), None);
    }

    #[test]
    fn enterprise_match_thumb() {
        let m = EnterpriseMatch {
            score: Some(80),
            timecode: Some("00:01".into()),
            song_link: Some("https://lis.tn/abc".into()),
            ..Default::default()
        };
        assert_eq!(
            m.thumbnail_url().as_deref(),
            Some("https://lis.tn/abc?thumb")
        );
    }

    // ----- StreamingProvider / streaming_url / preview_url -----

    #[test]
    fn streaming_url_prefers_direct_apple_music() {
        let v = json!({
            "timecode": "00:01",
            "song_link": "https://lis.tn/abc",
            "apple_music": {"url": "https://music.apple.com/track/123"}
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(
            r.streaming_url(StreamingProvider::AppleMusic).as_deref(),
            Some("https://music.apple.com/track/123"),
        );
    }

    #[test]
    fn streaming_url_falls_back_to_lis_tn_redirect() {
        let v = json!({
            "timecode": "00:01",
            "song_link": "https://lis.tn/abc"
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        // No metadata blocks → fall back to lis.tn redirect.
        assert_eq!(
            r.streaming_url(StreamingProvider::Spotify).as_deref(),
            Some("https://lis.tn/abc?spotify"),
        );
        assert_eq!(
            r.streaming_url(StreamingProvider::YouTube).as_deref(),
            Some("https://lis.tn/abc?youtube"),
        );
    }

    #[test]
    fn streaming_url_returns_none_for_youtube_song_link() {
        // YouTube song-link host → no lis.tn fallback, no metadata block ever.
        let v = json!({
            "timecode": "00:01",
            "song_link": "https://www.youtube.com/watch?v=abc"
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.streaming_url(StreamingProvider::Spotify), None);
        assert_eq!(r.streaming_url(StreamingProvider::YouTube), None);
    }

    #[test]
    fn streaming_urls_lists_all_resolvable() {
        let v = json!({
            "timecode": "00:01",
            "song_link": "https://lis.tn/abc",
            "deezer": {"link": "https://deezer.com/track/9"}
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        let urls = r.streaming_urls();
        // Deezer is direct, others use lis.tn redirect.
        assert_eq!(
            urls.get(&StreamingProvider::Deezer).map(String::as_str),
            Some("https://deezer.com/track/9"),
        );
        assert_eq!(
            urls.get(&StreamingProvider::Spotify).map(String::as_str),
            Some("https://lis.tn/abc?spotify"),
        );
        assert_eq!(urls.len(), 5);
    }

    #[test]
    fn streaming_url_spotify_external_urls_in_extras() {
        // Spotify ships external_urls.spotify in the metadata block; we read
        // it through the forward-compat extras map.
        let v = json!({
            "timecode": "00:01",
            "spotify": {
                "id": "abc",
                "external_urls": {"spotify": "https://open.spotify.com/track/abc"}
            }
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(
            r.streaming_url(StreamingProvider::Spotify).as_deref(),
            Some("https://open.spotify.com/track/abc"),
        );
    }

    #[test]
    fn preview_url_apple_music_first() {
        let v = json!({
            "timecode": "00:01",
            "apple_music": {
                "previews": [{"url": "https://itunes/preview.m4a"}]
            },
            "spotify": {"preview_url": "https://spotify/preview.mp3"}
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(
            r.preview_url().as_deref(),
            Some("https://itunes/preview.m4a")
        );
    }

    #[test]
    fn preview_url_falls_through_to_deezer() {
        let v = json!({
            "timecode": "00:01",
            "deezer": {"preview": "https://deezer/preview.mp3"}
        });
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(
            r.preview_url().as_deref(),
            Some("https://deezer/preview.mp3")
        );
    }

    #[test]
    fn preview_url_none_when_absent() {
        let v = json!({"timecode": "00:01"});
        let r: RecognitionResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.preview_url(), None);
    }

    #[test]
    fn enterprise_match_streaming_urls_lis_tn_only() {
        let m = EnterpriseMatch {
            score: Some(90),
            timecode: Some("00:01".into()),
            song_link: Some("https://lis.tn/abc".into()),
            ..Default::default()
        };
        let urls = m.streaming_urls();
        assert_eq!(urls.len(), 5);
        assert_eq!(
            urls.get(&StreamingProvider::Spotify).map(String::as_str),
            Some("https://lis.tn/abc?spotify"),
        );
    }

    #[test]
    fn enterprise_match_streaming_urls_empty_for_youtube() {
        let m = EnterpriseMatch {
            score: Some(90),
            timecode: Some("00:01".into()),
            song_link: Some("https://www.youtube.com/watch?v=x".into()),
            ..Default::default()
        };
        assert!(m.streaming_urls().is_empty());
    }

    // ----- Serialize round-trip on every public model -----

    #[test]
    fn serialize_round_trip_recognition_result_typed_fields() {
        // Deserialize a typical fixture, serialize it back, and verify the
        // typed fields survive the round-trip. Extras (HashMap) may permute
        // key order — that's fine; we re-parse and compare typed fields only.
        let v = json!({
            "artist": "Tears For Fears",
            "title": "Everybody Wants To Rule The World",
            "album": "Songs From The Big Chair",
            "release_date": "1985-03-25",
            "label": "Mercury Records",
            "timecode": "00:56",
            "song_link": "https://lis.tn/NbkVb",
            "apple_music": {
                "name": "Everybody Wants To Rule The World",
                "artistName": "Tears For Fears",
                "url": "https://music.apple.com/track/123",
                "isrc": "GBF088400024"
            },
            "spotify": {
                "id": "abc",
                "name": "Everybody Wants To Rule The World",
                "external_urls": {"spotify": "https://open.spotify.com/track/abc"}
            },
            "deezer": {"id": 9, "title": "Everybody Wants To Rule The World", "link": "https://deezer.com/track/9"},
            "napster": {"id": "n1", "name": "X", "isrc": "GBF088400024"},
            "musicbrainz": [{"id": "mb1", "title": "X"}],
            "tidal": {"id": "t1"}
        });
        let original: RecognitionResult = serde_json::from_value(v).unwrap();
        let bytes = serde_json::to_vec(&original).unwrap();
        let round_tripped: RecognitionResult = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(round_tripped.artist, original.artist);
        assert_eq!(round_tripped.title, original.title);
        assert_eq!(round_tripped.album, original.album);
        assert_eq!(round_tripped.release_date, original.release_date);
        assert_eq!(round_tripped.label, original.label);
        assert_eq!(round_tripped.timecode, original.timecode);
        assert_eq!(round_tripped.song_link, original.song_link);
        assert_eq!(round_tripped.apple_music, original.apple_music);
        assert_eq!(round_tripped.spotify, original.spotify);
        assert_eq!(round_tripped.deezer, original.deezer);
        assert_eq!(round_tripped.napster, original.napster);
        assert_eq!(round_tripped.musicbrainz, original.musicbrainz);
        // Extras (HashMap) — key order may permute on serialize; compare the
        // forward-compat field by lookup.
        assert_eq!(
            round_tripped.extras.get("tidal").and_then(|v| v.get("id")),
            original.extras.get("tidal").and_then(|v| v.get("id"))
        );
    }

    #[test]
    fn serialize_round_trip_enterprise_chunk() {
        let v = json!({
            "songs": [{
                "score": 95,
                "timecode": "00:00",
                "artist": "X",
                "title": "Y",
                "song_link": "https://lis.tn/abc",
                "isrc": "AAA",
                "upc": "BBB",
                "start_offset": 0,
                "end_offset": 30
            }],
            "offset": "00:00"
        });
        let original: EnterpriseChunkResult = serde_json::from_value(v).unwrap();
        let bytes = serde_json::to_vec(&original).unwrap();
        let round_tripped: EnterpriseChunkResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(round_tripped.offset, original.offset);
        assert_eq!(round_tripped.songs.len(), 1);
        assert_eq!(round_tripped.songs[0], original.songs[0]);
    }

    #[test]
    fn enterprise_song_without_score_parses() {
        // Regression: the enterprise endpoint legitimately returns songs with
        // no `score` (and no `isrc`/`upc`/`label`). Parsing must succeed and
        // leave the absent fields as `None` — never a deserialization error.
        let v = json!({
            "songs": [{
                "timecode": "00:00",
                "artist": "X",
                "title": "Y"
            }],
            "offset": "00:00"
        });
        let chunk: EnterpriseChunkResult = serde_json::from_value(v).unwrap();
        assert_eq!(chunk.songs.len(), 1);
        let song = &chunk.songs[0];
        assert_eq!(song.score, None);
        assert_eq!(song.isrc, None);
        assert_eq!(song.upc, None);
        assert_eq!(song.label, None);
        assert_eq!(song.artist.as_deref(), Some("X"));
    }

    #[test]
    fn enterprise_match_missing_timecode_parses() {
        // A match with no `timecode` must parse rather than error.
        let v = json!({"artist": "X", "title": "Y"});
        let m: EnterpriseMatch = serde_json::from_value(v).unwrap();
        assert_eq!(m.timecode, None);
        assert_eq!(m.score, None);
    }

    #[test]
    fn serialize_round_trip_stream() {
        let s = Stream {
            radio_id: Some(42),
            url: Some("https://stream.example/live".into()),
            stream_running: true,
            longpoll_category: Some("abc123".into()),
            extras: HashMap::new(),
        };
        let bytes = serde_json::to_vec(&s).unwrap();
        let round_tripped: Stream = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(round_tripped, s);
    }

    #[test]
    fn serialize_round_trip_lyrics_result() {
        let l = LyricsResult {
            artist: Some("Tears For Fears".into()),
            title: Some("Everybody Wants To Rule The World".into()),
            lyrics: Some("Welcome to your life…".into()),
            song_id: Some(1),
            media: Some("https://media.example/x".into()),
            full_title: Some("Tears For Fears – Everybody Wants To Rule The World".into()),
            artist_id: Some(99),
            song_link: Some("https://lis.tn/abc".into()),
            extras: HashMap::new(),
        };
        let bytes = serde_json::to_vec(&l).unwrap();
        let round_tripped: LyricsResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(round_tripped, l);
    }

    #[test]
    fn serialize_round_trip_streaming_provider() {
        for (provider, wire) in [
            (StreamingProvider::Spotify, "\"spotify\""),
            (StreamingProvider::AppleMusic, "\"apple_music\""),
            (StreamingProvider::Deezer, "\"deezer\""),
            (StreamingProvider::Napster, "\"napster\""),
            (StreamingProvider::YouTube, "\"youtube\""),
        ] {
            let s = serde_json::to_string(&provider).unwrap();
            assert_eq!(s, wire, "{provider:?} should serialize as {wire}");
            let back: StreamingProvider = serde_json::from_str(&s).unwrap();
            assert_eq!(back, provider);
        }
    }

    #[test]
    fn serialize_round_trip_recognition_match_tagged() {
        let r = RecognitionResult {
            timecode: Some("00:01".into()),
            audio_id: Some(7),
            ..Default::default()
        };
        let m = RecognitionMatch::from(r);
        let s = serde_json::to_value(&m).unwrap();
        // Tagged: {"kind":"custom","result":{...}}
        assert_eq!(s.get("kind").and_then(|v| v.as_str()), Some("custom"));
        let back: RecognitionMatch = serde_json::from_value(s).unwrap();
        assert_eq!(back, m);
    }
}
