//! Walk a folder of audio files, recognize each via AudD, write the result
//! into the file's tags via `lofty` (works for MP3, FLAC, OGG/Opus, M4A/MP4,
//! WAV, AAC), then rename the file to `Artist - Title.ext`.
//!
//! Defaults to **dry-run** — pass `--apply` to actually mutate files.
//!
//! Run:
//! ```text
//! AUDD_API_TOKEN=... cargo run --example scan_and_rename -- /path/to/folder
//! AUDD_API_TOKEN=... cargo run --example scan_and_rename -- /path/to/folder --apply --concurrency 8
//! ```
//!
//! Reads the api_token from `AUDD_API_TOKEN`. Skips files that don't match.

#![allow(clippy::result_large_err)] // matches lib.rs — `AudDError` is intentionally rich

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use audd::{AudD, RecognitionResult};
use clap::Parser;
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use walkdir::WalkDir;

/// Audio file extensions we'll attempt to recognize.
const AUDIO_EXTS: &[&str] = &["mp3", "flac", "ogg", "opus", "m4a", "mp4", "wav", "aac"];

/// Maximum length of a sanitized artist/title segment in the new filename.
const MAX_NAME_SEGMENT: usize = 200;

#[derive(Parser, Debug)]
#[command(
    name = "scan_and_rename",
    about = "Recognize every audio file in a folder, write tags, and rename to `Artist - Title.ext`.",
    long_about = None,
)]
struct Args {
    /// Folder to scan recursively.
    folder: PathBuf,

    /// Actually mutate files. Without this flag the tool runs in dry-run mode.
    #[arg(long)]
    apply: bool,

    /// Maximum number of concurrent recognitions.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
}

#[derive(Default)]
struct Summary {
    seen: usize,
    matched: usize,
    no_match: usize,
    skipped: usize,
    errors: usize,
}

impl Summary {
    fn merge(&mut self, other: &Self) {
        self.seen += other.seen;
        self.matched += other.matched;
        self.no_match += other.no_match;
        self.skipped += other.skipped;
        self.errors += other.errors;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if !args.folder.is_dir() {
        return Err(format!("not a directory: {}", args.folder.display()).into());
    }
    let concurrency = args.concurrency.max(1);

    // Empty token => SDK falls back to AUDD_API_TOKEN env var.
    let audd = Arc::new(AudD::new(
        std::env::var("AUDD_API_TOKEN").unwrap_or_default(),
    ));
    let sem = Arc::new(Semaphore::new(concurrency));

    let mode_label = if args.apply { "APPLY" } else { "DRY-RUN" };
    println!(
        "[{mode_label}] scanning {} (concurrency={concurrency})",
        args.folder.display()
    );

    let mut set: JoinSet<Summary> = JoinSet::new();
    for entry in WalkDir::new(&args.folder)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if !is_audio_file(&path) {
            continue;
        }

        let audd = Arc::clone(&audd);
        let sem = Arc::clone(&sem);
        let apply = args.apply;
        set.spawn(async move {
            // Acquire-on-task-spawn keeps queueing simple: every file gets
            // queued, but only `concurrency` recognitions run at once.
            let _permit = sem.acquire_owned().await.expect("semaphore not closed");
            process_one(&audd, &path, apply).await
        });
    }

    let mut total = Summary::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(s) => total.merge(&s),
            Err(e) => {
                eprintln!("task panicked: {e}");
                total.errors += 1;
            }
        }
    }

    println!(
        "\nsummary: seen={} matched={} no_match={} skipped={} errors={} (mode={})",
        total.seen, total.matched, total.no_match, total.skipped, total.errors, mode_label,
    );
    if !args.apply {
        println!("dry-run: re-run with --apply to write tags and rename files.");
    }
    Ok(())
}

/// Does `path` have one of the audio extensions we care about?
fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| AUDIO_EXTS.contains(&e.as_str()))
}

/// Recognize a single file, write tags + rename if `apply`. Returns a per-file
/// summary so the caller can roll them up.
async fn process_one(audd: &AudD, path: &Path, apply: bool) -> Summary {
    let mut s = Summary {
        seen: 1,
        ..Summary::default()
    };
    let display = path.display();

    let result = match audd.recognize(path).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            println!("[no match] {display}");
            s.no_match = 1;
            return s;
        }
        Err(e) => {
            eprintln!("[error]    {display}: {e}");
            s.errors = 1;
            return s;
        }
    };

    s.matched = 1;
    let artist = result.artist.as_deref().unwrap_or("").trim();
    let title = result.title.as_deref().unwrap_or("").trim();
    if artist.is_empty() || title.is_empty() {
        eprintln!("[skip]     {display}: matched but missing artist/title");
        s.matched = 0;
        s.skipped = 1;
        return s;
    }

    let new_name = sanitized_filename(artist, title, path);
    let new_path = path.with_file_name(&new_name);

    if apply {
        if let Err(e) = write_tags(path, &result) {
            eprintln!("[tag-fail] {display}: {e}");
            s.errors = 1;
            return s;
        }
        if new_path == path {
            println!("[ok]       {display}: tags written, name already correct");
        } else if new_path.exists() {
            println!(
                "[collide]  {display}: '{}' exists, not renaming",
                new_path.display()
            );
            s.skipped = 1;
        } else if let Err(e) = std::fs::rename(path, &new_path) {
            eprintln!("[rename-fail] {display}: {e}");
            s.errors = 1;
        } else {
            println!("[renamed]  {display} -> {}", new_path.display());
        }
    } else if new_path == path {
        println!("[match]    {display}: {artist} — {title} (name already correct)");
    } else if new_path.exists() {
        println!(
            "[match]    {display}: {artist} — {title} (would skip rename: '{}' exists)",
            new_path.display()
        );
    } else {
        println!(
            "[match]    {display}: {artist} — {title} (would rename -> {})",
            new_path.display()
        );
    }
    s
}

/// Compute `Artist - Title.ext` with disallowed chars replaced by `_` and each
/// segment capped at [`MAX_NAME_SEGMENT`] characters.
fn sanitized_filename(artist: &str, title: &str, original: &Path) -> String {
    let ext = original.extension().and_then(OsStr::to_str).unwrap_or("");
    let safe_artist = sanitize_segment(artist);
    let safe_title = sanitize_segment(title);
    if ext.is_empty() {
        format!("{safe_artist} - {safe_title}")
    } else {
        format!("{safe_artist} - {safe_title}.{ext}")
    }
}

/// Replace cross-platform-unsafe filename chars with `_` and cap length.
fn sanitize_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            // Strip control chars; they don't belong in filenames.
            c if (c as u32) < 0x20 => out.push('_'),
            c => out.push(c),
        }
    }
    let trimmed = out.trim().trim_end_matches('.').to_string();
    // Cap by char count, not byte count, so we don't split a UTF-8 codepoint.
    if trimmed.chars().count() > MAX_NAME_SEGMENT {
        trimmed.chars().take(MAX_NAME_SEGMENT).collect()
    } else {
        trimmed
    }
}

/// Write recognition metadata into the file's primary tag using `lofty`.
fn write_tags(path: &Path, r: &RecognitionResult) -> lofty::error::Result<()> {
    let mut tagged = Probe::open(path)?.read()?;
    let tag = match tagged.primary_tag_mut() {
        Some(t) => t,
        None => match tagged.first_tag_mut() {
            Some(t) => t,
            None => {
                let tag_type = tagged.primary_tag_type();
                tagged.insert_tag(Tag::new(tag_type));
                tagged.primary_tag_mut().expect("tag was just inserted")
            }
        },
    };

    if let Some(a) = r.artist.as_deref() {
        tag.set_artist(a.to_string());
    }
    if let Some(t) = r.title.as_deref() {
        tag.set_title(t.to_string());
    }
    if let Some(al) = r.album.as_deref() {
        tag.set_album(al.to_string());
    }
    // release_date is `YYYY-MM-DD` on the AudD wire format; keep just the year.
    if let Some(year) = r.release_date.as_deref().and_then(parse_year) {
        tag.set_year(year);
    }

    tag.save_to_path(path, WriteOptions::default())
}

/// Pull a 4-digit year out of an AudD `release_date` string. Tolerant of
/// e.g. "1985", "1985-03-25", or just nonsense (returns `None`).
fn parse_year(s: &str) -> Option<u32> {
    let head: String = s.chars().take(4).collect();
    head.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_disallowed() {
        assert_eq!(
            sanitize_segment(r#"AC/DC: "Back" \in\ <Black>?|*"#),
            "AC_DC_ _Back_ _in_ _Black____"
        );
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "x".repeat(MAX_NAME_SEGMENT + 50);
        assert_eq!(sanitize_segment(&long).chars().count(), MAX_NAME_SEGMENT);
    }

    #[test]
    fn parse_year_handles_full_date() {
        assert_eq!(parse_year("1985-03-25"), Some(1985));
        assert_eq!(parse_year("1985"), Some(1985));
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("Q4 1985"), None);
    }

    #[test]
    fn sanitized_filename_keeps_extension() {
        let p = Path::new("/tmp/whatever.flac");
        assert_eq!(
            sanitized_filename("Tears For Fears", "Everybody Wants To Rule The World", p),
            "Tears For Fears - Everybody Wants To Rule The World.flac"
        );
    }

    #[test]
    fn is_audio_recognizes_common_extensions() {
        for ext in AUDIO_EXTS {
            let p = PathBuf::from(format!("clip.{ext}"));
            assert!(is_audio_file(&p), "{ext}");
        }
        assert!(!is_audio_file(Path::new("notes.txt")));
        assert!(!is_audio_file(Path::new("clip")));
    }
}
