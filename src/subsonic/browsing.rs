use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use std::collections::BTreeMap;
use tracing::debug;

use crate::cache::db as cache_db;
use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::proxy::router::route_to_backend;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::models::{merge_artist_indexes, NamespaceIds};
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn get_music_folders(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getMusicFolders");
    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "musicFolders": {
                "musicFolder": [{
                    "id": 0,
                    "name": "Fugue Library"
                }]
            }
        }),
    ))
}

pub async fn get_artists(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getArtists");

    // Try cache first (deduplicated)
    if any_cache_fresh(&state).await {
        let artists = cache_db::get_all_artists_deduped(state.db()).await?;
        if !artists.is_empty() {
            debug!("getArtists serving from cache ({} artists, deduped)", artists.len());
            let indexed = build_artist_index(&artists);
            return Ok(SubsonicResponse::ok(params.format, indexed));
        }
    }

    // Fall back to fan-out
    let results = fan_out(state.backends(), "getArtists", &[]).await?;
    let merged = merge_artist_indexes(results);
    Ok(SubsonicResponse::ok(params.format, merged))
}

pub async fn get_indexes(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getIndexes");

    if any_cache_fresh(&state).await {
        let artists = cache_db::get_all_artists_deduped(state.db()).await?;
        if !artists.is_empty() {
            debug!("getIndexes serving from cache (deduped)");
            let indexed = build_artist_index(&artists);
            if let Some(artists_val) = indexed.get("artists") {
                return Ok(SubsonicResponse::ok(
                    params.format,
                    json!({ "indexes": artists_val }),
                ));
            }
        }
    }

    let results = fan_out(state.backends(), "getIndexes", &[]).await?;
    let merged = merge_artist_indexes(results);
    if let Some(artists) = merged.get("artists") {
        Ok(SubsonicResponse::ok(
            params.format,
            json!({ "indexes": artists }),
        ))
    } else {
        Ok(SubsonicResponse::ok(params.format, merged))
    }
}

pub async fn get_artist(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    // Check if the same artist exists on multiple backends
    let sibling_ids = cache_db::find_artist_ids_by_name(state.db(), id).await?;

    if sibling_ids.len() > 1 {
        debug!(
            "getArtist id={} found on {} backends, merging albums",
            id,
            sibling_ids.len()
        );

        // Get the primary artist response
        let (backend, original_id) = route_to_backend(&state, id)?;
        let mut resp = backend
            .request_json("getArtist", &[("id", &original_id)])
            .await?;
        resp.namespace_ids(backend.index);

        // Collect albums from all other backends with the same artist
        let mut extra_albums = Vec::new();
        for (sibling_id, _) in &sibling_ids {
            if sibling_id == id {
                continue;
            }
            if let Ok((sib_backend, sib_original_id)) = route_to_backend(&state, sibling_id) {
                if let Ok(mut sib_resp) = sib_backend
                    .request_json("getArtist", &[("id", &sib_original_id)])
                    .await
                {
                    sib_resp.namespace_ids(sib_backend.index);
                    if let Some(albums) = sib_resp
                        .get("artist")
                        .and_then(|a| a.get("album"))
                        .and_then(|a| a.as_array())
                    {
                        extra_albums.extend(albums.clone());
                    }
                }
            }
        }

        // Merge extra albums into the primary response and deduplicate
        if !extra_albums.is_empty() {
            if let Some(artist) = resp.get_mut("artist") {
                if let Some(albums) = artist.get_mut("album").and_then(|a| a.as_array_mut()) {
                    albums.extend(extra_albums);

                    // Deduplicate by normalized album name
                    let mut seen = std::collections::HashSet::new();
                    albums.retain(|album| {
                        let name = album
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        seen.insert(name)
                    });

                    // Sort by year then name
                    albums.sort_by(|a, b| {
                        let ay = a.get("year").and_then(|y| y.as_i64()).unwrap_or(0);
                        let by = b.get("year").and_then(|y| y.as_i64()).unwrap_or(0);
                        ay.cmp(&by).then_with(|| {
                            let an = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let bn = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            an.to_lowercase().cmp(&bn.to_lowercase())
                        })
                    });
                }

                // Update albumCount
                if let Some(obj) = artist.as_object_mut() {
                    let count = obj
                        .get("album")
                        .and_then(|a| a.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    obj.insert("albumCount".into(), json!(count));
                }
            }
        }

        return Ok(SubsonicResponse::ok(params.format, resp));
    }

    // Single backend — simple route
    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("getArtist id={} -> backend={}", id, backend.name);
    let mut resp = backend
        .request_json("getArtist", &[("id", &original_id)])
        .await?;
    resp.namespace_ids(backend.index);

    Ok(SubsonicResponse::ok(params.format, resp))
}

pub async fn get_album(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("getAlbum id={} -> backend={}", id, backend.name);
    let mut resp = backend
        .request_json("getAlbum", &[("id", &original_id)])
        .await?;
    resp.namespace_ids(backend.index);

    Ok(SubsonicResponse::ok(params.format, resp))
}

pub async fn get_song(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("getSong id={} -> backend={}", id, backend.name);
    let mut resp = backend
        .request_json("getSong", &[("id", &original_id)])
        .await?;
    resp.namespace_ids(backend.index);

    Ok(SubsonicResponse::ok(params.format, resp))
}

pub async fn get_genres(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getGenres");
    let results = fan_out(state.backends(), "getGenres", &[]).await?;

    use std::collections::HashMap;
    let mut genre_map: HashMap<String, (u64, u64)> = HashMap::new();

    for (_, resp) in results {
        if let Some(genres) = resp.get("genres").and_then(|g| g.get("genre")).and_then(|g| g.as_array()) {
            for genre in genres {
                let name = genre.get("value").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let song_count = genre.get("songCount").and_then(|c| c.as_u64()).unwrap_or(0);
                let album_count = genre.get("albumCount").and_then(|c| c.as_u64()).unwrap_or(0);
                let entry = genre_map.entry(name).or_insert((0, 0));
                entry.0 += song_count;
                entry.1 += album_count;
            }
        }
    }

    let genres: Vec<serde_json::Value> = genre_map
        .into_iter()
        .map(|(name, (songs, albums))| {
            json!({
                "value": name,
                "songCount": songs,
                "albumCount": albums,
            })
        })
        .collect();

    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "genres": {
                "genre": genres
            }
        }),
    ))
}

// --- Helpers ---

/// Check if at least one backend's cache is fresh.
async fn any_cache_fresh(state: &AppState) -> bool {
    let max_age = state.config().cache.refresh_interval_secs * 2;
    for (i, _) in state.backends().iter().enumerate() {
        if cache_db::is_cache_fresh(state.db(), i, max_age)
            .await
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// Build Subsonic-format artist index from a flat list of cached artists.
fn build_artist_index(artists: &[serde_json::Value]) -> serde_json::Value {
    let mut index_map: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for artist in artists {
        let name = artist
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("#");
        let letter = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "#".into());
        let key = if letter.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
            letter
        } else {
            "#".into()
        };
        index_map.entry(key).or_default().push(artist.clone());
    }

    let indexes: Vec<serde_json::Value> = index_map
        .into_iter()
        .map(|(name, artists)| {
            json!({
                "name": name,
                "artist": artists,
            })
        })
        .collect();

    json!({
        "artists": {
            "ignoredArticles": "The El La Los Las Le Les",
            "index": indexes,
        }
    })
}
