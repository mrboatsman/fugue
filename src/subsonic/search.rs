use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::debug;

use crate::cache::db as cache_db;
use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::models::merge_search_results;
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn search2(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let query = params.raw.get("query").cloned().unwrap_or_default();
    let artist_count: usize = params.raw.get("artistCount").and_then(|v| v.parse().ok()).unwrap_or(20);
    let album_count: usize = params.raw.get("albumCount").and_then(|v| v.parse().ok()).unwrap_or(20);
    let song_count: usize = params.raw.get("songCount").and_then(|v| v.parse().ok()).unwrap_or(20);

    debug!("search2 query={} artistCount={} albumCount={} songCount={}", query, artist_count, album_count, song_count);

    // Try cache first
    if !query.is_empty() {
        if let Ok((artists, albums, songs)) =
            cache_db::search_cached(state.db(), &query, artist_count, album_count, song_count).await
        {
            if !artists.is_empty() || !albums.is_empty() || !songs.is_empty() {
                debug!("search2 serving from cache");
                return Ok(SubsonicResponse::ok(
                    params.format,
                    json!({
                        "searchResult2": {
                            "artist": artists,
                            "album": albums,
                            "song": songs,
                        }
                    }),
                ));
            }
        }
    }

    let mut extra_params: Vec<(&str, &str)> = vec![("query", &query)];
    let ac = artist_count.to_string();
    let alc = album_count.to_string();
    let sc = song_count.to_string();
    extra_params.push(("artistCount", &ac));
    extra_params.push(("albumCount", &alc));
    extra_params.push(("songCount", &sc));

    let results = fan_out(state.backends(), "search2", &extra_params).await?;
    let mut merged = merge_search_results(results, artist_count, album_count, song_count);

    if let Some(sr3) = merged.as_object_mut().and_then(|m| m.remove("searchResult3")) {
        merged.as_object_mut().unwrap().insert("searchResult2".into(), sr3);
    }

    Ok(SubsonicResponse::ok(params.format, merged))
}

pub async fn search3(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let query = params.raw.get("query").cloned().unwrap_or_default();
    let artist_count: usize = params.raw.get("artistCount").and_then(|v| v.parse().ok()).unwrap_or(20);
    let album_count: usize = params.raw.get("albumCount").and_then(|v| v.parse().ok()).unwrap_or(20);
    let song_count: usize = params.raw.get("songCount").and_then(|v| v.parse().ok()).unwrap_or(20);

    debug!("search3 query={} artistCount={} albumCount={} songCount={}", query, artist_count, album_count, song_count);

    // Try cache first
    if !query.is_empty() {
        if let Ok((artists, albums, songs)) =
            cache_db::search_cached(state.db(), &query, artist_count, album_count, song_count).await
        {
            if !artists.is_empty() || !albums.is_empty() || !songs.is_empty() {
                debug!("search3 serving from cache");
                return Ok(SubsonicResponse::ok(
                    params.format,
                    json!({
                        "searchResult3": {
                            "artist": artists,
                            "album": albums,
                            "song": songs,
                        }
                    }),
                ));
            }
        }
    }

    let mut extra_params: Vec<(&str, &str)> = vec![("query", &query)];
    let ac = artist_count.to_string();
    let alc = album_count.to_string();
    let sc = song_count.to_string();
    extra_params.push(("artistCount", &ac));
    extra_params.push(("albumCount", &alc));
    extra_params.push(("songCount", &sc));

    let results = fan_out(state.backends(), "search3", &extra_params).await?;
    let merged = merge_search_results(results, artist_count, album_count, song_count);

    Ok(SubsonicResponse::ok(params.format, merged))
}
