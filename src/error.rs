use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Subsonic API error codes:
/// 0  - Generic error
/// 10 - Required parameter is missing
/// 20 - Incompatible Subsonic REST protocol version
/// 30 - Incompatible server/client version
/// 40 - Wrong username or password
/// 41 - Token authentication not supported (legacy)
/// 50 - User is not authorized for the given operation
/// 60 - Trial period expired (not applicable)
/// 70 - Data not found
#[derive(Debug)]
pub enum FugueError {
    /// Subsonic protocol errors with code
    Subsonic { code: u32, message: String },
    /// Internal errors
    Internal(String),
    /// Backend communication errors
    Backend(String),
    /// Auth failures
    AuthFailed,
    /// Not found
    NotFound(String),
    /// Missing parameter
    MissingParam(String),
    /// Not authorized
    Forbidden(String),
}

impl FugueError {
    pub fn subsonic_code(&self) -> u32 {
        match self {
            Self::Subsonic { code, .. } => *code,
            Self::AuthFailed => 40,
            Self::NotFound(_) => 70,
            Self::MissingParam(_) => 10,
            Self::Forbidden(_) => 50,
            Self::Internal(_) | Self::Backend(_) => 0,
        }
    }

    pub fn subsonic_message(&self) -> String {
        match self {
            Self::Subsonic { message, .. } => message.clone(),
            Self::AuthFailed => "Wrong username or password".into(),
            Self::NotFound(msg) => msg.clone(),
            Self::MissingParam(param) => format!("Required parameter is missing: {param}"),
            Self::Forbidden(msg) => msg.clone(),
            Self::Internal(msg) => msg.clone(),
            Self::Backend(msg) => format!("Backend error: {msg}"),
        }
    }

    /// Helper to create a missing parameter error.
    pub fn missing(param: &str) -> Self {
        Self::Subsonic {
            code: 10,
            message: format!("Missing required parameter: {param}"),
        }
    }
}

impl std::fmt::Display for FugueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subsonic { code, message } => write!(f, "Subsonic error {code}: {message}"),
            Self::Internal(msg) => write!(f, "Internal error: {msg}"),
            Self::Backend(msg) => write!(f, "Backend error: {msg}"),
            Self::AuthFailed => write!(f, "Authentication failed"),
            Self::NotFound(msg) => write!(f, "Not found: {msg}"),
            Self::MissingParam(param) => write!(f, "Missing parameter: {param}"),
            Self::Forbidden(msg) => write!(f, "Forbidden: {msg}"),
        }
    }
}

impl std::error::Error for FugueError {}

impl IntoResponse for FugueError {
    fn into_response(self) -> Response {
        let code = self.subsonic_code();
        let message = self.subsonic_message();
        let escaped_message = message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");

        // Return XML by default (most clients expect it for errors)
        let body = format!(
            r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="failed" version="1.16.1" type="fugue"><error code="{code}" message="{escaped_message}"/></subsonic-response>"#,
        );
        (
            StatusCode::OK,
            [("content-type", "text/xml; charset=utf-8")],
            body,
        )
            .into_response()
    }
}

impl From<reqwest::Error> for FugueError {
    fn from(e: reqwest::Error) -> Self {
        Self::Backend(e.to_string())
    }
}

impl From<sqlx::Error> for FugueError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.to_string())
    }
}
