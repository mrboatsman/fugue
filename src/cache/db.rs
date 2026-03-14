use serde_json::Value;
use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;

// --- Cache meta helpers ---

pub async fn get_cache_meta(db: &SqlitePool, key: &str) -> Result<Option<String>, FugueError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM cache_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(v,)| v))
}

pub async fn set_cache_meta(db: &SqlitePool, key: &str, value: &str) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO cache_meta (key, value, updated_at) VALUES (?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// Check if cache for a backend is fresh (updated within `max_age_secs`).
pub async fn is_cache_fresh(
    db: &SqlitePool,
    backend_idx: usize,
    max_age_secs: u64,
) -> Result<bool, FugueError> {
    let key = format!("backend_{}_last_sync", backend_idx);
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM cache_meta WHERE key = ?")
            .bind(&key)
            .fetch_optional(db)
            .await?;

    if let Some((timestamp,)) = row {
        // Parse and check age
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT (strftime('%s', 'now') - strftime('%s', ?)) as age",
        )
        .bind(&timestamp)
        .fetch_optional(db)
        .await?;

        if let Some((age,)) = row {
            return Ok(age < max_age_secs as i64);
        }
    }
    Ok(false)
}

// --- Artist CRUD ---

pub async fn upsert_artist(
    db: &SqlitePool,
    id: &str,
    backend_idx: usize,
    original_id: &str,
    name: &str,
    album_count: i64,
    data_json: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO artists (id, backend_idx, original_id, name, name_norm, album_count, data_json, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, name_norm = excluded.name_norm,
           album_count = excluded.album_count, data_json = excluded.data_json,
           updated_at = excluded.updated_at",
    )
    .bind(id)
    .bind(backend_idx as i64)
    .bind(original_id)
    .bind(name)
    .bind(name.to_lowercase().trim().to_string())
    .bind(album_count)
    .bind(data_json)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn get_all_artists(db: &SqlitePool) -> Result<Vec<Value>, FugueError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT data_json FROM artists ORDER BY name_norm")
            .fetch_all(db)
            .await?;

    let artists: Vec<Value> = rows
        .into_iter()
        .filter_map(|(json,)| serde_json::from_str(&json).ok())
        .collect();

    debug!("cache get_all_artists count={}", artists.len());
    Ok(artists)
}

// --- Album CRUD ---

pub async fn upsert_album(
    db: &SqlitePool,
    id: &str,
    backend_idx: usize,
    original_id: &str,
    name: &str,
    artist: Option<&str>,
    artist_id: Option<&str>,
    year: Option<i64>,
    genre: Option<&str>,
    song_count: i64,
    duration: i64,
    data_json: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO albums (id, backend_idx, original_id, name, name_norm, artist, artist_id, year, genre, song_count, duration, data_json, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, name_norm = excluded.name_norm,
           artist = excluded.artist, artist_id = excluded.artist_id,
           year = excluded.year, genre = excluded.genre,
           song_count = excluded.song_count, duration = excluded.duration,
           data_json = excluded.data_json, updated_at = excluded.updated_at",
    )
    .bind(id)
    .bind(backend_idx as i64)
    .bind(original_id)
    .bind(name)
    .bind(name.to_lowercase().trim().to_string())
    .bind(artist)
    .bind(artist_id)
    .bind(year)
    .bind(genre)
    .bind(song_count)
    .bind(duration)
    .bind(data_json)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn get_all_albums(
    db: &SqlitePool,
    sort: &str,
    size: usize,
    offset: usize,
) -> Result<Vec<Value>, FugueError> {
    let order = match sort {
        "newest" => "updated_at DESC",
        "alphabeticalByName" => "name_norm ASC",
        "alphabeticalByArtist" => "LOWER(COALESCE(artist, '')) ASC, name_norm ASC",
        "byYear" => "year ASC, name_norm ASC",
        _ => "name_norm ASC",
    };

    let query = format!(
        "SELECT data_json FROM albums ORDER BY {} LIMIT ? OFFSET ?",
        order
    );

    let rows: Vec<(String,)> = sqlx::query_as(&query)
        .bind(size as i64)
        .bind(offset as i64)
        .fetch_all(db)
        .await?;

    let albums: Vec<Value> = rows
        .into_iter()
        .filter_map(|(json,)| serde_json::from_str(&json).ok())
        .collect();

    debug!("cache get_all_albums sort={} count={}", sort, albums.len());
    Ok(albums)
}

pub async fn get_albums_by_artist(
    db: &SqlitePool,
    artist_id: &str,
) -> Result<Vec<Value>, FugueError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT data_json FROM albums WHERE artist_id = ? ORDER BY COALESCE(year, 0), name_norm")
            .bind(artist_id)
            .fetch_all(db)
            .await?;

    Ok(rows
        .into_iter()
        .filter_map(|(json,)| serde_json::from_str(&json).ok())
        .collect())
}

// --- Track CRUD ---

pub async fn upsert_track(
    db: &SqlitePool,
    id: &str,
    backend_idx: usize,
    original_id: &str,
    title: &str,
    artist: Option<&str>,
    album: Option<&str>,
    album_id: Option<&str>,
    track_number: Option<i64>,
    duration: Option<i64>,
    bitrate: Option<i64>,
    content_type: Option<&str>,
    suffix: Option<&str>,
    data_json: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO tracks (id, backend_idx, original_id, title, title_norm, artist, album, album_id, track_number, duration, bitrate, content_type, suffix, data_json, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
           title = excluded.title, title_norm = excluded.title_norm,
           artist = excluded.artist, album = excluded.album,
           album_id = excluded.album_id, track_number = excluded.track_number,
           duration = excluded.duration, bitrate = excluded.bitrate,
           content_type = excluded.content_type, suffix = excluded.suffix,
           data_json = excluded.data_json, updated_at = excluded.updated_at",
    )
    .bind(id)
    .bind(backend_idx as i64)
    .bind(original_id)
    .bind(title)
    .bind(title.to_lowercase().trim().to_string())
    .bind(artist)
    .bind(album)
    .bind(album_id)
    .bind(track_number)
    .bind(duration)
    .bind(bitrate)
    .bind(content_type)
    .bind(suffix)
    .bind(data_json)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn search_cached(
    db: &SqlitePool,
    query: &str,
    artist_count: usize,
    album_count: usize,
    song_count: usize,
) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>), FugueError> {
    let pattern = format!("%{}%", query.to_lowercase());

    // Artists: deduplicate by name_norm, keep the one with highest album_count
    let artists: Vec<(String,)> = sqlx::query_as(
        "SELECT data_json FROM artists
         WHERE name_norm LIKE ?
         AND id IN (
           SELECT a1.id FROM artists a1
           WHERE NOT EXISTS (
             SELECT 1 FROM artists a2
             WHERE a2.name_norm = a1.name_norm
             AND a2.album_count > a1.album_count
           )
           GROUP BY a1.name_norm
         )
         LIMIT ?",
    )
    .bind(&pattern)
    .bind(artist_count as i64)
    .fetch_all(db)
    .await?;

    // Albums: exclude hidden dedup members (keep best-scored only)
    let albums: Vec<(String,)> = sqlx::query_as(
        "SELECT data_json FROM albums
         WHERE name_norm LIKE ?
         AND id NOT IN (
           SELECT dm.namespaced_id FROM dedup_members dm
           JOIN dedup_groups dg ON dg.fingerprint = dm.fingerprint
           WHERE dg.entity_type = 'album'
           AND dm.namespaced_id != (
             SELECT dm2.namespaced_id FROM dedup_members dm2
             WHERE dm2.fingerprint = dm.fingerprint
             ORDER BY dm2.score DESC, dm2.rowid ASC
             LIMIT 1
           )
         )
         LIMIT ?",
    )
    .bind(&pattern)
    .bind(album_count as i64)
    .fetch_all(db)
    .await?;

    // Tracks: exclude hidden dedup members (keep best-scored only)
    let songs: Vec<(String,)> = sqlx::query_as(
        "SELECT data_json FROM tracks
         WHERE title_norm LIKE ?
         AND id NOT IN (
           SELECT dm.namespaced_id FROM dedup_members dm
           JOIN dedup_groups dg ON dg.fingerprint = dm.fingerprint
           WHERE dg.entity_type = 'track'
           AND dm.namespaced_id != (
             SELECT dm2.namespaced_id FROM dedup_members dm2
             WHERE dm2.fingerprint = dm.fingerprint
             ORDER BY dm2.score DESC, dm2.rowid ASC
             LIMIT 1
           )
         )
         LIMIT ?",
    )
    .bind(&pattern)
    .bind(song_count as i64)
    .fetch_all(db)
    .await?;

    debug!(
        "cache search query={} artists={} albums={} songs={}",
        query,
        artists.len(),
        albums.len(),
        songs.len()
    );

    Ok((
        artists.into_iter().filter_map(|(j,)| serde_json::from_str(&j).ok()).collect(),
        albums.into_iter().filter_map(|(j,)| serde_json::from_str(&j).ok()).collect(),
        songs.into_iter().filter_map(|(j,)| serde_json::from_str(&j).ok()).collect(),
    ))
}

/// Remove all cached data for a given backend (before re-syncing).
pub async fn clear_backend(db: &SqlitePool, backend_idx: usize) -> Result<(), FugueError> {
    let idx = backend_idx as i64;
    sqlx::query("DELETE FROM tracks WHERE backend_idx = ?").bind(idx).execute(db).await?;
    sqlx::query("DELETE FROM albums WHERE backend_idx = ?").bind(idx).execute(db).await?;
    sqlx::query("DELETE FROM artists WHERE backend_idx = ?").bind(idx).execute(db).await?;
    debug!("cache cleared backend_idx={}", backend_idx);
    Ok(())
}

/// Get deduplicated albums. For albums that exist in dedup_groups, only return
/// the best member (highest score). Non-duplicate albums pass through unchanged.
pub async fn get_all_albums_deduped(
    db: &SqlitePool,
    sort: &str,
    size: usize,
    offset: usize,
) -> Result<Vec<Value>, FugueError> {
    // Get the IDs of albums that are NOT the best member of their dedup group
    // (i.e., the ones we want to hide)
    let hidden_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT dm.namespaced_id FROM dedup_members dm
         JOIN dedup_groups dg ON dg.fingerprint = dm.fingerprint
         WHERE dg.entity_type = 'album'
         AND dm.namespaced_id != (
           SELECT dm2.namespaced_id FROM dedup_members dm2
           WHERE dm2.fingerprint = dm.fingerprint
           ORDER BY dm2.score DESC, dm2.rowid ASC
           LIMIT 1
         )",
    )
    .fetch_all(db)
    .await?;

    let hidden: std::collections::HashSet<String> =
        hidden_ids.into_iter().map(|(id,)| id).collect();

    // Get all albums, then filter out hidden ones
    let order = match sort {
        "newest" => "updated_at DESC",
        "alphabeticalByName" => "name_norm ASC",
        "alphabeticalByArtist" => "LOWER(COALESCE(artist, '')) ASC, name_norm ASC",
        "byYear" => "year ASC, name_norm ASC",
        _ => "name_norm ASC",
    };

    let query = format!("SELECT id, data_json FROM albums ORDER BY {}", order);
    let rows: Vec<(String, String)> = sqlx::query_as(&query).fetch_all(db).await?;

    let albums: Vec<Value> = rows
        .into_iter()
        .filter(|(id, _)| !hidden.contains(id))
        .filter_map(|(_, json)| serde_json::from_str(&json).ok())
        .skip(offset)
        .take(size)
        .collect();

    debug!("cache get_all_albums_deduped sort={} count={} hidden={}", sort, albums.len(), hidden.len());
    Ok(albums)
}

/// Get deduplicated artists. Groups by normalized name, keeps the version
/// with the most albums but patches albumCount to reflect the total unique
/// albums across all backends (after album dedup).
pub async fn get_all_artists_deduped(db: &SqlitePool) -> Result<Vec<Value>, FugueError> {
    // For each unique artist name, pick the representative row (highest album_count)
    // and count the actual unique (non-hidden) albums across all backends.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.id, a.data_json FROM artists a
         INNER JOIN (
           SELECT name_norm, MAX(album_count) as max_ac
           FROM artists GROUP BY name_norm
         ) best ON a.name_norm = best.name_norm AND a.album_count = best.max_ac
         GROUP BY a.name_norm
         ORDER BY a.name_norm",
    )
    .fetch_all(db)
    .await?;

    let mut artists = Vec::new();
    for (_artist_id, json_str) in rows {
        let mut artist: Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Count unique albums for this artist across all backends (deduped)
        let name_norm: String = artist
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_lowercase();

        let album_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(DISTINCT COALESCE(
               (SELECT dm.fingerprint FROM dedup_members dm
                JOIN dedup_groups dg ON dg.fingerprint = dm.fingerprint
                WHERE dm.namespaced_id = alb.id AND dg.entity_type = 'album'),
               alb.id
             ))
             FROM albums alb
             JOIN artists art ON alb.artist_id = art.id
             WHERE art.name_norm = ?",
        )
        .bind(&name_norm)
        .fetch_one(db)
        .await
        .unwrap_or((0,));

        if let Some(obj) = artist.as_object_mut() {
            obj.insert("albumCount".into(), Value::Number(album_count.0.into()));
        }

        artists.push(artist);
    }

    debug!("cache get_all_artists_deduped count={}", artists.len());
    Ok(artists)
}

/// Find all namespaced artist IDs across backends for the same artist name.
pub async fn find_artist_ids_by_name(
    db: &SqlitePool,
    artist_namespaced_id: &str,
) -> Result<Vec<(String, i64)>, FugueError> {
    // Find the normalized name of the given artist
    let name_row: Option<(String,)> = sqlx::query_as(
        "SELECT name_norm FROM artists WHERE id = ?",
    )
    .bind(artist_namespaced_id)
    .fetch_optional(db)
    .await?;

    let name_norm = match name_row {
        Some((n,)) => n,
        None => return Ok(vec![]),
    };

    // Find all artist IDs with the same normalized name
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, backend_idx FROM artists WHERE name_norm = ?",
    )
    .bind(&name_norm)
    .fetch_all(db)
    .await?;

    Ok(rows)
}

/// Get total counts for cache status.
pub async fn cache_stats(db: &SqlitePool) -> Result<(i64, i64, i64), FugueError> {
    let artists: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM artists")
        .fetch_one(db)
        .await?;
    let albums: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM albums")
        .fetch_one(db)
        .await?;
    let tracks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tracks")
        .fetch_one(db)
        .await?;
    Ok((artists.0, albums.0, tracks.0))
}
