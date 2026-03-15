use std::time::Duration;

use serde_json::Value;
use sqlx::SqlitePool;
use tracing::{debug, error, info, warn};

use crate::cache::db;
use crate::id::encode_id;
use crate::proxy::backend::BackendClient;
use crate::subsonic::models::NamespaceIds;

/// Spawn the background cache refresh task.
pub fn spawn_refresh_task(
    db: SqlitePool,
    backends: Vec<BackendClient>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        // Initial sync after a short delay to let the server start
        tokio::time::sleep(Duration::from_secs(5)).await;
        info!("cache refresh: initial sync starting");
        refresh_all(&db, &backends, false).await;

        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await; // skip the first immediate tick
        loop {
            interval.tick().await;
            debug!("cache refresh: periodic sync starting");
            refresh_all(&db, &backends, true).await;
        }
    });
}

/// Run a one-shot sync (for `fugue sync` CLI command / admin endpoint).
pub async fn run_sync(db: &SqlitePool, backends: &[BackendClient]) {
    refresh_all(db, backends, false).await;
}

async fn refresh_all(db: &SqlitePool, backends: &[BackendClient], incremental: bool) {
    // Register backends so foreign keys are satisfied
    for backend in backends {
        if let Err(e) = sqlx::query(
            "INSERT INTO backends (idx, name, url) VALUES (?, ?, ?)
             ON CONFLICT(idx) DO UPDATE SET name = excluded.name, url = excluded.url",
        )
        .bind(backend.index as i64)
        .bind(&backend.name)
        .bind(&backend.base_url)
        .execute(db)
        .await
        {
            error!("failed to register backend {}: {}", backend.name, e);
        }
    }

    let mut any_changed = false;
    for backend in backends {
        match refresh_backend(db, backend, incremental).await {
            Ok(changed) => {
                if changed {
                    any_changed = true;
                }
            }
            Err(e) => {
                error!("cache refresh failed for backend {}: {}", backend.name, e);
            }
        }
    }

    match db::cache_stats(db).await {
        Ok((artists, albums, tracks)) => {
            info!(
                "cache refresh complete: {} artists, {} albums, {} tracks",
                artists, albums, tracks
            );
        }
        Err(e) => warn!("cache stats query failed: {}", e),
    }

    // Only run dedup if something changed
    if any_changed {
        if let Err(e) = crate::dedup::run_dedup(db).await {
            error!("dedup failed: {}", e);
        }
        let weights: Vec<(usize, i32)> = backends.iter().map(|b| (b.index, b.weight)).collect();
        if let Err(e) = crate::dedup::resolver::update_scores(db, &weights).await {
            error!("dedup score update failed: {}", e);
        }
    } else {
        debug!("cache refresh: no changes detected, skipping dedup");
    }
}

/// Refresh a single backend. Returns true if anything changed.
async fn refresh_backend(
    db: &SqlitePool,
    backend: &BackendClient,
    incremental: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    // 1. Quick change detection via getIndexes?ifModifiedSince
    if incremental {
        let last_modified = get_last_modified(db, backend.index).await;
        if let Some(last_mod_ms) = last_modified {
            let ms_str = last_mod_ms.to_string();
            let resp = backend
                .request_json("getIndexes", &[("ifModifiedSince", &ms_str)])
                .await?;

            // Check if the response indicates no changes
            // If indexes.lastModified matches our stored value, nothing changed
            let new_last_modified = resp
                .get("indexes")
                .and_then(|i| i.get("lastModified"))
                .and_then(|l| l.as_i64())
                .or_else(|| {
                    resp.get("indexes")
                        .and_then(|i| i.get("lastModified"))
                        .and_then(|l| l.as_str())
                        .and_then(|s| s.parse().ok())
                });

            if let Some(new_lm) = new_last_modified {
                if new_lm <= last_mod_ms {
                    // Check for new albums too (indexes only covers artist changes)
                    let has_new_albums = check_new_albums(db, backend).await?;
                    if !has_new_albums {
                        info!(
                            "cache refresh: backend {} unchanged (lastModified={}), skipping",
                            backend.name, last_mod_ms
                        );
                        return Ok(false);
                    }
                    info!(
                        "cache refresh: backend {} has new albums, doing incremental sync",
                        backend.name
                    );
                    return incremental_sync(db, backend).await;
                }
            }

            // Artist index changed — check if we can do incremental or need full
            info!(
                "cache refresh: backend {} index changed, doing incremental sync",
                backend.name
            );
            return incremental_sync(db, backend).await;
        }
    }

    // Full sync (first run or forced)
    full_sync(db, backend).await
}

/// Check if there are new albums by comparing newest album timestamps.
async fn check_new_albums(
    db: &SqlitePool,
    backend: &BackendClient,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Get the newest album's created timestamp from our cache for this backend
    let cached_newest: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT MAX(updated_at) FROM albums WHERE backend_idx = ?",
    )
    .bind(backend.index as i64)
    .fetch_optional(db)
    .await?;

    // Fetch the single newest album from the backend
    let resp = backend
        .request_json("getAlbumList2", &[("type", "newest"), ("size", "1")])
        .await?;

    let newest_created = resp
        .get("albumList2")
        .and_then(|al| al.get("album"))
        .and_then(|a| a.as_array())
        .and_then(|arr| arr.first())
        .and_then(|a| a.get("created"))
        .and_then(|c| c.as_str());

    if let (Some((Some(cached_ts),)), Some(backend_ts)) = (cached_newest, newest_created) {
        if backend_ts > cached_ts.as_str() {
            debug!(
                "cache refresh: backend {} has newer albums (backend={} > cache={})",
                backend.name, backend_ts, cached_ts
            );
            return Ok(true);
        }
        return Ok(false);
    }

    // If we can't compare, assume changed
    Ok(true)
}

/// Incremental sync: only fetch new/changed albums and their tracks.
async fn incremental_sync(
    db: &SqlitePool,
    backend: &BackendClient,
) -> Result<bool, Box<dyn std::error::Error>> {
    info!("cache refresh: incremental sync for backend {}", backend.name);

    // 1. Refresh artist index (lightweight — just metadata)
    sync_artists(db, backend).await?;

    // 2. Fetch newest albums and check which ones are new to our cache
    let mut new_album_count = 0;
    let mut new_track_count = 0;
    let mut offset = 0;
    let batch_size: usize = 50;

    loop {
        let offset_str = offset.to_string();
        let size_str = batch_size.to_string();
        let resp = backend
            .request_json(
                "getAlbumList2",
                &[("type", "newest"), ("size", &size_str), ("offset", &offset_str)],
            )
            .await?;

        let albums = resp
            .get("albumList2")
            .and_then(|al| al.get("album"))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();

        if albums.is_empty() {
            break;
        }

        let mut found_existing = false;
        for album in &albums {
            let original_id = album.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let namespaced_id = encode_id(backend.index, original_id);

            // Check if this album already exists in our cache
            let exists: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM albums WHERE id = ?",
            )
            .bind(&namespaced_id)
            .fetch_optional(db)
            .await?;

            if exists.is_some() {
                found_existing = true;
                continue;
            }

            // New album — cache it and fetch its tracks
            cache_album(db, backend, album).await;
            new_album_count += 1;

            // Fetch tracks for this new album
            match backend
                .request_json("getAlbum", &[("id", original_id)])
                .await
            {
                Ok(resp) => {
                    if let Some(songs) = resp
                        .get("album")
                        .and_then(|a| a.get("song"))
                        .and_then(|s| s.as_array())
                    {
                        for song in songs {
                            cache_track(db, backend, song, &namespaced_id).await;
                            new_track_count += 1;
                        }
                    }
                }
                Err(e) => warn!("incremental sync: failed to get album {}: {}", original_id, e),
            }
        }

        // If we hit an album that already exists, we've caught up
        if found_existing || albums.len() < batch_size {
            break;
        }
        offset += batch_size;
    }

    // Update sync timestamp
    let key = format!("backend_{}_last_sync", backend.index);
    db::set_cache_meta(db, &key, &epoch_now_str()).await.map_err(|e| format!("set meta: {e}"))?;

    // Store lastModified from getIndexes for next change detection
    if let Ok(resp) = backend.request_json("getIndexes", &[]).await {
        if let Some(last_mod) = resp
            .get("indexes")
            .and_then(|i| i.get("lastModified"))
            .and_then(|l| l.as_i64())
        {
            let key = format!("backend_{}_last_modified", backend.index);
            db::set_cache_meta(db, &key, &last_mod.to_string())
                .await
                .map_err(|e| format!("set lastModified: {e}"))?;
        }
    }

    if new_album_count > 0 || new_track_count > 0 {
        info!(
            "cache refresh: backend {} incremental sync - {} new albums, {} new tracks",
            backend.name, new_album_count, new_track_count
        );
        Ok(true)
    } else {
        info!("cache refresh: backend {} incremental sync - no new content", backend.name);
        Ok(false)
    }
}

/// Full sync: crawl everything from scratch.
async fn full_sync(
    db: &SqlitePool,
    backend: &BackendClient,
) -> Result<bool, Box<dyn std::error::Error>> {
    info!("cache refresh: full sync for backend {} ({})", backend.name, backend.base_url);

    // 1. Sync artists
    let artist_count = sync_artists(db, backend).await?;

    // 2. Crawl albums via getAlbumList2 in batches
    let mut album_offset = 0;
    let batch_size: usize = 500;
    let mut total_albums = 0;

    loop {
        let offset_str = album_offset.to_string();
        let size_str = batch_size.to_string();
        let resp = backend
            .request_json(
                "getAlbumList2",
                &[
                    ("type", "alphabeticalByName"),
                    ("size", &size_str),
                    ("offset", &offset_str),
                ],
            )
            .await?;

        let albums = resp
            .get("albumList2")
            .and_then(|al| al.get("album"))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();

        if albums.is_empty() {
            break;
        }

        let batch_count = albums.len();
        for album in &albums {
            cache_album(db, backend, album).await;
            total_albums += 1;
        }

        debug!(
            "cache refresh: backend {} - albums batch offset={} count={}",
            backend.name, album_offset, batch_count
        );

        if batch_count < batch_size {
            break;
        }
        album_offset += batch_size;
    }

    // 3. Crawl tracks per album
    let mut total_tracks = 0;
    let album_ids: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, original_id FROM albums WHERE backend_idx = ?",
    )
    .bind(backend.index as i64)
    .fetch_all(db)
    .await?;

    for (namespaced_album_id, original_album_id) in &album_ids {
        match backend
            .request_json("getAlbum", &[("id", original_album_id)])
            .await
        {
            Ok(resp) => {
                if let Some(songs) = resp
                    .get("album")
                    .and_then(|a| a.get("song"))
                    .and_then(|s| s.as_array())
                {
                    for song in songs {
                        cache_track(db, backend, song, namespaced_album_id).await;
                        total_tracks += 1;
                    }
                }
            }
            Err(e) => {
                warn!(
                    "cache refresh: failed to get album {} from {}: {}",
                    original_album_id, backend.name, e
                );
            }
        }
    }

    // Store sync timestamp
    let key = format!("backend_{}_last_sync", backend.index);
    db::set_cache_meta(db, &key, &epoch_now_str()).await.map_err(|e| format!("set meta: {e}"))?;

    // Store lastModified for future change detection
    if let Ok(resp) = backend.request_json("getIndexes", &[]).await {
        if let Some(last_mod) = resp
            .get("indexes")
            .and_then(|i| i.get("lastModified"))
            .and_then(|l| l.as_i64())
        {
            let key = format!("backend_{}_last_modified", backend.index);
            db::set_cache_meta(db, &key, &last_mod.to_string())
                .await
                .map_err(|e| format!("set lastModified: {e}"))?;
        }
    }

    info!(
        "cache refresh: backend {} full sync done - {} artists, {} albums, {} tracks",
        backend.name, artist_count, total_albums, total_tracks
    );

    Ok(true)
}

/// Sync the artist index for a backend.
async fn sync_artists(
    db: &SqlitePool,
    backend: &BackendClient,
) -> Result<usize, Box<dyn std::error::Error>> {
    let artists_resp = backend.request_json("getArtists", &[]).await?;
    let mut count = 0;

    if let Some(indexes) = artists_resp
        .get("artists")
        .and_then(|a| a.get("index"))
        .and_then(|i| i.as_array())
    {
        for index in indexes {
            if let Some(artists) = index.get("artist").and_then(|a| a.as_array()) {
                for artist in artists {
                    let original_id = artist.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    let name = artist.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                    let album_count = artist.get("albumCount").and_then(|v| v.as_i64()).unwrap_or(0);

                    let namespaced_id = encode_id(backend.index, original_id);
                    let mut data = artist.clone();
                    data.namespace_ids(backend.index);

                    db::upsert_artist(
                        db,
                        &namespaced_id,
                        backend.index,
                        original_id,
                        name,
                        album_count,
                        &serde_json::to_string(&data)?,
                    )
                    .await
                    .map_err(|e| format!("upsert artist: {e}"))?;

                    count += 1;
                }
            }
        }
    }

    debug!("cache refresh: backend {} - {} artists synced", backend.name, count);
    Ok(count)
}

/// Get the stored lastModified timestamp for a backend's index.
async fn get_last_modified(db: &SqlitePool, backend_idx: usize) -> Option<i64> {
    let key = format!("backend_{}_last_modified", backend_idx);
    db::get_cache_meta(db, &key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

async fn cache_album(db: &SqlitePool, backend: &BackendClient, album: &Value) {
    let original_id = album.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let name = album.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let artist = album.get("artist").and_then(|v| v.as_str());
    let original_artist_id = album.get("artistId").and_then(|v| v.as_str());
    let year = album.get("year").and_then(|v| v.as_i64());
    let genre = album.get("genre").and_then(|v| v.as_str());
    let song_count = album.get("songCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let duration = album.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);

    let namespaced_id = encode_id(backend.index, original_id);
    let namespaced_artist_id = original_artist_id.map(|aid| encode_id(backend.index, aid));

    let mut data = album.clone();
    data.namespace_ids(backend.index);

    let json_str = match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(_) => return,
    };

    let _ = db::upsert_album(
        db,
        &namespaced_id,
        backend.index,
        original_id,
        name,
        artist,
        namespaced_artist_id.as_deref(),
        year,
        genre,
        song_count,
        duration,
        &json_str,
    )
    .await;
}

async fn cache_track(
    db: &SqlitePool,
    backend: &BackendClient,
    song: &Value,
    namespaced_album_id: &str,
) {
    let original_id = song.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let title = song.get("title").and_then(|v| v.as_str()).unwrap_or_default();
    let artist = song.get("artist").and_then(|v| v.as_str());
    let album = song.get("album").and_then(|v| v.as_str());
    let track_number = song.get("track").and_then(|v| v.as_i64());
    let duration = song.get("duration").and_then(|v| v.as_i64());
    let bitrate = song.get("bitRate").and_then(|v| v.as_i64());
    let content_type = song.get("contentType").and_then(|v| v.as_str());
    let suffix = song.get("suffix").and_then(|v| v.as_str());

    let namespaced_id = encode_id(backend.index, original_id);

    let mut data = song.clone();
    data.namespace_ids(backend.index);

    let json_str = match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(_) => return,
    };

    let _ = db::upsert_track(
        db,
        &namespaced_id,
        backend.index,
        original_id,
        title,
        artist,
        album,
        Some(namespaced_album_id),
        track_number,
        duration,
        bitrate,
        content_type,
        suffix,
        &json_str,
    )
    .await;
}

fn epoch_now_str() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}
