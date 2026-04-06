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
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<impl IntoResponse, FugueError> {
    // If the client is naviamp (Moosic) and Iroh P2P is enabled, include
    // the ticket in the ping response so the client can auto-upgrade from
    // HTTP to Iroh QUIC on subsequent connections.
    if params.client == "naviamp" {
        if let Some(endpoint) = state.iroh() {
            let display_name = state.config().social.display_name.as_str();
            let ticket = crate::social::node::generate_ticket(endpoint, Some(display_name));
            return Ok(SubsonicResponse::ok(
                params.format,
                json!({ "irohTicket": ticket }),
            ));
        }
    }
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

// ── Party mode admin endpoints ──────────────────────────────────

/// POST /admin/party-create — create a new party session.
pub async fn admin_party_create(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Iroh is not configured" }));
    };
    let node_id = endpoint.id().to_string();
    let display_name = state.config().social.display_name.clone();

    let session_id = {
        let mut party = social.party.write().await;
        let session = party.create_session(display_name.clone(), node_id.clone());
        session.session_id.clone()
    };

    social.broadcast_party_create(&session_id, &node_id).await;

    axum::Json(json!({
        "ok": true,
        "session_id": session_id,
        "host": display_name,
    }))
}

/// POST /admin/party-join?session_id=X&host_node_id=Y&host_name=Z
pub async fn admin_party_join(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Iroh is not configured" }));
    };

    let session_id = match params.get("session_id") {
        Some(s) => s.clone(),
        None => return axum::Json(json!({ "error": "Missing session_id" })),
    };
    let host_node_id = params.get("host_node_id").cloned().unwrap_or_default();
    let host_name = params.get("host_name").cloned().unwrap_or_default();
    let node_id = endpoint.id().to_string();

    {
        let mut party = social.party.write().await;
        party.follow(session_id.clone(), host_node_id.clone(), host_name.clone());
    }

    social.broadcast_party_join(&session_id, &node_id).await;

    // Start direct-poll background task that queries the host's Fugue
    // every 3s via QUIC, bypassing gossip for reliable party sync.
    social.start_follow_poll(session_id.clone(), host_node_id);

    axum::Json(json!({
        "ok": true,
        "session_id": session_id,
        "host_name": host_name,
    }))
}

/// POST /admin/party-leave
pub async fn admin_party_leave(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Iroh is not configured" }));
    };
    let node_id = endpoint.id().to_string();

    social.stop_follow_poll();

    let session_id = {
        let mut party = social.party.write().await;
        party.unfollow()
    };

    if let Some(sid) = session_id {
        social.broadcast_party_leave(&sid, &node_id).await;
        axum::Json(json!({ "ok": true, "left_session": sid }))
    } else {
        axum::Json(json!({ "error": "Not in a party session" }))
    }
}

/// POST /admin/party-sync — host broadcasts current playback state.
/// Query params: state=Playing|Paused|Stopped, position_secs=f64,
///   song_id, title, artist, album, track_number, duration_secs, fingerprint (track fields)
pub async fn admin_party_sync(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let mut party = social.party.write().await;
    let Some(ref mut session) = party.hosting else {
        info!("admin/party-sync called but not hosting");
        return axum::Json(json!({ "error": "Not hosting a party" }));
    };
    info!("admin/party-sync: state={:?} pos={}", params.get("state"), params.get("position_secs").unwrap_or(&"?".into()));

    let playback_state = match params.get("state").map(|s| s.as_str()) {
        Some("Playing") => crate::social::protocol::PartyPlaybackState::Playing,
        Some("Paused") => crate::social::protocol::PartyPlaybackState::Paused,
        _ => crate::social::protocol::PartyPlaybackState::Stopped,
    };
    let position_secs: f64 = params
        .get("position_secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    // DJ's wall-clock timestamp when position was read (for end-to-end extrapolation)
    let dj_timestamp_ms: u64 = params
        .get("dj_timestamp_ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Build track from query params (if song_id is present)
    let track = params.get("song_id").map(|song_id| {
        crate::social::protocol::PartyTrack {
            fingerprint: params.get("fingerprint").cloned(),
            song_id: song_id.clone(),
            title: params.get("title").cloned().unwrap_or_default(),
            artist: params.get("artist").cloned().unwrap_or_default(),
            album: params.get("album").cloned().unwrap_or_default(),
            track_number: params.get("track_number").and_then(|s| s.parse().ok()),
            duration_secs: params.get("duration_secs").and_then(|s| s.parse().ok()),
        }
    });

    session.seq += 1;
    session.state = playback_state;
    session.position_secs = position_secs;
    session.dj_timestamp_ms = dj_timestamp_ms;
    session.track = track.clone();

    let seq = session.seq;
    let session_id = session.session_id.clone();
    drop(party);

    // Push to local event channel so subscribers (including the follower's
    // Fugue connected via admin/events) get instant notification.
    social.push_party_sync_event(&session_id, seq, playback_state, track.as_ref(), position_secs, dj_timestamp_ms);

    social
        .broadcast_party_sync(&session_id, seq, playback_state, track, position_secs)
        .await;

    axum::Json(json!({ "ok": true, "seq": seq }))
}

/// POST /admin/party-queue-sync — DJ broadcasts playlist + queue state.
/// Params: playlist_json (JSON-encoded Vec<PartyTrack>), playlist_index, queue_json, queue_index, playing_from_queue
pub async fn admin_party_queue_sync(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let mut party = social.party.write().await;
    let Some(ref mut session) = party.hosting else {
        info!("admin/party-queue-sync called but not hosting");
        return axum::Json(json!({ "error": "Not hosting a party" }));
    };

    let playlist: Vec<crate::social::protocol::PartyTrack> = params
        .get("playlist_json")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let playlist_index: usize = params
        .get("playlist_index")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let queue: Vec<crate::social::protocol::PartyTrack> = params
        .get("queue_json")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let queue_index: usize = params
        .get("queue_index")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let playing_from_queue: bool = params
        .get("playing_from_queue")
        .map(|s| s == "true")
        .unwrap_or(false);

    // Store in session so direct-poll can return it
    session.playlist = playlist.clone();
    session.playlist_index = playlist_index;
    session.queue = queue.clone();
    session.queue_index = queue_index;
    session.playing_from_queue = playing_from_queue;

    session.seq += 1;
    let seq = session.seq;
    let session_id = session.session_id.clone();
    info!("admin/party-queue-sync: {} playlist tracks, {} queue tracks, idx={}/{}",
        playlist.len(), queue.len(), playlist_index, queue_index);
    drop(party);

    // Push to local event channel for instant follower notification.
    social.push_party_queue_sync_event(
        &session_id, seq, &playlist, playlist_index, &queue, queue_index, playing_from_queue,
    );

    social
        .broadcast_party_queue_sync(
            &session_id, seq, playlist, playlist_index, queue, queue_index, playing_from_queue,
        )
        .await;

    axum::Json(json!({ "ok": true, "seq": seq }))
}

/// GET /admin/party-discover — query friends directly for active parties.
/// Uses direct QUIC connections (FUGUE_ALPN) to each friend, bypassing gossip.
pub async fn admin_party_discover(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Iroh is not configured" }));
    };

    // First check local gossip-based cache
    let mut party = social.party.write().await;
    let mut parties = party.discover_parties();
    drop(party);

    // Also query friends directly via QUIC for reliability
    let friends = crate::social::friends::list_friends(state.db()).await.unwrap_or_default();
    for f in &friends {
        // Skip if we already know about a party from this node
        if parties.iter().any(|p| p.host_node_id == f.public_key) {
            continue;
        }
        if let Ok(addr) = crate::social::node::parse_ticket(&f.ticket) {
            match query_friend_party_status(endpoint, addr.id).await {
                Ok(Some((session_id, host_name))) => {
                    parties.push(crate::social::party::ActiveParty {
                        session_id,
                        host_name,
                        host_node_id: f.public_key.clone(),
                        last_seen_ms: crate::social::party::now_ms(),
                    });
                }
                Ok(None) => {} // friend not hosting
                Err(e) => {
                    tracing::debug!("party-discover: failed to query {}: {}", f.name, e);
                }
            }
        }
    }

    let list: Vec<serde_json::Value> = parties
        .iter()
        .map(|p| {
            json!({
                "session_id": p.session_id,
                "host_name": p.host_name,
                "host_node_id": p.host_node_id,
            })
        })
        .collect();
    axum::Json(json!({ "parties": list }))
}

/// Query a friend's Fugue for party status via SUBSONIC_ALPN (same protocol Moosic uses).
async fn query_friend_party_status(
    endpoint: &iroh::Endpoint,
    peer: iroh::PublicKey,
) -> Result<Option<(String, String)>, Box<dyn std::error::Error + Send + Sync>> {
    let conn = endpoint
        .connect(peer, crate::social::node::SUBSONIC_ALPN)
        .await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    // Send a Subsonic-bridge style request for admin/party-status
    let req = serde_json::json!({
        "endpoint": "admin/party-status",
        "params": {}
    });
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read the response header line (JSON + newline) then the body
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match recv.read(&mut byte).await? {
            Some(1) => {
                if byte[0] == b'\n' { break; }
                buf.push(byte[0]);
            }
            _ => return Err("stream ended before header".into()),
        }
    }
    let header: serde_json::Value = serde_json::from_slice(&buf)?;
    let status = header["status"].as_u64().unwrap_or(0) as u16;
    if status != 200 {
        return Err(format!("HTTP {}", status).into());
    }

    // Read body
    let body_bytes = recv.read_to_end(64 * 1024).await?;
    let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;

    // Parse the party-status response
    let mode = body.get("mode").and_then(|m| m.as_str()).unwrap_or("off");
    if mode == "hosting" {
        let session_id = body.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let host_name = body.get("host_name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        if !session_id.is_empty() {
            return Ok(Some((session_id, host_name)));
        }
    }
    Ok(None)
}

/// GET /admin/party-beacon — re-broadcast PartyCreate for the existing hosted session.
/// Called periodically by the DJ's Moosic to keep the party discoverable.
pub async fn admin_party_beacon(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let Some(endpoint) = state.iroh() else {
        return axum::Json(json!({ "error": "Iroh is not configured" }));
    };

    let party = social.party.read().await;
    let Some(ref session) = party.hosting else {
        return axum::Json(json!({ "error": "Not hosting a party" }));
    };
    let session_id = session.session_id.clone();
    let node_id = endpoint.id().to_string();
    drop(party);

    social.broadcast_party_create(&session_id, &node_id).await;
    axum::Json(json!({ "ok": true }))
}

/// POST /admin/party-end — host ends the session.
pub async fn admin_party_end(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let session_id = {
        let mut party = social.party.write().await;
        party.end_session()
    };

    if let Some(sid) = session_id {
        social.broadcast_party_end(&sid).await;
        axum::Json(json!({ "ok": true, "ended_session": sid }))
    } else {
        axum::Json(json!({ "error": "Not hosting a party" }))
    }
}

/// GET /admin/party-status — current party state.
pub async fn admin_party_status(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let party = social.party.read().await;
    let status = crate::social::party::PartyStatus::from_state(&party);
    axum::Json(serde_json::to_value(status).unwrap_or_default())
}

/// GET /admin/party-full-state — full party state for direct-poll followers.
/// Returns track, position, playlist, queue when hosting.
pub async fn admin_party_full_state(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let party = social.party.read().await;
    match &party.hosting {
        Some(session) => axum::Json(json!({
            "found": true,
            "seq": session.seq,
            "state": session.state,
            "track": session.track,
            "position_secs": session.position_secs,
            "dj_timestamp_ms": session.dj_timestamp_ms,
            "dj_endpoint_addr": session.dj_endpoint_addr,
            "playlist": session.playlist,
            "playlist_index": session.playlist_index,
            "queue": session.queue,
            "queue_index": session.queue_index,
            "playing_from_queue": session.playing_from_queue,
        })),
        None => axum::Json(json!({ "found": false })),
    }
}

/// POST /admin/party-advertise-direct?addr=BASE64 — DJ's Moosic advertises its
/// EndpointAddr so followers can connect directly.
pub async fn admin_party_advertise_direct(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };
    let addr = params.get("addr").cloned().unwrap_or_default();
    if addr.is_empty() {
        return axum::Json(json!({ "error": "Missing addr parameter" }));
    }

    let mut party = social.party.write().await;
    if let Some(ref mut session) = party.hosting {
        session.dj_endpoint_addr = Some(addr.clone());
        info!("party: DJ Moosic advertised direct addr ({} chars)", addr.len());
        axum::Json(json!({ "ok": true }))
    } else {
        axum::Json(json!({ "error": "Not hosting a party" }))
    }
}

/// GET /admin/party-peer-addr?session_id=X — returns the DJ Moosic's EndpointAddr
/// for direct connection. Called by follower's Moosic after joining.
pub async fn admin_party_peer_addr(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(social) = state.social() else {
        return axum::Json(json!({ "error": "Social is not enabled" }));
    };

    let session_id = params.get("session_id").cloned().unwrap_or_default();

    // Check local hosting state first
    let party = social.party.read().await;
    if let Some(ref session) = party.hosting {
        if session.session_id == session_id {
            return axum::Json(json!({
                "ok": true,
                "addr": session.dj_endpoint_addr,
            }));
        }
    }
    drop(party);

    // Check if we know the DJ's addr from gossip/polling (active_parties)
    // For now, the follower's Fugue polls the DJ's Fugue which has the addr
    axum::Json(json!({ "ok": true, "addr": null }))
}

/// GET /admin/party-time-ping — NTP-style time exchange for clock offset estimation.
/// Returns server timestamps so the caller can compute offset and RTT.
pub async fn admin_party_time_ping(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let server_recv_ms = crate::social::party::now_ms();
    let client_send_ms: u64 = params
        .get("t1")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let server_send_ms = crate::social::party::now_ms();
    axum::Json(json!({
        "t1": client_send_ms,
        "t2": server_recv_ms,
        "t3": server_send_ms,
    }))
}

/// GET /admin/party-resolve-track?title=X&artist=Y&album=Z&fingerprint=FP
pub async fn admin_party_resolve_track(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let track = crate::social::protocol::PartyTrack {
        fingerprint: params.get("fingerprint").cloned(),
        song_id: params.get("song_id").cloned().unwrap_or_default(),
        title: params.get("title").cloned().unwrap_or_default(),
        artist: params.get("artist").cloned().unwrap_or_default(),
        album: params.get("album").cloned().unwrap_or_default(),
        track_number: params.get("track_number").and_then(|s| s.parse().ok()),
        duration_secs: params.get("duration_secs").and_then(|s| s.parse().ok()),
    };
    match crate::social::party::resolve_track(state.db(), &track).await {
        Some(song_id) => axum::Json(json!({ "ok": true, "song_id": song_id })),
        None => axum::Json(json!({
            "ok": false,
            "error": "Track not found",
            "title": track.title,
            "artist": track.artist,
        })),
    }
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
