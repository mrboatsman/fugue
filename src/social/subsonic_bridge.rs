//! Bridges Subsonic API requests from Iroh QUIC streams to the axum router.
//!
//! Each QUIC bi-directional stream carries one Subsonic API request/response.
//! The client sends a JSON request (endpoint name + query params), this module
//! constructs a synthetic HTTP request, calls the axum router as a tower
//! Service, and streams the response back over the QUIC stream.
//!
//! This means **zero changes** to existing Subsonic endpoint handlers — they
//! see the same `Request` they'd get from an HTTP client.

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

/// Handle a single Subsonic-over-Iroh bi-stream.
///
/// `router` must already have state applied (i.e. `Router<()>`).
pub async fn handle_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    router: axum::Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Read the request JSON (same 64KB limit as the social protocol)
    let request_bytes = recv.read_to_end(64 * 1024).await?;
    let req: SubsonicRequest = serde_json::from_slice(&request_bytes)?;

    debug!("subsonic-over-iroh: {} with {} params", req.endpoint, req.params.len());

    // 2. Build query string from params
    let query_string: String = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&req.params)
        .finish();

    // 3. Construct a synthetic HTTP GET request
    let uri = format!("/rest/{}?{}", req.endpoint, query_string);
    let http_request = http::Request::builder()
        .method(http::Method::GET)
        .uri(&uri)
        .body(Body::empty())?;

    // 4. Call the axum router as a tower Service
    let response: Response = router.oneshot(http_request).await?;

    // 5. Extract response metadata
    let status = response.status().as_u16();
    let content_type = header_to_str(response.headers().get(http::header::CONTENT_TYPE))
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_length = header_to_str(response.headers().get(http::header::CONTENT_LENGTH))
        .and_then(|v| v.parse::<u64>().ok());

    // 6. Write the response header JSON + newline delimiter
    let header = ResponseHeader {
        status,
        content_type,
        content_length,
    };
    let header_bytes = serde_json::to_vec(&header)?;
    send.write_all(&header_bytes).await?;
    send.write_all(b"\n").await?;

    // 7. Stream the response body chunk-by-chunk (no full buffering)
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

    // 8. Signal end of response
    send.finish()?;

    Ok(())
}
