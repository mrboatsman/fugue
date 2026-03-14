use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::debug;

use crate::cache::db as cache_db;
use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::proxy::router::route_to_backend;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::favorites_db;
use crate::subsonic::models::{merge_album_lists, merge_random_songs, NamespaceIds};
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn get_album_list(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let list_type = params.raw.get("type").cloned().unwrap_or_else(|| "newest".into());
    let size: usize = params.raw.get("size").and_then(|v| v.parse().ok()).unwrap_or(10);
    let offset: usize = params.raw.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);

    debug!("getAlbumList type={} size={} offset={}", list_type, size, offset);

    if list_type == "starred" || list_type == "starred2" {
        let albums = resolve_starred_albums(&state, &auth.username, size, offset).await?;
        return Ok(SubsonicResponse::ok(
            params.format,
            json!({ "albumList": { "album": albums } }),
        ));
    }

    let extra: Vec<(&str, &str)> = vec![("type", &list_type)];
    let results = fan_out(state.backends(), "getAlbumList", &extra).await?;
    let mut merged = merge_album_lists(results, &list_type, size, offset);

    // Rename albumList2 to albumList for this endpoint
    if let Some(al2) = merged.as_object_mut().and_then(|m| m.remove("albumList2")) {
        merged.as_object_mut().unwrap().insert("albumList".into(), al2);
    }

    Ok(SubsonicResponse::ok(params.format, merged))
}

pub async fn get_album_list2(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let list_type = params.raw.get("type").cloned().unwrap_or_else(|| "newest".into());
    let size: usize = params.raw.get("size").and_then(|v| v.parse().ok()).unwrap_or(10);
    let offset: usize = params.raw.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);

    debug!("getAlbumList2 type={} size={} offset={}", list_type, size, offset);

    if list_type == "starred" || list_type == "starred2" {
        let albums = resolve_starred_albums(&state, &auth.username, size, offset).await?;
        return Ok(SubsonicResponse::ok(
            params.format,
            json!({ "albumList2": { "album": albums } }),
        ));
    }

    // Serve from cache for sortable types
    let cacheable = matches!(
        list_type.as_str(),
        "newest" | "alphabeticalByName" | "alphabeticalByArtist" | "byYear"
    );
    if cacheable && any_cache_fresh(&state).await {
        let albums = cache_db::get_all_albums_deduped(state.db(), &list_type, size, offset).await?;
        if !albums.is_empty() {
            debug!("getAlbumList2 serving from cache ({} albums, deduped)", albums.len());
            return Ok(SubsonicResponse::ok(
                params.format,
                json!({ "albumList2": { "album": albums } }),
            ));
        }
    }

    let extra: Vec<(&str, &str)> = vec![("type", &list_type)];
    let results = fan_out(state.backends(), "getAlbumList2", &extra).await?;
    let merged = merge_album_lists(results, &list_type, size, offset);

    Ok(SubsonicResponse::ok(params.format, merged))
}

/// Resolve starred albums from local favorites DB by fetching metadata from backends.
async fn resolve_starred_albums(
    state: &AppState,
    username: &str,
    size: usize,
    offset: usize,
) -> Result<Vec<serde_json::Value>, FugueError> {
    let starred = favorites_db::get_starred_ids(state.db(), username, "album").await?;
    debug!("resolve_starred_albums user={} total={} offset={} size={}", username, starred.len(), offset, size);

    let mut albums = Vec::new();
    for (namespaced_id, starred_at) in starred.iter().skip(offset).take(size) {
        let Ok((backend, original_id)) = route_to_backend(state, namespaced_id) else {
            continue;
        };
        match backend.request_json("getAlbum", &[("id", &original_id)]).await {
            Ok(mut resp) => {
                resp.namespace_ids(backend.index);
                if let Some(mut album) = resp.get("album").cloned() {
                    if let Some(obj) = album.as_object_mut() {
                        obj.insert("starred".into(), json!(starred_at));
                    }
                    albums.push(album);
                }
            }
            Err(_) => continue,
        }
    }

    Ok(albums)
}

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

pub async fn get_random_songs(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let size: usize = params.raw.get("size").and_then(|v| v.parse().ok()).unwrap_or(10);

    debug!("getRandomSongs size={}", size);
    let results = fan_out(state.backends(), "getRandomSongs", &[("size", "50")]).await?;
    let merged = merge_random_songs(results, size);

    Ok(SubsonicResponse::ok(params.format, merged))
}
