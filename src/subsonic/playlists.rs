use axum::extract::State;
use axum::response::IntoResponse;
use tracing::debug;

use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::proxy::router::route_to_backend;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::models::{merge_playlists, NamespaceIds};
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::playlist_db;
use crate::subsonic::response::SubsonicResponse;

/// Merge local (Fugue-owned) playlists with remote backend playlists.
pub async fn get_playlists(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getPlaylists user={}", auth.username);
    // Fetch remote playlists from all backends
    let remote_results = fan_out(state.backends(), "getPlaylists", &[]).await;
    let mut merged = match remote_results {
        Ok(results) => merge_playlists(results),
        Err(_) => serde_json::json!({ "playlists": { "playlist": [] } }),
    };

    // Fetch local playlists
    let local = playlist_db::get_playlists_for_user(state.db(), &auth.username).await?;

    // Append local playlists to the merged list
    if let Some(playlists) = merged
        .get_mut("playlists")
        .and_then(|p| p.get_mut("playlist"))
        .and_then(|p| p.as_array_mut())
    {
        playlists.extend(local);
    }

    Ok(SubsonicResponse::ok(params.format, merged))
}

/// Get a single playlist — local or remote.
pub async fn get_playlist(
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

    // Check if this is a local playlist
    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("getPlaylist id={} -> local uuid={}", id, uuid);
        let playlist = playlist_db::get_playlist(state.db(), &uuid).await?;

        // For local playlists, resolve track metadata from backends
        let mut playlist = playlist;
        if let Some(entries) = playlist
            .get_mut("playlist")
            .and_then(|p| p.get_mut("entry"))
            .and_then(|e| e.as_array_mut())
        {
            let mut resolved = Vec::new();
            for entry in entries.iter() {
                if let Some(track_id) = entry.get("id").and_then(|i| i.as_str()) {
                    // Try to resolve track metadata from the owning backend
                    match route_to_backend(&state, track_id) {
                        Ok((backend, original_id)) => {
                            match backend
                                .request_json("getSong", &[("id", &original_id)])
                                .await
                            {
                                Ok(mut song_resp) => {
                                    song_resp.namespace_ids(backend.index);
                                    if let Some(song) = song_resp.get("song") {
                                        resolved.push(song.clone());
                                        continue;
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                        Err(_) => {}
                    }
                    // Fallback: keep minimal entry
                    resolved.push(entry.clone());
                }
            }
            *entries = resolved;
        }

        return Ok(SubsonicResponse::ok(params.format, playlist));
    }

    // Remote playlist
    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("getPlaylist id={} -> remote backend={}", id, backend.name);
    let mut resp = backend
        .request_json("getPlaylist", &[("id", &original_id)])
        .await?;
    resp.namespace_ids(backend.index);

    Ok(SubsonicResponse::ok(params.format, resp))
}

/// Create a playlist — always stored locally in Fugue.
pub async fn create_playlist(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let name = params
        .raw
        .get("name")
        .cloned()
        .unwrap_or_else(|| "New Playlist".into());

    debug!("createPlaylist name={} user={}", name, auth.username);
    let uuid = playlist_db::create_playlist(state.db(), &name, &auth.username).await?;

    // If songId params were provided, add them
    if let Some(song_id) = params.raw.get("songId") {
        playlist_db::add_tracks_to_playlist(state.db(), &uuid, &[song_id.clone()]).await?;
    }

    let playlist = playlist_db::get_playlist(state.db(), &uuid).await?;
    Ok(SubsonicResponse::ok(params.format, playlist))
}

/// Update a playlist — local or remote.
pub async fn update_playlist(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("playlistId")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: playlistId".into(),
        })?;

    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("updatePlaylist id={} -> local uuid={}", id, uuid);
        // Update local playlist metadata
        playlist_db::update_playlist(
            state.db(),
            &uuid,
            params.raw.get("name").map(|s| s.as_str()),
            params.raw.get("comment").map(|s| s.as_str()),
            params.raw.get("public").map(|s| s == "true"),
        )
        .await?;

        // Add songs if songIdToAdd is present
        if let Some(song_id) = params.raw.get("songIdToAdd") {
            playlist_db::add_tracks_to_playlist(state.db(), &uuid, &[song_id.clone()]).await?;
        }

        // Remove songs if songIndexToRemove is present
        if let Some(idx_str) = params.raw.get("songIndexToRemove") {
            if let Ok(idx) = idx_str.parse::<i64>() {
                playlist_db::remove_tracks_from_playlist(state.db(), &uuid, &[idx]).await?;
            }
        }

        return Ok(SubsonicResponse::empty(params.format));
    }

    // Remote playlist — forward to backend
    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("updatePlaylist id={} -> remote backend={}", id, backend.name);
    let mut extra: Vec<(&str, &str)> = vec![("playlistId", &original_id)];
    if let Some(n) = params.raw.get("name") {
        extra.push(("name", n));
    }
    if let Some(c) = params.raw.get("comment") {
        extra.push(("comment", c));
    }
    if let Some(p) = params.raw.get("public") {
        extra.push(("public", p));
    }

    backend.request_json("updatePlaylist", &extra).await?;
    Ok(SubsonicResponse::empty(params.format))
}

/// Delete a playlist — local or remote.
pub async fn delete_playlist(
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

    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("deletePlaylist id={} -> local uuid={}", id, uuid);
        playlist_db::delete_playlist(state.db(), &uuid).await?;
        return Ok(SubsonicResponse::empty(params.format));
    }

    // Remote playlist
    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("deletePlaylist id={} -> remote backend={}", id, backend.name);
    backend
        .request_json("deletePlaylist", &[("id", &original_id)])
        .await?;

    Ok(SubsonicResponse::empty(params.format))
}
