//! Additional Subsonic endpoints that clients may call.
//! These are either proxied to backends or return sensible empty responses.

use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::debug;

use crate::error::FugueError;
use crate::proxy::fanout::fan_out;
use crate::proxy::router::route_to_backend;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::models::NamespaceIds;
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn get_similar_songs(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    let count: usize = params.raw.get("count").and_then(|v| v.parse().ok()).unwrap_or(50);

    debug!("getSimilarSongs id={} count={}", id, count);

    // Route to the owning backend
    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            let count_str = count.to_string();
            match backend
                .request_json("getSimilarSongs", &[("id", &original_id), ("count", &count_str)])
                .await
            {
                Ok(mut resp) => {
                    resp.namespace_ids(backend.index);
                    Ok(SubsonicResponse::ok(params.format, resp))
                }
                Err(_) => Ok(SubsonicResponse::ok(
                    params.format,
                    json!({ "similarSongs": { "song": [] } }),
                )),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(
            params.format,
            json!({ "similarSongs": { "song": [] } }),
        )),
    }
}

pub async fn get_similar_songs2(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    let count: usize = params.raw.get("count").and_then(|v| v.parse().ok()).unwrap_or(50);

    debug!("getSimilarSongs2 id={} count={}", id, count);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            let count_str = count.to_string();
            match backend
                .request_json("getSimilarSongs2", &[("id", &original_id), ("count", &count_str)])
                .await
            {
                Ok(mut resp) => {
                    resp.namespace_ids(backend.index);
                    Ok(SubsonicResponse::ok(params.format, resp))
                }
                Err(_) => Ok(SubsonicResponse::ok(
                    params.format,
                    json!({ "similarSongs2": { "song": [] } }),
                )),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(
            params.format,
            json!({ "similarSongs2": { "song": [] } }),
        )),
    }
}

pub async fn get_top_songs(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let artist = params.raw.get("artist").cloned().unwrap_or_default();
    let count: usize = params.raw.get("count").and_then(|v| v.parse().ok()).unwrap_or(50);

    debug!("getTopSongs artist={} count={}", artist, count);

    // Try all backends and merge
    let count_str = count.to_string();
    match fan_out(state.backends(), "getTopSongs", &[("artist", &artist), ("count", &count_str)]).await {
        Ok(results) => {
            let mut all_songs = Vec::new();
            for (backend_idx, mut resp) in results {
                resp.namespace_ids(backend_idx);
                if let Some(songs) = resp.get("topSongs").and_then(|t| t.get("song")).and_then(|s| s.as_array()) {
                    all_songs.extend(songs.clone());
                }
            }
            all_songs.truncate(count);
            Ok(SubsonicResponse::ok(
                params.format,
                json!({ "topSongs": { "song": all_songs } }),
            ))
        }
        Err(_) => Ok(SubsonicResponse::ok(
            params.format,
            json!({ "topSongs": { "song": [] } }),
        )),
    }
}

pub async fn get_now_playing(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getNowPlaying");
    let mut entries = crate::social::activity::get_now_playing(state.db()).await?;

    // Enrich with playback position from reports
    for entry in &mut entries {
        if let Some(media_id) = entry.get("id").and_then(|i| i.as_str()) {
            let node_id = entry.get("nodeId").and_then(|n| n.as_str()).unwrap_or("");
            let user = entry.get("username").and_then(|u| u.as_str()).unwrap_or("");
            let report: Option<(i64, String)> = sqlx::query_as(
                "SELECT position_ms, state FROM playback_reports
                 WHERE node_id = ? AND user_name = ? AND media_id = ?",
            )
            .bind(node_id)
            .bind(user)
            .bind(media_id)
            .fetch_optional(state.db())
            .await
            .unwrap_or(None);

            if let Some((pos, play_state)) = report {
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("playerPosition".into(), json!(pos));
                    obj.insert("playbackState".into(), json!(play_state));
                }
            }
        }
    }

    Ok(SubsonicResponse::ok(
        params.format,
        json!({ "nowPlaying": { "entry": entries } }),
    ))
}

pub async fn report_playback(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let media_id = params.raw.get("mediaId").or(params.raw.get("id"))
        .ok_or_else(|| FugueError::missing("mediaId"))?;
    let position_ms: i64 = params.raw.get("positionMs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let play_state = params.raw.get("state").cloned().unwrap_or_else(|| "playing".into());
    let ignore_scrobble = params.raw.get("ignoreScrobble")
        .map(|v| v == "true")
        .unwrap_or(false);

    debug!("reportPlayback user={} media={} pos={}ms state={}", auth.username, media_id, position_ms, play_state);

    let node_id = state.node_id().unwrap_or_else(|| "local".into());

    sqlx::query(
        "INSERT INTO playback_reports (user_name, node_id, media_id, position_ms, state, updated_at)
         VALUES (?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(node_id, user_name, media_id) DO UPDATE SET
           position_ms = excluded.position_ms, state = excluded.state, updated_at = excluded.updated_at",
    )
    .bind(&auth.username)
    .bind(&node_id)
    .bind(media_id)
    .bind(position_ms)
    .bind(&play_state)
    .execute(state.db())
    .await?;

    // Broadcast to friends if social is enabled and not just a position update
    if !ignore_scrobble {
        if let Some(social) = state.social() {
            // Resolve track metadata for the now playing broadcast
            if let Ok((backend, original_id)) = crate::proxy::router::route_to_backend(&state, media_id) {
                if let Ok(mut resp) = backend.request_json("getSong", &[("id", &original_id)]).await {
                    resp.namespace_ids(backend.index);
                    if let Some(song) = resp.get("song") {
                        let mut track = song.clone();
                        if let Some(obj) = track.as_object_mut() {
                            obj.insert("playerPosition".into(), json!(position_ms));
                        }
                        social.broadcast_now_playing(&track).await;
                    }
                }
            }
        }
    }

    Ok(SubsonicResponse::empty(params.format))
}

pub async fn get_bookmarks(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getBookmarks");
    Ok(SubsonicResponse::ok(
        params.format,
        json!({ "bookmarks": { "bookmark": [] } }),
    ))
}

pub async fn create_bookmark(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("createBookmark");
    Ok(SubsonicResponse::empty(params.format))
}

pub async fn delete_bookmark(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("deleteBookmark");
    Ok(SubsonicResponse::empty(params.format))
}

pub async fn get_play_queue(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getPlayQueue");
    // Return empty — play queue is client-side
    Ok(SubsonicResponse::ok(params.format, json!({})))
}

pub async fn save_play_queue(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("savePlayQueue");
    Ok(SubsonicResponse::empty(params.format))
}

pub async fn get_internet_radio_stations(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    debug!("getInternetRadioStations");
    Ok(SubsonicResponse::ok(
        params.format,
        json!({ "internetRadioStations": { "internetRadioStation": [] } }),
    ))
}

pub async fn get_lyrics(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let artist = params.raw.get("artist").cloned().unwrap_or_default();
    let title = params.raw.get("title").cloned().unwrap_or_default();

    debug!("getLyrics artist={} title={}", artist, title);

    // Try each backend until one returns lyrics
    for backend in state.backends() {
        if let Ok(resp) = backend
            .request_json("getLyrics", &[("artist", &artist), ("title", &title)])
            .await
        {
            if resp.get("lyrics").is_some() {
                return Ok(SubsonicResponse::ok(params.format, resp));
            }
        }
    }

    Ok(SubsonicResponse::ok(
        params.format,
        json!({ "lyrics": {} }),
    ))
}

pub async fn get_lyrics_by_song_id(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    debug!("getLyricsBySongId id={}", id);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            match backend.request_json("getLyricsBySongId", &[("id", &original_id)]).await {
                Ok(resp) => Ok(SubsonicResponse::ok(params.format, resp)),
                Err(_) => Ok(SubsonicResponse::ok(
                    params.format,
                    json!({ "lyricsList": { "structuredLyrics": [] } }),
                )),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(
            params.format,
            json!({ "lyricsList": { "structuredLyrics": [] } }),
        )),
    }
}

pub async fn get_album_info(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    debug!("getAlbumInfo id={}", id);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            match backend.request_json("getAlbumInfo", &[("id", &original_id)]).await {
                Ok(resp) => Ok(SubsonicResponse::ok(params.format, resp)),
                Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "albumInfo": {} }))),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "albumInfo": {} }))),
    }
}

pub async fn get_album_info2(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    debug!("getAlbumInfo2 id={}", id);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            match backend.request_json("getAlbumInfo2", &[("id", &original_id)]).await {
                Ok(resp) => Ok(SubsonicResponse::ok(params.format, resp)),
                Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "albumInfo": {} }))),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "albumInfo": {} }))),
    }
}

pub async fn get_artist_info(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    debug!("getArtistInfo id={}", id);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            match backend.request_json("getArtistInfo", &[("id", &original_id)]).await {
                Ok(mut resp) => {
                    resp.namespace_ids(backend.index);
                    Ok(SubsonicResponse::ok(params.format, resp))
                }
                Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "artistInfo": {} }))),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "artistInfo": {} }))),
    }
}

pub async fn get_artist_info2(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let id = params.raw.get("id").ok_or_else(|| FugueError::missing("id"))?;
    debug!("getArtistInfo2 id={}", id);

    match route_to_backend(&state, id) {
        Ok((backend, original_id)) => {
            match backend.request_json("getArtistInfo2", &[("id", &original_id)]).await {
                Ok(mut resp) => {
                    resp.namespace_ids(backend.index);
                    Ok(SubsonicResponse::ok(params.format, resp))
                }
                Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "artistInfo2": {} }))),
            }
        }
        Err(_) => Ok(SubsonicResponse::ok(params.format, json!({ "artistInfo2": {} }))),
    }
}

pub async fn get_chat_messages(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let since: u64 = params.raw.get("since").and_then(|v| v.parse().ok()).unwrap_or(3600);
    debug!("getChatMessages since={}s", since);

    let messages = crate::social::activity::get_chat_messages(state.db(), since).await?;

    // Convert to Subsonic chatMessage format
    let chat_messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            json!({
                "username": m.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                "time": m.get("time").and_then(|v| v.as_str()).unwrap_or(""),
                "message": m.get("message").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();

    Ok(SubsonicResponse::ok(
        params.format,
        json!({ "chatMessages": { "chatMessage": chat_messages } }),
    ))
}

pub async fn add_chat_message(
    auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let message = params
        .raw
        .get("message")
        .ok_or_else(|| FugueError::missing("message"))?;

    debug!("addChatMessage user={} message={}", auth.username, message);

    // Store locally
    let node_id = state.node_id().unwrap_or_else(|| "local".into());
    crate::social::activity::add_chat_message(
        state.db(),
        &node_id,
        &auth.username,
        message,
    )
    .await?;

    // Broadcast to friends if social is enabled
    if let Some(social) = state.social() {
        social.broadcast_chat(message).await;
    }

    Ok(SubsonicResponse::empty(params.format))
}
