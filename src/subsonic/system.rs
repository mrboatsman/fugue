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
