//! Library sharing: publish local library metadata for friends to discover.

use sqlx::SqlitePool;
use tracing::{debug, info};

use crate::cache::db as cache_db;
use crate::error::FugueError;

/// Publish the local library index as a summary for friends.
/// Returns a JSON blob that can be shared via iroh-docs.
pub async fn build_library_summary(db: &SqlitePool) -> Result<serde_json::Value, FugueError> {
    let (artist_count, album_count, track_count) = cache_db::cache_stats(db).await?;

    // Get all artists (just names and IDs for the summary)
    let artists: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, name FROM artists ORDER BY name_norm",
    )
    .fetch_all(db)
    .await?;

    let artist_list: Vec<serde_json::Value> = artists
        .into_iter()
        .map(|(id, name)| {
            serde_json::json!({
                "id": id,
                "name": name,
            })
        })
        .collect();

    // Get all albums (summary)
    let albums: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, artist, year FROM albums ORDER BY name_norm",
    )
    .fetch_all(db)
    .await?;

    let album_list: Vec<serde_json::Value> = albums
        .into_iter()
        .map(|(id, name, artist, year)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "artist": artist,
                "year": year,
            })
        })
        .collect();

    let summary = serde_json::json!({
        "version": 1,
        "stats": {
            "artists": artist_count,
            "albums": album_count,
            "tracks": track_count,
        },
        "artists": artist_list,
        "albums": album_list,
    });

    debug!(
        "social: library summary built - {} artists, {} albums, {} tracks",
        artist_count, album_count, track_count
    );

    Ok(summary)
}

/// Store a friend's library summary in the local database for merged browsing.
pub async fn store_friend_library(
    db: &SqlitePool,
    friend_node_id: &str,
    friend_name: &str,
    summary: &serde_json::Value,
) -> Result<(), FugueError> {
    let json_str = serde_json::to_string(summary)
        .map_err(|e| FugueError::Internal(format!("serialize friend library: {e}")))?;

    sqlx::query(
        "INSERT INTO cache_meta (key, value, updated_at) VALUES (?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(format!("friend_library_{}", friend_node_id))
    .bind(&json_str)
    .execute(db)
    .await?;

    info!(
        "social: stored library summary from friend {} ({})",
        friend_name, friend_node_id
    );
    Ok(())
}

/// Get a friend's stored library summary.
pub async fn get_friend_library(
    db: &SqlitePool,
    friend_node_id: &str,
) -> Result<Option<serde_json::Value>, FugueError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT value FROM cache_meta WHERE key = ?",
    )
    .bind(format!("friend_library_{}", friend_node_id))
    .fetch_optional(db)
    .await?;

    match row {
        Some((json_str,)) => {
            let value: serde_json::Value = serde_json::from_str(&json_str)
                .map_err(|e| FugueError::Internal(format!("parse friend library: {e}")))?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}
