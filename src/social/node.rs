//! Iroh node management: identity, endpoint lifecycle, tickets.

use iroh::{Endpoint, SecretKey};
use iroh_base::EndpointAddr;
use sqlx::SqlitePool;
use tracing::{debug, info};

use crate::error::FugueError;

/// The ALPN protocol identifier for Fugue direct requests.
pub const FUGUE_ALPN: &[u8] = b"fugue/social/0";

/// Re-export the gossip ALPN so we can register it on the endpoint.
pub const GOSSIP_ALPN: &[u8] = iroh_gossip::net::GOSSIP_ALPN;

/// Load or generate the node's persistent secret key.
pub async fn load_or_create_secret_key(db: &SqlitePool) -> Result<SecretKey, FugueError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT value FROM identity WHERE key = 'secret_key'")
            .fetch_optional(db)
            .await?;

    if let Some((key_bytes,)) = row {
        let bytes: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| FugueError::Internal("Invalid stored secret key".into()))?;
        let key = SecretKey::from_bytes(&bytes);
        debug!("social: loaded existing identity {}", key.public());
        return Ok(key);
    }

    let key = SecretKey::generate(&mut rand::rng());
    let key_bytes = key.to_bytes();

    sqlx::query("INSERT INTO identity (key, value) VALUES ('secret_key', ?)")
        .bind(key_bytes.as_slice())
        .execute(db)
        .await?;

    info!("social: generated new identity {}", key.public());
    Ok(key)
}

/// Create and start the Iroh endpoint.
pub async fn create_endpoint(secret_key: SecretKey) -> Result<Endpoint, FugueError> {
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![FUGUE_ALPN.to_vec(), GOSSIP_ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| FugueError::Internal(format!("Failed to create Iroh endpoint: {e}")))?;

    info!("social: Iroh endpoint ready, id={}", endpoint.id());

    Ok(endpoint)
}

/// Generate a ticket string containing the full endpoint address
/// (node ID + relay URL + direct addresses).
pub fn generate_ticket(endpoint: &Endpoint) -> String {
    let addr = endpoint.addr();
    let json = serde_json::to_string(&addr).unwrap_or_default();
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// Parse a ticket string back into an EndpointAddr.
pub fn parse_ticket(ticket: &str) -> Result<EndpointAddr, FugueError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(ticket)
        .map_err(|e| FugueError::Internal(format!("Invalid ticket encoding: {e}")))?;
    let json = String::from_utf8(bytes)
        .map_err(|e| FugueError::Internal(format!("Invalid ticket UTF-8: {e}")))?;
    let addr: EndpointAddr = serde_json::from_str(&json)
        .map_err(|e| FugueError::Internal(format!("Invalid ticket format: {e}")))?;
    Ok(addr)
}
