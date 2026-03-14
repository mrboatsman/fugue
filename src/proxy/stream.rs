use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use futures::TryStreamExt;
use tracing::debug;

use crate::error::FugueError;
use crate::proxy::backend::BackendClient;

/// Proxy a binary stream (audio, cover art) from a backend to the client.
pub async fn proxy_stream(
    backend: &BackendClient,
    endpoint: &str,
    params: &[(&str, &str)],
) -> Result<Response, FugueError> {
    debug!("proxy_stream endpoint={} backend={}", endpoint, backend.name);
    let resp = backend.request_stream(endpoint, params).await?;

    let mut builder = Response::builder();

    // Forward relevant headers
    if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
        debug!("proxy_stream content_type={:?}", ct);
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    if let Some(cl) = resp.headers().get(header::CONTENT_LENGTH) {
        builder = builder.header(header::CONTENT_LENGTH, cl);
    }
    if let Some(cd) = resp.headers().get(header::CONTENT_DISPOSITION) {
        builder = builder.header(header::CONTENT_DISPOSITION, cd);
    }
    // Forward Accept-Ranges for seeking
    if let Some(ar) = resp.headers().get(header::ACCEPT_RANGES) {
        builder = builder.header(header::ACCEPT_RANGES, ar);
    }

    let stream = resp.bytes_stream().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    });

    let body = Body::from_stream(stream);

    builder
        .body(body)
        .map_err(|e| FugueError::Internal(e.to_string()))
        .map(|r| r.into_response())
}
