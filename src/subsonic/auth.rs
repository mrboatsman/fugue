use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use md5::{Digest, Md5};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, error};

use crate::config::AuthConfig;
use crate::error::FugueError;
use crate::state::AppState;

/// Extractor that validates Subsonic client authentication.
/// Must be used with AppState.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub username: String,
}

#[derive(Deserialize)]
struct AuthQuery {
    #[serde(flatten)]
    params: HashMap<String, String>,
}

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = FugueError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Query(query): Query<AuthQuery> = Query::from_request_parts(parts, state)
            .await
            .map_err(|_| FugueError::AuthFailed)?;

        let params = query.params;
        let username = params.get("u").ok_or_else(|| {
            error!("auth failed: missing username parameter");
            FugueError::AuthFailed
        })?;

        let auth_config = &state.config().auth;

        // Find the user in config
        let user = auth_config
            .users
            .iter()
            .find(|u| u.username == *username)
            .ok_or_else(|| {
                error!("auth failed: unknown user={}", username);
                FugueError::AuthFailed
            })?;

        // Try token+salt auth first (preferred)
        if let (Some(token), Some(salt)) = (params.get("t"), params.get("s")) {
            if verify_token(&user.password, salt, token) {
                debug!("auth ok user={} method=token", username);
                return Ok(AuthenticatedUser {
                    username: username.clone(),
                });
            }
            error!("auth failed: bad token for user={}", username);
            return Err(FugueError::AuthFailed);
        }

        // Try plaintext password
        if let Some(password) = params.get("p") {
            let password = password.strip_prefix("enc:").map_or_else(
                || password.clone(),
                |hex_str| {
                    hex::decode(hex_str)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_default()
                },
            );
            if password == user.password {
                debug!("auth ok user={} method=password", username);
                return Ok(AuthenticatedUser {
                    username: username.clone(),
                });
            }
            error!("auth failed: bad password for user={}", username);
            return Err(FugueError::AuthFailed);
        }

        error!("auth failed: no credentials for user={}", username);
        Err(FugueError::AuthFailed)
    }
}

fn verify_token(password: &str, salt: &str, token: &str) -> bool {
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let expected = hex::encode(hasher.finalize());
    expected == token.to_lowercase()
}

// Helper to validate auth without being an extractor (for manual use)
pub fn validate_auth(
    auth_config: &AuthConfig,
    username: &str,
    token: Option<&str>,
    salt: Option<&str>,
    password: Option<&str>,
) -> Result<(), FugueError> {
    let user = auth_config
        .users
        .iter()
        .find(|u| u.username == username)
        .ok_or(FugueError::AuthFailed)?;

    if let (Some(token), Some(salt)) = (token, salt) {
        if verify_token(&user.password, salt, token) {
            return Ok(());
        }
        return Err(FugueError::AuthFailed);
    }

    if let Some(password) = password {
        let password = password.strip_prefix("enc:").map_or_else(
            || password.to_string(),
            |hex_str| {
                hex::decode(hex_str)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default()
            },
        );
        if password == user.password {
            return Ok(());
        }
        return Err(FugueError::AuthFailed);
    }

    Err(FugueError::AuthFailed)
}
