use axum::extract::State;
use axum::response::IntoResponse;
use serde_json::json;
use tracing::info;

use crate::cache;
use crate::error::FugueError;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::params::SubsonicParams;
use crate::subsonic::response::SubsonicResponse;

pub async fn ping(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    Ok(SubsonicResponse::empty(params.format))
}

pub async fn get_license(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "license": {
                "valid": true,
                "email": "fugue@localhost",
                "licenseExpires": "2099-12-31T23:59:59"
            }
        }),
    ))
}

pub async fn get_user(
    auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    let username = params
        .raw
        .get("username")
        .cloned()
        .unwrap_or_else(|| auth.username.clone());

    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "user": {
                "username": username,
                "email": "",
                "scrobblingEnabled": true,
                "maxBitRate": 0,
                "adminRole": false,
                "settingsRole": false,
                "downloadRole": true,
                "uploadRole": false,
                "playlistRole": true,
                "coverArtRole": true,
                "commentRole": false,
                "podcastRole": false,
                "streamRole": true,
                "jukeboxRole": false,
                "shareRole": false,
                "videoConversionRole": false,
                "folder": [0]
            }
        }),
    ))
}

/// Admin endpoint: trigger a cache refresh on the running server.
/// POST /admin/sync — no Subsonic auth required (internal use only).
pub async fn admin_sync(
    State(state): State<AppState>,
) -> impl IntoResponse {
    info!("admin sync triggered");
    let db = state.db().clone();
    let backends = state.backends().to_vec();

    // Spawn the sync in the background so the HTTP response returns immediately
    tokio::spawn(async move {
        cache::refresh::run_sync(&db, &backends).await;
        info!("admin sync complete");
    });

    axum::Json(json!({ "status": "sync started" }))
}

/// Admin endpoint: return the node's social ticket.
pub async fn admin_ticket(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.iroh() {
        Some(endpoint) => {
            let display_name = state.config().social.display_name.as_str();
            let ticket = crate::social::node::generate_ticket(endpoint, Some(display_name));
            let node_id = endpoint.id().to_string();
            axum::Json(serde_json::json!({
                "ticket": ticket,
                "node_id": node_id,
            }))
        }
        None => {
            axum::Json(serde_json::json!({
                "error": "Social is not enabled. Set [social] enabled = true in config."
            }))
        }
    }
}

/// Admin endpoint: social network status.
pub async fn admin_status(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let social_enabled = state.iroh().is_some();

    let (node_id, addresses, relay) = match state.iroh() {
        Some(endpoint) => {
            let addr = endpoint.addr();
            let relay_url = addr.addrs.iter().find_map(|a| {
                if let iroh_base::TransportAddr::Relay(url) = a {
                    Some(url.to_string())
                } else {
                    None
                }
            });
            let direct_addrs: Vec<String> = addr.addrs.iter().filter_map(|a| {
                if let iroh_base::TransportAddr::Ip(ip) = a {
                    Some(ip.to_string())
                } else {
                    None
                }
            }).collect();
            (
                Some(endpoint.id().to_string()),
                direct_addrs,
                relay_url,
            )
        }
        None => (None, vec![], None),
    };

    // Get friends from DB with health info
    let friends = match crate::social::friends::list_friends(state.db()).await {
        Ok(f) => f,
        Err(_) => vec![],
    };

    let friends_json: Vec<serde_json::Value> = friends
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "node_id": f.public_key,
                "last_seen": f.last_seen,
            })
        })
        .collect();

    // Get cache stats
    let (artists, albums, tracks) = crate::cache::db::cache_stats(state.db())
        .await
        .unwrap_or((0, 0, 0));

    // Get backend health
    let backends_json: Vec<serde_json::Value> = state
        .backends()
        .iter()
        .map(|b| {
            let health = state.health().get(b.index);
            json!({
                "name": b.name,
                "url": b.base_url,
                "available": health.available,
                "latency_ms": health.latency_ms,
                "consecutive_failures": health.consecutive_failures,
            })
        })
        .collect();

    axum::Json(json!({
        "social": {
            "enabled": social_enabled,
            "node_id": node_id,
            "relay": relay,
            "direct_addresses": addresses,
            "friends": friends_json,
        },
        "cache": {
            "artists": artists,
            "albums": albums,
            "tracks": tracks,
        },
        "backends": backends_json,
    }))
}

/// Admin endpoint: reload friends and register their addresses with Iroh.
pub async fn admin_refresh_friends(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let friends = crate::social::friends::list_friends(state.db()).await.unwrap_or_default();
    let memory_lookup = iroh::address_lookup::MemoryLookup::default();
    let mut registered = 0;

    for f in &friends {
        if let Ok(addr) = crate::social::node::parse_ticket(&f.ticket) {
            memory_lookup.add_endpoint_info(addr);
            registered += 1;
        }
    }

    endpoint.address_lookup().add(memory_lookup);
    info!("admin: refreshed {} friend addresses", registered);

    axum::Json(json!({
        "status": "ok",
        "friends_registered": registered,
    }))
}

/// Admin endpoint: broadcast a full sync for a collaborative playlist.
pub async fn admin_playlist_sync(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let playlist_id = match params.get("id") {
        Some(id) => id.clone(),
        None => return axum::Json(json!({ "error": "Missing ?id= parameter" })),
    };

    if let Some(social) = state.social() {
        match crate::social::collab_playlist::get_all_tracks(state.db(), &playlist_id).await {
            Ok(tracks) => {
                let name_row: Option<(String,)> = sqlx::query_as(
                    "SELECT name FROM collab_playlists WHERE id = ?",
                )
                .bind(&playlist_id)
                .fetch_optional(state.db())
                .await
                .unwrap_or(None);

                let name = name_row.map(|(n,)| n).unwrap_or_else(|| "Unknown".into());

                social.broadcast_playlist_op(
                    crate::social::collab_playlist::PlaylistOp::FullSync {
                        playlist_id: playlist_id.clone(),
                        name,
                        tracks,
                    },
                ).await;

                info!("admin: broadcast playlist sync for {}", playlist_id);
                axum::Json(json!({ "status": "sync broadcast sent" }))
            }
            Err(e) => axum::Json(json!({ "error": e.to_string() })),
        }
    } else {
        axum::Json(json!({ "error": "Social not enabled" }))
    }
}

/// Admin endpoint: join a collaborative playlist via invite code.
pub async fn admin_playlist_join(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = match params.get("code") {
        Some(c) => c.clone(),
        None => return axum::Json(json!({ "error": "Missing ?code= parameter" })),
    };

    let invite = match crate::social::collab_playlist::parse_invite(&code) {
        Some(parsed) => parsed,
        None => return axum::Json(json!({ "error": "Invalid invite code" })),
    };

    let node_id = state.node_id().unwrap_or_else(|| "local".into());
    let display_name = state.config().social.display_name.clone();
    let db = state.db();

    // Auto-add sender as friend if invite includes their ticket
    if let (Some(ref sender_name), Some(ref sender_ticket)) = (&invite.sender_name, &invite.sender_ticket) {
        if !sender_name.is_empty() && !sender_ticket.is_empty() {
            let public_key = match crate::social::node::parse_ticket(sender_ticket) {
                Ok(addr) => addr.id.to_string(),
                Err(_) => sender_ticket.clone(),
            };
            // Only add if not already a friend
            let existing = crate::social::friends::list_friends(db).await.unwrap_or_default();
            if !existing.iter().any(|f| f.public_key == public_key) {
                let _ = crate::social::friends::add_friend(db, sender_name, &public_key, sender_ticket).await;
                info!("admin: auto-added friend '{}' from invite code", sender_name);
                // Refresh addresses so Iroh can connect immediately
                if let Some(endpoint) = state.iroh() {
                    crate::social::service::refresh_friend_addresses_now(db, endpoint).await;
                }
            }
        }
    }

    // Create playlist if it doesn't exist yet
    let exists: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM collab_playlists WHERE id = ?",
    )
    .bind(&invite.playlist_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    if exists.is_none() {
        if let Err(e) = crate::social::collab_playlist::create_playlist(db, &invite.playlist_id, &invite.name, "friend").await {
            return axum::Json(json!({ "error": format!("Failed to create playlist: {e}") }));
        }
    } else {
        let _ = crate::social::collab_playlist::rename_playlist(db, &invite.playlist_id, &invite.name).await;
    }

    // Add this node as a member with the invite role
    if let Err(e) = crate::social::collab_playlist::add_member(db, &invite.playlist_id, &node_id, &display_name, invite.role).await {
        return axum::Json(json!({ "error": format!("Failed to join: {e}") }));
    }

    let encoded_id = crate::social::collab_playlist::encode_collab_id(&invite.playlist_id);
    let role_str = invite.role.as_str();

    info!("admin: joined collaborative playlist '{}' as {}", invite.name, role_str);

    axum::Json(json!({
        "status": "ok",
        "playlist_name": invite.name,
        "role": role_str,
        "playlist_id": encoded_id,
    }))
}

/// Admin endpoint: generate an invite code for a collaborative playlist.
pub async fn admin_playlist_invite(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let id = match params.get("id") {
        Some(id) => id.clone(),
        None => return axum::Json(json!({ "error": "Missing ?id= parameter" })),
    };
    let role_str = params.get("role").map(|s| s.as_str()).unwrap_or("collab");

    // Decode collab ID to get raw UUID
    let uuid = match crate::social::collab_playlist::decode_collab_id(&id) {
        Some(uuid) => uuid,
        None => id.clone(), // Allow raw UUID too
    };

    let role = match crate::social::collab_playlist::Role::from_str(role_str) {
        Some(r) => r,
        None => return axum::Json(json!({ "error": format!("Invalid role: {role_str}") })),
    };

    // Get playlist name
    let name_row: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM collab_playlists WHERE id = ?",
    )
    .bind(&uuid)
    .fetch_optional(state.db())
    .await
    .unwrap_or(None);

    let name = match name_row {
        Some((n,)) => n,
        None => return axum::Json(json!({ "error": "Playlist not found" })),
    };

    // Include sender's ticket so the recipient can auto-add us as a friend
    let sender_name = state.config().social.display_name.clone();
    let sender_ticket = state.iroh()
        .map(|ep| crate::social::node::generate_ticket(ep, None))
        .unwrap_or_default();

    let code = crate::social::collab_playlist::generate_invite(&uuid, role, &name, &sender_name, &sender_ticket);

    axum::Json(json!({
        "status": "ok",
        "code": code,
        "playlist_name": name,
        "role": role_str,
    }))
}

/// Admin endpoint: generate this node's friend code for sharing.
pub async fn admin_friend_code(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.iroh() {
        Some(endpoint) => {
            let display_name = state.config().social.display_name.clone();
            let ticket = crate::social::node::generate_ticket(endpoint, None);
            let code = crate::social::collab_playlist::generate_friend_code(&display_name, &ticket);
            axum::Json(json!({
                "status": "ok",
                "code": code,
                "display_name": display_name,
                "node_id": endpoint.id().to_string(),
            }))
        }
        None => {
            axum::Json(json!({ "error": "Social is not enabled" }))
        }
    }
}

/// Admin endpoint: add a friend via friend code.
pub async fn admin_friend_add(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = match params.get("code") {
        Some(c) => c.clone(),
        None => return axum::Json(json!({ "error": "Missing ?code= parameter" })),
    };

    let (friend_name, ticket) = match crate::social::collab_playlist::parse_friend_code(&code) {
        Some(parsed) => parsed,
        None => return axum::Json(json!({ "error": "Invalid friend code" })),
    };

    let public_key = match crate::social::node::parse_ticket(&ticket) {
        Ok(addr) => addr.id.to_string(),
        Err(_) => return axum::Json(json!({ "error": "Invalid ticket in friend code" })),
    };

    let db = state.db();
    if let Err(e) = crate::social::friends::add_friend(db, &friend_name, &public_key, &ticket).await {
        return axum::Json(json!({ "error": format!("Failed to add friend: {e}") }));
    }

    // Refresh addresses so Iroh connects immediately
    if let Some(endpoint) = state.iroh() {
        crate::social::service::refresh_friend_addresses_now(db, endpoint).await;
    }

    info!("admin: added friend '{}' ({})", friend_name, public_key);

    axum::Json(json!({
        "status": "ok",
        "friend_name": friend_name,
        "node_id": public_key,
    }))
}

/// Admin endpoint: list all friends.
pub async fn admin_friends(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let friends = crate::social::friends::list_friends(state.db()).await.unwrap_or_default();
    let list: Vec<serde_json::Value> = friends.iter().map(|f| {
        json!({
            "name": f.name,
            "node_id": f.public_key,
            "last_seen": f.last_seen,
        })
    }).collect();

    axum::Json(json!({ "friends": list }))
}

/// Admin endpoint: friends with activity (now playing).
pub async fn admin_activity(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let friends = crate::social::friends::list_friends(state.db()).await.unwrap_or_default();
    let now_playing = crate::social::activity::get_now_playing(state.db()).await.unwrap_or_default();

    let friend_list: Vec<serde_json::Value> = friends.iter().map(|f| {
        // Find now-playing entry for this friend
        let playing = now_playing.iter().find(|np| {
            np.get("nodeId").and_then(|n| n.as_str()) == Some(&f.public_key)
        });
        json!({
            "name": f.name,
            "node_id": f.public_key,
            "last_seen": f.last_seen,
            "now_playing": playing,
        })
    }).collect();

    axum::Json(json!({
        "friends": friend_list,
        "display_name": state.config().social.display_name,
    }))
}

pub async fn get_open_subsonic_extensions(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "openSubsonicExtensions": [
                { "name": "formPost", "versions": [1] },
                { "name": "songLyrics", "versions": [1] },
                { "name": "transcodeOffset", "versions": [1] },
                { "name": "playbackReport", "versions": [1] },
                { "name": "apiKeyAuthentication", "versions": [1] }
            ]
        }),
    ))
}

pub async fn get_scan_status(
    _auth: AuthenticatedUser,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    Ok(SubsonicResponse::ok(
        params.format,
        json!({
            "scanStatus": {
                "scanning": false,
                "count": 0
            }
        }),
    ))
}
