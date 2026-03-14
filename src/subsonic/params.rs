use axum::body::Body;
use axum::extract::{FromRequestParts, Query, Request};
use axum::http::request::Parts;
use axum::http::Uri;
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use std::collections::HashMap;

/// Common Subsonic query parameters present on every request.
#[derive(Debug, Clone)]
pub struct SubsonicParams {
    pub username: String,
    pub version: String,
    pub client: String,
    pub format: ResponseFormat,
    /// The raw query parameters for forwarding additional params.
    pub raw: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFormat {
    Xml,
    Json,
    Jsonp(String),
}

impl ResponseFormat {
    fn from_params(params: &HashMap<String, String>) -> Self {
        match params.get("f").map(|s| s.as_str()) {
            Some("json") => Self::Json,
            Some("jsonp") => {
                let callback = params
                    .get("callback")
                    .cloned()
                    .unwrap_or_else(|| "callback".into());
                Self::Jsonp(callback)
            }
            _ => Self::Xml,
        }
    }
}

#[derive(Deserialize)]
struct RawParams {
    #[serde(flatten)]
    all: HashMap<String, String>,
}

impl<S: Send + Sync> FromRequestParts<S> for SubsonicParams {
    type Rejection = axum::response::Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(raw_params): Query<RawParams> =
            Query::from_request_parts(parts, state)
                .await
                .map_err(|e| {
                    let body = format!(
                        r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="failed" version="1.16.1"><error code="10" message="Missing required parameters: {e}"/></subsonic-response>"#
                    );
                    axum::response::Response::builder()
                        .header("content-type", "text/xml; charset=utf-8")
                        .body(axum::body::Body::from(body))
                        .unwrap()
                        .into()
                })?;

        let all = raw_params.all;

        let username = all.get("u").cloned().unwrap_or_default();
        let version = all.get("v").cloned().unwrap_or_else(|| "1.16.1".into());
        let client = all.get("c").cloned().unwrap_or_else(|| "unknown".into());
        let format = ResponseFormat::from_params(&all);

        Ok(SubsonicParams {
            username,
            version,
            client,
            format,
            raw: all,
        })
    }
}

/// Middleware that merges POST form body params into the URL query string.
/// This allows all extractors (SubsonicParams, AuthenticatedUser) to work
/// with both GET query params and POST form bodies without modification.
pub async fn merge_post_form_params(request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();

    if parts.method == axum::http::Method::POST {
        if let Some(content_type) = parts.headers.get(axum::http::header::CONTENT_TYPE) {
            if content_type
                .to_str()
                .unwrap_or("")
                .starts_with("application/x-www-form-urlencoded")
            {
                // Read the body
                let body_bytes = match axum::body::to_bytes(body, 1024 * 64).await {
                    Ok(b) => b,
                    Err(_) => {
                        return Response::builder()
                            .status(400)
                            .body(Body::from("Failed to read request body"))
                            .unwrap();
                    }
                };

                // Parse form params
                let form_params: Vec<(String, String)> =
                    form_urlencoded::parse(&body_bytes)
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();

                // Merge with existing query params (query params take precedence)
                let existing_query = parts.uri.query().unwrap_or("");
                let existing_params: HashMap<String, String> =
                    form_urlencoded::parse(existing_query.as_bytes())
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();

                let mut merged = HashMap::new();
                for (k, v) in &form_params {
                    merged.insert(k.clone(), v.clone());
                }
                // Query params override form params
                for (k, v) in &existing_params {
                    merged.insert(k.clone(), v.clone());
                }

                // Rebuild the URI with merged query string
                let new_query: String = form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(merged.iter())
                    .finish();

                let mut uri_parts = parts.uri.into_parts();
                let path = uri_parts
                    .path_and_query
                    .as_ref()
                    .map(|pq| pq.path().to_string())
                    .unwrap_or_else(|| "/".into());

                uri_parts.path_and_query =
                    Some(format!("{path}?{new_query}").parse().unwrap());

                parts.uri = Uri::from_parts(uri_parts).unwrap();

                // Continue with empty body since we consumed it
                let request = Request::from_parts(parts, Body::empty());
                return next.run(request).await;
            }
        }
    }

    let request = Request::from_parts(parts, body);
    next.run(request).await
}
