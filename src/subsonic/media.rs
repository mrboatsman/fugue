use axum::extract::State;
use axum::response::Response;
use sqlx;
use tracing::{debug, warn};

use crate::dedup::resolver;
use crate::error::FugueError;
use crate::id::is_dedup_id;
use crate::proxy::router::route_to_backend;
use crate::proxy::stream::proxy_stream;
use crate::state::AppState;
use crate::subsonic::auth::AuthenticatedUser;
use crate::subsonic::params::SubsonicParams;

/// Build the params vec for a stream/download/coverArt request.
fn build_stream_params<'a>(
    original_id: &'a str,
    extra: &'a [(String, String)],
) -> Vec<(&'a str, &'a str)> {
    let mut params = vec![("id", original_id)];
    for (k, v) in extra {
        params.push((k.as_str(), v.as_str()));
    }
    params
}

/// Stream with fallback: for dedup IDs, try sources in health-adjusted score order.
/// If the best source fails to stream, try the next one.
async fn stream_with_fallback(
    state: &AppState,
    id: &str,
    endpoint: &str,
    extra: &[(String, String)],
) -> Result<Response, FugueError> {
    if is_dedup_id(id) {
        debug!("stream resolving dedup id={}", id);

        let mut sources = resolver::resolve_best_sources(state.db(), id).await?;
        if sources.is_empty() {
            return Err(FugueError::NotFound("No sources for dedup track".into()));
        }

        // Sort by health-adjusted score: unavailable backends last, then by score - latency penalty
        sources.sort_by(|a, b| {
            let a_health = state.health().get(a.backend_idx);
            let b_health = state.health().get(b.backend_idx);

            match (a_health.available, b_health.available) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }

            let a_adjusted = a.score - (a_health.latency_ms as f64 * 0.1);
            let b_adjusted = b.score - (b_health.latency_ms as f64 * 0.1);
            b_adjusted.partial_cmp(&a_adjusted).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Try each source, fall back on stream failure
        let mut last_err = None;
        for source in &sources {
            let Ok((backend, original_id)) = route_to_backend(state, &source.namespaced_id) else {
                continue;
            };

            let health = state.health().get(backend.index);
            debug!(
                "stream dedup trying backend={} score={:.0} latency={}ms available={}",
                backend.name, source.score, health.latency_ms, health.available
            );

            let params = build_stream_params(&original_id, extra);
            match proxy_stream(backend, endpoint, &params).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!("stream dedup backend={} failed, trying next: {}", backend.name, e);
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| FugueError::NotFound("All dedup sources failed".into())))
    } else {
        // Regular ID — try direct route first
        let (backend, original_id) = route_to_backend(state, id)?;
        debug!("stream id={} -> backend={}", id, backend.name);
        let params = build_stream_params(&original_id, extra);

        match proxy_stream(backend, endpoint, &params).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // Primary backend failed — check if this track has dedup siblings
                warn!("stream backend={} failed: {}, checking dedup fallback", backend.name, e);

                if let Ok(Some(dedup_members)) = find_dedup_siblings(state, id).await {
                    for sibling_id in &dedup_members {
                        if sibling_id == id {
                            continue;
                        }
                        let Ok((sib_backend, sib_original_id)) = route_to_backend(state, sibling_id) else {
                            continue;
                        };
                        if !state.health().is_available(sib_backend.index) {
                            debug!("stream dedup fallback skipping unavailable backend={}", sib_backend.name);
                            continue;
                        }
                        debug!("stream dedup fallback trying backend={}", sib_backend.name);
                        let sib_params = build_stream_params(&sib_original_id, extra);
                        match proxy_stream(sib_backend, endpoint, &sib_params).await {
                            Ok(resp) => return Ok(resp),
                            Err(e2) => {
                                warn!("stream dedup fallback backend={} also failed: {}", sib_backend.name, e2);
                            }
                        }
                    }
                }

                Err(e)
            }
        }
    }
}

/// Find all sibling namespaced IDs for a track that belongs to a dedup group.
async fn find_dedup_siblings(
    state: &AppState,
    namespaced_id: &str,
) -> Result<Option<Vec<String>>, FugueError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT dm2.namespaced_id FROM dedup_members dm1
         JOIN dedup_members dm2 ON dm2.fingerprint = dm1.fingerprint
         WHERE dm1.namespaced_id = ?
         ORDER BY dm2.score DESC",
    )
    .bind(namespaced_id)
    .fetch_all(state.db())
    .await?;

    if rows.len() <= 1 {
        return Ok(None);
    }

    Ok(Some(rows.into_iter().map(|(id,)| id).collect()))
}

pub async fn stream(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<Response, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    let mut extra = Vec::new();
    if let Some(v) = params.raw.get("maxBitRate") {
        extra.push(("maxBitRate".into(), v.clone()));
    }
    if let Some(v) = params.raw.get("format") {
        extra.push(("format".into(), v.clone()));
    }
    if let Some(v) = params.raw.get("timeOffset") {
        extra.push(("timeOffset".into(), v.clone()));
    }
    if let Some(v) = params.raw.get("estimatedContentLength") {
        extra.push(("estimatedContentLength".into(), v.clone()));
    }

    stream_with_fallback(&state, id, "stream", &extra).await
}

pub async fn download(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<Response, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    stream_with_fallback(&state, id, "download", &[]).await
}

pub async fn get_cover_art(
    _auth: AuthenticatedUser,
    State(state): State<AppState>,
    params: SubsonicParams,
) -> Result<Response, FugueError> {
    let id = params
        .raw
        .get("id")
        .ok_or_else(|| FugueError::Subsonic {
            code: 10,
            message: "Missing required parameter: id".into(),
        })?;

    let mut extra = Vec::new();
    if let Some(v) = params.raw.get("size") {
        extra.push(("size".into(), v.clone()));
    }

    stream_with_fallback(&state, id, "getCoverArt", &extra).await
}
