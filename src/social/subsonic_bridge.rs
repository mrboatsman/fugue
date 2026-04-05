//! Bridges Subsonic API requests from Iroh QUIC streams to the axum router.
//!
//! Each QUIC bi-directional stream carries one Subsonic API request/response.
//! The client sends a JSON request (endpoint name + query params), this module
//! constructs a synthetic HTTP request, calls the axum router as a tower
//! Service, and streams the response back over the QUIC stream.
//!
//! Special case: `admin/events` opens a long-lived event stream that pushes
//! activity updates (now playing, friend online/offline) in real time.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{self, HeaderValue};
use axum::response::Response;
use http_body_util::BodyExt;
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use tracing::{debug, error};

/// JSON request sent by the client over a QUIC bi-stream.
#[derive(Deserialize)]
struct SubsonicRequest {
    endpoint: String,
    params: HashMap<String, String>,
}

/// JSON response header written before the body bytes.
/// Terminated by a `\n` so the client can parse it incrementally.
#[derive(Serialize)]
struct ResponseHeader {
    status: u16,
    content_type: String,
    content_length: Option<u64>,
}

fn header_to_str(val: Option<&HeaderValue>) -> Option<&str> {
    val.and_then(|v| v.to_str().ok())
}

/// Handle a bi-stream: either a regular request/response or an event subscription.
pub async fn handle_stream_or_events(
    mut send: SendStream,
    mut recv: RecvStream,
    router: axum::Router,
    event_tx: tokio::sync::broadcast::Sender<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let request_bytes = recv.read_to_end(64 * 1024).await?;
    let req: SubsonicRequest = serde_json::from_slice(&request_bytes)?;

    if req.endpoint == "admin/events" {
        return handle_event_stream(send, event_tx).await;
    }

    handle_request(send, req, router).await
}

/// Handle a regular Subsonic API request/response.
async fn handle_request(
    mut send: SendStream,
    req: SubsonicRequest,
    router: axum::Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    debug!("subsonic-over-iroh: {} with {} params", req.endpoint, req.params.len());

    let query_string: String = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&req.params)
        .finish();

    let path = if req.endpoint.starts_with("admin/") {
        format!("/{}", req.endpoint)
    } else {
        format!("/rest/{}", req.endpoint)
    };
    let uri = format!("{}?{}", path, query_string);
    let http_request = http::Request::builder()
        .method(http::Method::GET)
        .uri(&uri)
        .body(Body::empty())?;

    let response: Response = router.oneshot(http_request).await?;

    let status = response.status().as_u16();
    let content_type = header_to_str(response.headers().get(http::header::CONTENT_TYPE))
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_length = header_to_str(response.headers().get(http::header::CONTENT_LENGTH))
        .and_then(|v| v.parse::<u64>().ok());

    let header = ResponseHeader {
        status,
        content_type,
        content_length,
    };
    let header_bytes = serde_json::to_vec(&header)?;
    send.write_all(&header_bytes).await?;
    send.write_all(b"\n").await?;

    let mut body = response.into_body();
    while let Some(chunk) = body.frame().await {
        match chunk {
            Ok(frame) => {
                if let Some(data) = frame.data_ref() {
                    if let Err(e) = send.write_all(data).await {
                        error!("subsonic-over-iroh: write error: {e}");
                        return Err(e.into());
                    }
                }
            }
            Err(e) => {
                error!("subsonic-over-iroh: body read error: {e}");
                break;
            }
        }
    }

    send.finish()?;
    Ok(())
}

/// Handle a long-lived event subscription stream.
/// Pushes newline-delimited JSON events until the client disconnects.
async fn handle_event_stream(
    mut send: SendStream,
    event_tx: tokio::sync::broadcast::Sender<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    debug!("subsonic-over-iroh: event subscription started");

    let mut rx = event_tx.subscribe();

    loop {
        match rx.recv().await {
            Ok(event_json) => {
                let mut line = event_json.into_bytes();
                line.push(b'\n');
                if let Err(e) = send.write_all(&line).await {
                    debug!("event stream: client disconnected: {e}");
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                debug!("event stream: lagged by {n} messages");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                debug!("event stream: channel closed");
                break;
            }
        }
    }

    let _ = send.finish();
    Ok(())
}
