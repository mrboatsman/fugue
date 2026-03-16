use axum::extract::State;
use axum::response::Response;
use sqlx;
use tracing::{debug, warn};

use crate::dedup::resolver;
use crate::error::FugueError;
use crate::id::is_dedup_id;
use crate::proxy::router::route_to_backend;
use crate::proxy::stream::proxy_stream;
use crate::social::collab_playlist;
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
    // Check if this is a remote track (from a friend's collaborative playlist)
    if let Some((owner_node, remote_track_id)) = collab_playlist::decode_remote_track_id(id) {
        let my_node = state.node_id().unwrap_or_default();
        if owner_node == my_node {
            // Track is ours — stream directly from our backend
            debug!("stream collab track (ours) track={}", remote_track_id);
            let (backend, original_id) = route_to_backend(state, &remote_track_id)?;
            let params = build_stream_params(&original_id, extra);
            return proxy_stream(backend, endpoint, &params).await;
        }
        debug!("stream remote {} from node={} track={}", endpoint, owner_node, remote_track_id);
        return stream_from_friend(state, &owner_node, &remote_track_id, endpoint).await;
    }

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

/// Stream a track from a friend's Fugue node via Iroh QUIC.
async fn stream_from_friend(
    state: &AppState,
    owner_node: &str,
    track_id: &str,
    endpoint_type: &str,
) -> Result<Response, FugueError> {
    let endpoint = state.iroh().ok_or_else(|| {
        FugueError::Internal("Social not enabled — cannot stream from friend".into())
    })?;

    debug!("stream_from_friend connecting to {} for track {}", owner_node, track_id);

    // Look up the friend's ticket to get their full address (relay + direct IPs)
    let friend = crate::social::friends::list_friends(state.db())
        .await?
        .into_iter()
        .find(|f| f.public_key == owner_node);

    let addr = if let Some(f) = friend {
        match crate::social::node::parse_ticket(&f.ticket) {
            Ok(addr) => {
                // Register address so endpoint knows how to reach them
                let memory_lookup = iroh::address_lookup::MemoryLookup::default();
                memory_lookup.add_endpoint_info(addr.clone());
                endpoint.address_lookup().add(memory_lookup);
                debug!("stream_from_friend registered friend address with {} addrs", addr.addrs.len());
                addr
            }
            Err(_) => {
                let node_id: iroh::PublicKey = owner_node
                    .parse()
                    .map_err(|_| FugueError::Internal(format!("Invalid friend node ID: {owner_node}")))?;
                iroh_base::EndpointAddr { id: node_id, addrs: Default::default() }
            }
        }
    } else {
        let node_id: iroh::PublicKey = owner_node
            .parse()
            .map_err(|_| FugueError::Internal(format!("Invalid friend node ID: {owner_node}")))?;
        iroh_base::EndpointAddr { id: node_id, addrs: Default::default() }
    };

    debug!("stream_from_friend attempting QUIC connect...");

    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        endpoint.connect(addr, crate::social::node::FUGUE_ALPN),
    )
    .await
    .map_err(|_| FugueError::Backend("Connect to friend timed out after 10s".into()))?
    .map_err(|e| FugueError::Backend(format!("Cannot connect to friend: {e}")))?;

    debug!("stream_from_friend connected, opening stream...");

    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| FugueError::Backend(format!("Cannot open stream to friend: {e}")))?;

    let request = if endpoint_type == "getCoverArt" {
        debug!("stream_from_friend sending StreamCoverArt request...");
        crate::social::protocol::RequestMessage::StreamCoverArt {
            track_id: track_id.to_string(),
        }
    } else {
        debug!("stream_from_friend sending StreamTrack request...");
        crate::social::protocol::RequestMessage::StreamTrack {
            track_id: track_id.to_string(),
        }
    };
    let request_bytes = serde_json::to_vec(&request)
        .map_err(|e| FugueError::Internal(format!("serialize request: {e}")))?;
    send.write_all(&request_bytes).await
        .map_err(|e| FugueError::Backend(format!("write to friend: {e}")))?;
    send.finish()
        .map_err(|e| FugueError::Backend(format!("finish send: {e}")))?;

    debug!("stream_from_friend streaming audio from friend...");

    // Stream the QUIC recv directly to the client as a chunked response.
    // No buffering — bytes flow through as they arrive.
    let stream = futures::stream::unfold(recv, |mut recv| async move {
        let mut buf = vec![0u8; 64 * 1024]; // 64KB chunks
        match recv.read(&mut buf).await {
            Ok(Some(n)) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes::Bytes::from(buf)), recv))
            }
            Ok(None) => None, // stream finished
            Err(e) => {
                warn!("stream_from_friend read error: {}", e);
                None
            }
        }
    });

    let body = axum::body::Body::from_stream(stream);

    let content_type = if endpoint_type == "getCoverArt" {
        "image/jpeg"
    } else {
        "audio/mpeg"
    };

    Ok(axum::response::Response::builder()
        .header("content-type", content_type)
        .body(body)
        .map_err(|e| FugueError::Internal(format!("build response: {e}")))?)
}

