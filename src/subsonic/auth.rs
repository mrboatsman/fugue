//! Subsonic authentication mechanisms.
//!
//! Fugue supports three authentication methods, checked in this order:
//!
//! 1. **API key** (`apiKey` param) — OpenSubsonic `apiKeyAuthentication`
//!    extension. The key is SHA-256 hashed and looked up in the `api_keys`
//!    table. Keys are created via `fugue api-key create` and only the hash
//!    is stored; the plaintext is shown once at creation time.
//!
//! 2. **Token + salt** (`u`, `t`, `s` params) — standard Subsonic auth.
//!    The client computes `t = MD5(password + s)` and sends it with a random
//!    salt. Fugue recomputes the hash server-side to verify.
//!
//! 3. **Plaintext password** (`u`, `p` params) — the password is sent
//!    directly (or hex-encoded with an `enc:` prefix). Least secure, but
//!    some older clients only support this.
//!
//! Authentication is implemented as an axum [`FromRequestParts`] extractor
//! ([`AuthenticatedUser`]), so adding it to a handler signature is enough
//! to require auth.

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
/// Supports: token+salt, plaintext password, and API key.
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

        // Check for API key auth first (OpenSubsonic extension)
        if let Some(api_key) = params.get("apiKey") {
            // Error if both apiKey and u are provided
            if params.contains_key("u") {
                return Err(FugueError::Subsonic {
                    code: 43,
                    message: "Multiple conflicting authentication mechanisms provided".into(),
                });
            }
            return verify_api_key(state, api_key).await;
        }

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

/// Verify an API key against stored keys in the database.
async fn verify_api_key(state: &AppState, api_key: &str) -> Result<AuthenticatedUser, FugueError> {
    use sha2::{Sha256, Digest as Sha2Digest};
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    let key_hash = hex::encode(hasher.finalize());

    let row: Option<(String, )> = sqlx::query_as(
        "SELECT username FROM api_keys WHERE key_hash = ?",
    )
    .bind(&key_hash)
    .fetch_optional(state.db())
    .await
    .map_err(|_| FugueError::AuthFailed)?;

    match row {
        Some((username,)) => {
            // Update last_used
            let _ = sqlx::query("UPDATE api_keys SET last_used = datetime('now') WHERE key_hash = ?")
                .bind(&key_hash)
                .execute(state.db())
                .await;
            debug!("auth ok user={} method=apiKey", username);
            Ok(AuthenticatedUser { username })
        }
        None => {
            error!("auth failed: invalid API key");
            Err(FugueError::AuthFailed)
        }
    }
}

/// Generate a new API key for a user. Returns the plaintext key.
pub async fn create_api_key(
    db: &sqlx::SqlitePool,
    username: &str,
    label: &str,
) -> Result<String, FugueError> {
    use sha2::{Sha256, Digest as Sha2Digest};

    // Generate a random key
    let key_bytes: [u8; 32] = rand::random();
    let api_key = hex::encode(key_bytes);

    // Store the hash
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    let key_hash = hex::encode(hasher.finalize());

    sqlx::query("INSERT INTO api_keys (key_hash, username, label) VALUES (?, ?, ?)")
        .bind(&key_hash)
        .bind(username)
        .bind(label)
        .execute(db)
        .await?;

    debug!("created API key for user={} label={}", username, label);
    Ok(api_key)
}

/// Revoke an API key by its hash prefix (first 16 chars).
pub async fn revoke_api_key(db: &sqlx::SqlitePool, hash_prefix: &str) -> Result<bool, FugueError> {
    let result = sqlx::query("DELETE FROM api_keys WHERE key_hash LIKE ?")
        .bind(format!("{}%", hash_prefix))
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// List API keys for a user (shows hash prefix + label, not the key itself).
pub async fn list_api_keys(
    db: &sqlx::SqlitePool,
    username: &str,
) -> Result<Vec<(String, String, String, Option<String>)>, FugueError> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT key_hash, username, label, last_used FROM api_keys WHERE username = ? ORDER BY created_at",
    )
    .bind(username)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

// Legacy helper
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
