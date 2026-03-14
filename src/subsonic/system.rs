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
