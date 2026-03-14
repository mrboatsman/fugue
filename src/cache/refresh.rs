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
        refresh_all(&db, &backends).await;

        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await; // skip the first immediate tick
        loop {
            interval.tick().await;
            debug!("cache refresh: periodic sync starting");
            refresh_all(&db, &backends).await;
        }
    });
}

/// Run a one-shot sync (for `fugue sync` CLI command).
pub async fn run_sync(db: &SqlitePool, backends: &[BackendClient]) {
    refresh_all(db, backends).await;
}

async fn refresh_all(db: &SqlitePool, backends: &[BackendClient]) {
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

    for backend in backends {
        if let Err(e) = refresh_backend(db, backend).await {
            error!("cache refresh failed for backend {}: {}", backend.name, e);
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

    // Run deduplication after cache is populated
    if let Err(e) = crate::dedup::run_dedup(db).await {
        error!("dedup failed: {}", e);
    }

    // Update dedup scores with backend config weights
    let weights: Vec<(usize, i32)> = backends.iter().map(|b| (b.index, b.weight)).collect();
    if let Err(e) = crate::dedup::resolver::update_scores(db, &weights).await {
        error!("dedup score update failed: {}", e);
    }
}

async fn refresh_backend(
    db: &SqlitePool,
    backend: &BackendClient,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("cache refresh: syncing backend {} ({})", backend.name, backend.base_url);

    // 1. Crawl artists via getArtists
    let artists_resp = backend.request_json("getArtists", &[]).await?;
    let mut artist_count = 0;

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
                    let album_count_val = artist.get("albumCount").and_then(|v| v.as_i64()).unwrap_or(0);

                    let namespaced_id = encode_id(backend.index, original_id);

                    // Namespace the IDs in the stored JSON
                    let mut data = artist.clone();
                    data.namespace_ids(backend.index);

                    db::upsert_artist(
                        db,
                        &namespaced_id,
                        backend.index,
                        original_id,
                        name,
                        album_count_val,
                        &serde_json::to_string(&data)?,
                    )
                    .await
                    .map_err(|e| format!("upsert artist: {e}"))?;

                    artist_count += 1;
                }
            }
        }
    }

    debug!("cache refresh: backend {} - {} artists indexed", backend.name, artist_count);

    // 2. Crawl albums via getAlbumList2 in batches
    let mut album_offset = 0;
    let batch_size = 500;
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

    debug!("cache refresh: backend {} - {} albums indexed", backend.name, total_albums);

    // 3. Crawl tracks per album (getSong data comes from getAlbum responses)
    // We re-fetch each album to get its songs — but only for albums we haven't
    // crawled tracks for yet. For efficiency, we do this in the album loop above
    // by fetching getAlbum for each album to get the song list.
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

    debug!(
        "cache refresh: backend {} - {} tracks indexed",
        backend.name, total_tracks
    );

    // Mark sync time
    let key = format!("backend_{}_last_sync", backend.index);
    db::set_cache_meta(db, &key, &chrono_now()).await.map_err(|e| format!("set meta: {e}"))?;

    info!(
        "cache refresh: backend {} done - {} artists, {} albums, {} tracks",
        backend.name, artist_count, total_albums, total_tracks
    );

    Ok(())
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

fn chrono_now() -> String {
    // SQLite-compatible UTC timestamp without pulling in chrono crate
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // We store as unix timestamp string; SQLite's strftime('%s', value) can compare it
    // But for consistency with datetime('now'), let's use a proper format
    // We'll just let SQLite generate it via a query
    now.to_string()
}
