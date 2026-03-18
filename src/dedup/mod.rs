//! Cross-backend deduplication engine.
//!
//! When the same music exists on multiple backends, Fugue detects the overlap
//! and presents a single unified library with no duplicates.
//!
//! # Fingerprinting
//!
//! After each cache refresh, every track and album is fingerprinted using
//! normalized metadata: `artist :: album :: title :: track_number`.
//!
//! Normalization rules:
//! - Case-insensitive comparison
//! - Strips noise like `(Remastered 2011)`, `[Deluxe Edition]`, etc.
//!
//! This means the same album with slightly different names across backends
//! still matches.
//!
//! # Scoring
//!
//! Duplicate groups are stored in SQLite with a score per source based on:
//! - **Bitrate** — higher is better
//! - **Format quality** — FLAC > Opus > MP3
//! - **Backend weight** — configured per-backend in `fugue.toml`
//!
//! # Client View
//!
//! - Album and artist lists show only one copy of each duplicate (the
//!   highest-scored version)
//! - When an artist exists on multiple backends, opening the artist merges
//!   albums from all backends and deduplicates — the client sees the full
//!   combined discography
//! - Streaming picks the best available source; if that backend goes down,
//!   Fugue falls back to the next best
//!
//! # Example
//!
//! ```text
//! Backend 1 (weight=10): Artist X → Album A, Album B, Album C
//! Backend 2 (weight=5):  Artist X → Album B, Album C, Album D, Album E
//!
//! Client sees: Artist X → Album A, B, C, D, E  (5 albums, no duplicates)
//! Streaming Album B → picks Backend 1 (higher weight + same bitrate)
//! ```
//!
//! # Submodules
//!
//! - [`matcher`] — fingerprint computation, duplicate group detection
//! - [`resolver`] — best-source selection for a given duplicate group

pub mod matcher;
pub mod resolver;

use sqlx::SqlitePool;
use tracing::info;

use crate::error::FugueError;

/// Run deduplication across all cached tracks and albums.
/// Called after a cache refresh cycle completes.
pub async fn run_dedup(db: &SqlitePool) -> Result<(), FugueError> {
    info!("dedup: starting deduplication pass");

    // Deduplicate tracks
    let track_groups = matcher::find_duplicate_tracks(db).await?;
    let mut track_dedup_count = 0;
    for (fingerprint, members) in &track_groups {
        if members.len() < 2 {
            continue;
        }
        matcher::upsert_dedup_group(db, fingerprint, "track").await?;
        for member in members {
            matcher::upsert_dedup_member(
                db,
                fingerprint,
                &member.namespaced_id,
                member.backend_idx,
                member.bitrate,
                member.format.as_deref(),
            )
            .await?;
        }
        track_dedup_count += 1;
    }

    // Deduplicate albums
    let album_groups = matcher::find_duplicate_albums(db).await?;
    let mut album_dedup_count = 0;
    for (fingerprint, members) in &album_groups {
        if members.len() < 2 {
            continue;
        }
        matcher::upsert_dedup_group(db, fingerprint, "album").await?;
        for member in members {
            matcher::upsert_dedup_member(
                db,
                fingerprint,
                &member.namespaced_id,
                member.backend_idx,
                member.bitrate,
                member.format.as_deref(),
            )
            .await?;
        }
        album_dedup_count += 1;
    }

    info!(
        "dedup: complete - {} track groups, {} album groups",
        track_dedup_count, album_dedup_count
    );

    Ok(())
}
