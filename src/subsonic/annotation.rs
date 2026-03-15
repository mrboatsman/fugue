use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::debug;

use crate::error::FugueError;
use crate::proxy::router::route_to_backend;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::favorites_db;
use crate::subsonic::models::NamespaceIds;
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn star(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    if let Some(id) = params.raw.get("id") {
        debug!("star id={} user={}", id, auth.username);
        favorites_db::star(state.db(), &auth.username, id, "song").await?;

        // Auto-star the parent album so it appears in getAlbumList2?type=starred
        match route_to_backend(&state, id) {
            Ok((backend, original_id)) => {
                match backend.request_json("getSong", &[("id", &original_id)]).await {
                    Ok(resp) => {
                        debug!("star getSong response keys={:?}", resp.as_object().map(|o| o.keys().collect::<Vec<_>>()));
                        if let Some(album_id) = resp.get("song").and_then(|s| s.get("albumId")).and_then(|a| a.as_str()) {
                            let namespaced_album_id = crate::id::encode_id(backend.index, album_id);
                            debug!("star auto-starring parent album={} for id={}", namespaced_album_id, id);
                            favorites_db::star(state.db(), &auth.username, &namespaced_album_id, "album").await?;
                        } else {
                            debug!("star no albumId found in getSong response for id={}", id);
                        }
                    }
                    Err(e) => {
                        debug!("star getSong failed for id={}: {}, trying as album", id, e);
                        // The id might be an album, not a song — star it as album too
                        favorites_db::star(state.db(), &auth.username, id, "album").await?;
                    }
                }
            }
            Err(e) => debug!("star route failed for id={}: {}", id, e),
        }
    }
    if let Some(id) = params.raw.get("albumId") {
        debug!("star album={} user={}", id, auth.username);
        favorites_db::star(state.db(), &auth.username, id, "album").await?;
    }
    if let Some(id) = params.raw.get("artistId") {
        debug!("star artist={} user={}", id, auth.username);
        favorites_db::star(state.db(), &auth.username, id, "artist").await?;
    }

    Ok(SubsonicResponse::empty(params.format))
}

pub async fn unstar(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    if let Some(id) = params.raw.get("id") {
        debug!("unstar song={} user={}", id, auth.username);
        favorites_db::unstar(state.db(), &auth.username, id).await?;
    }
    if let Some(id) = params.raw.get("albumId") {
        debug!("unstar album={} user={}", id, auth.username);
        favorites_db::unstar(state.db(), &auth.username, id).await?;
    }
    if let Some(id) = params.raw.get("artistId") {
        debug!("unstar artist={} user={}", id, auth.username);
        favorites_db::unstar(state.db(), &auth.username, id).await?;
    }

    Ok(SubsonicResponse::empty(params.format))
}

/// Build starred response by resolving metadata for each favorited item from its backend.
pub async fn get_starred(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getStarred user={}", auth.username);
    let result = build_starred_response(&state, &auth.username).await?;

    // Rename starred2 -> starred for this endpoint
    let mut out = result;
    if let Some(s2) = out.as_object_mut().and_then(|m| m.remove("starred2")) {
        out.as_object_mut().unwrap().insert("starred".into(), s2);
    }

    Ok(SubsonicResponse::ok(params.format, out))
}

pub async fn get_starred2(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getStarred2 user={}", auth.username);
    let result = build_starred_response(&state, &auth.username).await?;
    Ok(SubsonicResponse::ok(params.format, result))
}

async fn build_starred_response(
    state: &AppState,
    username: &str,
) -> Result<serde_json::Value, FugueError> {
    let starred_artists = favorites_db::get_starred_ids(state.db(), username, "artist").await?;
    let starred_albums = favorites_db::get_starred_ids(state.db(), username, "album").await?;
    let starred_songs = favorites_db::get_starred_ids(state.db(), username, "song").await?;

    let artists = resolve_items(state, &starred_artists, "getArtist", "artist").await;
    let albums = resolve_items(state, &starred_albums, "getAlbum", "album").await;
    let songs = resolve_items(state, &starred_songs, "getSong", "song").await;

    Ok(json!({
        "starred2": {
            "artist": artists,
            "album": albums,
            "song": songs,
        }
    }))
}

/// Resolve a list of namespaced IDs to full metadata from their owning backends.
async fn resolve_items(
    state: &AppState,
    items: &[(String, String)],
    endpoint: &str,
    response_key: &str,
) -> Vec<serde_json::Value> {
    let mut resolved = Vec::new();

    for (namespaced_id, starred_at) in items {
        let Ok((backend, original_id)) = route_to_backend(state, namespaced_id) else {
            continue;
        };

        match backend.request_json(endpoint, &[("id", &original_id)]).await {
            Ok(mut resp) => {
                resp.namespace_ids(backend.index);
                if let Some(mut item) = resp.get(response_key).cloned() {
                    // Inject starred timestamp
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("starred".into(), json!(starred_at));
                    }
                    resolved.push(item);
                }
            }
            Err(_) => continue,
        }
    }

    resolved
}

pub async fn set_rating(
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
    let rating = params.raw.get("rating").cloned().unwrap_or_else(|| "0".into());

    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("setRating id={} rating={} -> backend={}", id, rating, backend.name);
    backend
        .request_json("setRating", &[("id", &original_id), ("rating", &rating)])
        .await?;

    Ok(SubsonicResponse::empty(params.format))
}

pub async fn scrobble(
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
    debug!("scrobble id={} -> backend={}", id, backend.name);

    let mut extra: Vec<(&str, &str)> = vec![("id", &original_id)];
    if let Some(t) = params.raw.get("time") {
        extra.push(("time", t));
    }
    if let Some(s) = params.raw.get("submission") {
        extra.push(("submission", s));
    }

    backend.request_json("scrobble", &extra).await?;

    // Broadcast now-playing to friends if social is enabled and this is a "now playing" scrobble
    let is_submission = params.raw.get("submission").map(|s| s == "true").unwrap_or(true);
    if !is_submission {
        // submission=false means "now playing" (not a final scrobble)
        if let Some(social) = state.social() {
            // Fetch track info for the broadcast
            if let Ok(mut song_resp) = backend.request_json("getSong", &[("id", &original_id)]).await {
                song_resp.namespace_ids(backend.index);
                if let Some(song) = song_resp.get("song") {
                    social.broadcast_now_playing(song).await;
                }
            }
        }
    }

    Ok(SubsonicResponse::empty(params.format))
}
