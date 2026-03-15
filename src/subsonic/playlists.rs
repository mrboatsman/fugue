use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::debug;

use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::proxy::router::route_to_backend;
use crate::social::collab_playlist::{self, CollabTrack, PlaylistOp};
use crate::social::crdt::{self, CrdtOp, CrdtOpKind};
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::models::{merge_playlists, NamespaceIds};
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::playlist_db;
use crate::subsonic::response::SubsonicResponse;

/// Merge local + remote + collaborative playlists.
pub async fn get_playlists(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getPlaylists user={}", auth.username);

    let remote_results = fan_out(state.backends(), "getPlaylists", &[]).await;
    let mut merged = match remote_results {
        Ok(results) => merge_playlists(results),
        Err(_) => json!({ "playlists": { "playlist": [] } }),
    };

    // Local playlists
    let local = playlist_db::get_playlists_for_user(state.db(), &auth.username).await?;

    // Collaborative playlists
    let collab = collab_playlist::list_playlists(state.db()).await?;

    if let Some(playlists) = merged
        .get_mut("playlists")
        .and_then(|p| p.get_mut("playlist"))
        .and_then(|p| p.as_array_mut())
    {
        playlists.extend(local);
        playlists.extend(collab);
    }

    Ok(SubsonicResponse::ok(params.format, merged))
}

/// Get a single playlist — local, remote, or collaborative.
pub async fn get_playlist(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::missing("id"))?;

    // Check collaborative playlist
    if let Some(uuid) = collab_playlist::decode_collab_id(id) {
        debug!("getPlaylist id={} -> collab uuid={}", id, uuid);
        match collab_playlist::get_playlist(state.db(), &uuid).await? {
            Some(playlist) => return Ok(SubsonicResponse::ok(params.format, playlist)),
            None => return Err(FugueError::NotFound("Playlist not found".into())),
        }
    }

    // Check local playlist
    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("getPlaylist id={} -> local uuid={}", id, uuid);
        let playlist = playlist_db::get_playlist(state.db(), &uuid).await?;

        let mut playlist = playlist;
        if let Some(entries) = playlist
            .get_mut("playlist")
            .and_then(|p| p.get_mut("entry"))
            .and_then(|e| e.as_array_mut())
        {
            let mut resolved = Vec::new();
            for entry in entries.iter() {
                if let Some(track_id) = entry.get("id").and_then(|i| i.as_str()) {
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

/// Create a playlist. If name starts with "collab:" it creates a collaborative playlist.
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

    // Collaborative playlist: name starts with "collab:" or param collaborative=true
    let is_collab = name.starts_with("collab:")
        || params.raw.get("collaborative").map(|v| v == "true").unwrap_or(false);

    if is_collab {
        let clean_name = name.strip_prefix("collab:").unwrap_or(&name).trim().to_string();
        let playlist_id = format!("{:032x}", rand::random::<u128>());
        let node_id = state.node_id().unwrap_or_else(|| "local".into());

        debug!("createPlaylist collab name={} id={}", clean_name, playlist_id);

        collab_playlist::create_playlist(state.db(), &playlist_id, &clean_name, &node_id).await?;

        // Store CRDT create op
        let ts = crdt::next_timestamp(state.db(), &playlist_id, &node_id).await?;
        let op = CrdtOp {
            op_id: format!("{}:{}", node_id, ts),
            timestamp: ts,
            origin_node: node_id.clone(),
            kind: CrdtOpKind::SetName { name: clean_name.clone() },
        };
        crdt::store_op(state.db(), &playlist_id, &op).await?;

        // Broadcast CRDT op
        if let Some(social) = state.social() {
            let msg = crate::social::protocol::GossipMessage::CrdtSync {
                playlist_id: playlist_id.clone(),
                ops: vec![op],
            };
            let sender = social.sender().await;
            let _ = sender.broadcast(msg.to_bytes()).await;
        }

        let encoded_id = collab_playlist::encode_collab_id(&playlist_id);
        return Ok(SubsonicResponse::ok(
            params.format,
            json!({
                "playlist": {
                    "id": encoded_id,
                    "name": format!("[Collab] {}", clean_name),
                    "songCount": 0,
                    "duration": 0,
                    "public": true,
                    "owner": node_id,
                }
            }),
        ));
    }

    // Regular local playlist
    debug!("createPlaylist name={} user={}", name, auth.username);
    let uuid = playlist_db::create_playlist(state.db(), &name, &auth.username).await?;

    if let Some(song_id) = params.raw.get("songId") {
        playlist_db::add_tracks_to_playlist(state.db(), &uuid, &[song_id.clone()]).await?;
    }

    let playlist = playlist_db::get_playlist(state.db(), &uuid).await?;
    Ok(SubsonicResponse::ok(params.format, playlist))
}

/// Update a playlist — local, remote, or collaborative.
pub async fn update_playlist(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("playlistId")
        .ok_or_else(|| FugueError::missing("playlistId"))?;

    // Collaborative playlist — uses CRDT operations
    if let Some(uuid) = collab_playlist::decode_collab_id(id) {
        debug!("updatePlaylist id={} -> collab uuid={}", id, uuid);
        let node_id = state.node_id().unwrap_or_else(|| "local".into());
        let mut ops_to_broadcast: Vec<CrdtOp> = Vec::new();

        if let Some(name) = params.raw.get("name") {
            let ts = crdt::next_timestamp(state.db(), &uuid, &node_id).await?;
            let op = CrdtOp {
                op_id: format!("{}:{}", node_id, ts),
                timestamp: ts,
                origin_node: node_id.clone(),
                kind: CrdtOpKind::SetName { name: name.clone() },
            };
            crdt::store_op(state.db(), &uuid, &op).await?;
            ops_to_broadcast.push(op);
        }

        if let Some(song_id) = params.raw.get("songIdToAdd") {
            let track = resolve_track_for_collab(&state, song_id, &node_id).await;
            let ts = crdt::next_timestamp(state.db(), &uuid, &node_id).await?;
            let op = CrdtOp {
                op_id: format!("{}:{}", node_id, ts),
                timestamp: ts,
                origin_node: node_id.clone(),
                kind: CrdtOpKind::AddTrack { track },
            };
            crdt::store_op(state.db(), &uuid, &op).await?;
            ops_to_broadcast.push(op);
        }

        if let Some(idx_str) = params.raw.get("songIndexToRemove") {
            if let Ok(idx) = idx_str.parse::<i64>() {
                let tracks = collab_playlist::get_all_tracks(state.db(), &uuid).await?;
                if let Some(track) = tracks.get(idx as usize) {
                    let ts = crdt::next_timestamp(state.db(), &uuid, &node_id).await?;
                    let op = CrdtOp {
                        op_id: format!("{}:{}", node_id, ts),
                        timestamp: ts,
                        origin_node: node_id.clone(),
                        kind: CrdtOpKind::RemoveTrack {
                            track_id: track.track_id.clone(),
                            owner_node: track.owner_node.clone(),
                        },
                    };
                    crdt::store_op(state.db(), &uuid, &op).await?;
                    ops_to_broadcast.push(op);
                }
            }
        }

        // Rebuild materialized view from op log
        crdt::rebuild_playlist(state.db(), &uuid).await?;

        // Broadcast ops to friends
        if !ops_to_broadcast.is_empty() {
            if let Some(social) = state.social() {
                let msg = crate::social::protocol::GossipMessage::CrdtSync {
                    playlist_id: uuid.clone(),
                    ops: ops_to_broadcast,
                };
                let sender = social.sender().await;
                let _ = sender.broadcast(msg.to_bytes()).await;
            }
        }

        return Ok(SubsonicResponse::empty(params.format));
    }

    // Local playlist
    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("updatePlaylist id={} -> local uuid={}", id, uuid);
        playlist_db::update_playlist(
            state.db(),
            &uuid,
            params.raw.get("name").map(|s| s.as_str()),
            params.raw.get("comment").map(|s| s.as_str()),
            params.raw.get("public").map(|s| s == "true"),
        )
        .await?;

        if let Some(song_id) = params.raw.get("songIdToAdd") {
            playlist_db::add_tracks_to_playlist(state.db(), &uuid, &[song_id.clone()]).await?;
        }
        if let Some(idx_str) = params.raw.get("songIndexToRemove") {
            if let Ok(idx) = idx_str.parse::<i64>() {
                playlist_db::remove_tracks_from_playlist(state.db(), &uuid, &[idx]).await?;
            }
        }

        return Ok(SubsonicResponse::empty(params.format));
    }

    // Remote playlist
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

/// Delete a playlist — local, remote, or collaborative.
pub async fn delete_playlist(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::missing("id"))?;

    // Collaborative
    if let Some(uuid) = collab_playlist::decode_collab_id(id) {
        debug!("deletePlaylist id={} -> collab uuid={}", id, uuid);
        if let Some(social) = state.social() {
            social.broadcast_playlist_op(PlaylistOp::Delete {
                playlist_id: uuid.clone(),
            }).await;
        }
        collab_playlist::delete_playlist(state.db(), &uuid).await?;
        return Ok(SubsonicResponse::empty(params.format));
    }

    // Local
    if let Some(uuid) = playlist_db::decode_local_playlist_id(id) {
        debug!("deletePlaylist id={} -> local uuid={}", id, uuid);
        playlist_db::delete_playlist(state.db(), &uuid).await?;
        return Ok(SubsonicResponse::empty(params.format));
    }

    // Remote
    let (backend, original_id) = route_to_backend(&state, id)?;
    debug!("deletePlaylist id={} -> remote backend={}", id, backend.name);
    backend
        .request_json("deletePlaylist", &[("id", &original_id)])
        .await?;

    Ok(SubsonicResponse::empty(params.format))
}

/// Broadcast a full sync of a collaborative playlist to all friends.
/// This is called after every change to ensure all nodes converge.
/// If gossip isn't connected, the broadcast is a no-op — the next
/// NeighborUp event will trigger a full sync automatically.
async fn broadcast_playlist_full_sync(state: &AppState, playlist_id: &str) {
    let Some(social) = state.social() else { return };

    let name = sqlx::query_as::<_, (String,)>(
        "SELECT name FROM collab_playlists WHERE id = ?",
    )
    .bind(playlist_id)
    .fetch_optional(state.db())
    .await
    .ok()
    .flatten()
    .map(|(n,)| n)
    .unwrap_or_else(|| "Unknown".into());

    let tracks = collab_playlist::get_all_tracks(state.db(), playlist_id)
        .await
        .unwrap_or_default();

    social.broadcast_playlist_op(PlaylistOp::FullSync {
        playlist_id: playlist_id.to_string(),
        name,
        tracks,
    }).await;

    debug!("broadcast full sync for collab playlist {}", playlist_id);
}

/// Resolve a track's metadata for inclusion in a collaborative playlist.
async fn resolve_track_for_collab(state: &AppState, track_id: &str, node_id: &str) -> CollabTrack {
    // Try to get full metadata from the backend
    if let Ok((backend, original_id)) = route_to_backend(state, track_id) {
        if let Ok(resp) = backend.request_json("getSong", &[("id", &original_id)]).await {
            if let Some(song) = resp.get("song") {
                return CollabTrack {
                    track_id: track_id.to_string(),
                    owner_node: node_id.to_string(),
                    title: song.get("title").and_then(|t| t.as_str()).unwrap_or("Unknown").to_string(),
                    artist: song.get("artist").and_then(|a| a.as_str()).map(|s| s.to_string()),
                    album: song.get("album").and_then(|a| a.as_str()).map(|s| s.to_string()),
                    duration: song.get("duration").and_then(|d| d.as_i64()),
                    added_by: node_id.to_string(),
                };
            }
        }
    }

    // Fallback: minimal info
    CollabTrack {
        track_id: track_id.to_string(),
        owner_node: node_id.to_string(),
        title: "Unknown Track".to_string(),
        artist: None,
        album: None,
        duration: None,
        added_by: node_id.to_string(),
    }
}
