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
