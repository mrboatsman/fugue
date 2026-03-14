use serde_json::{json, Value};
use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;

/// ID prefix for Fugue-local playlists: base64url("m:{uuid}")
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

pub fn encode_local_playlist_id(uuid: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("m:{uuid}").as_bytes())
}

pub fn decode_local_playlist_id(namespaced: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(namespaced).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    raw.strip_prefix("m:").map(|s| s.to_string())
}

pub fn is_local_playlist_id(namespaced: &str) -> bool {
    decode_local_playlist_id(namespaced).is_some()
}

pub async fn create_playlist(
    db: &SqlitePool,
    name: &str,
    owner: &str,
) -> Result<String, FugueError> {
    let uuid = uuid_v4();
    debug!("db create_playlist name={} owner={} uuid={}", name, owner, uuid);
    sqlx::query("INSERT INTO playlists (id, name, owner) VALUES (?, ?, ?)")
        .bind(&uuid)
        .bind(name)
        .bind(owner)
        .execute(db)
        .await?;
    Ok(uuid)
}

pub async fn get_playlists_for_user(
    db: &SqlitePool,
    username: &str,
) -> Result<Vec<Value>, FugueError> {
    debug!("db get_playlists_for_user user={}", username);
    let rows = sqlx::query_as::<_, (String, String, String, bool, String, String)>(
        "SELECT id, name, comment, public, owner, created_at FROM playlists WHERE owner = ? OR public = 1",
    )
    .bind(username)
    .fetch_all(db)
    .await?;

    let mut playlists = Vec::new();
    for (id, name, comment, public, owner, created) in rows {
        let track_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM playlist_tracks WHERE playlist_id = ?")
                .bind(&id)
                .fetch_one(db)
                .await?;

        let duration: (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(0), 0) FROM playlist_tracks WHERE playlist_id = ?",
        )
        .bind(&id)
        .fetch_one(db)
        .await?;

        playlists.push(json!({
            "id": encode_local_playlist_id(&id),
            "name": name,
            "comment": comment,
            "public": public,
            "owner": owner,
            "songCount": track_count.0,
            "duration": duration.0,
            "created": created,
            "changed": created,
        }));
    }

    Ok(playlists)
}

pub async fn get_playlist(
    db: &SqlitePool,
    uuid: &str,
) -> Result<Value, FugueError> {
    debug!("db get_playlist uuid={}", uuid);
    let row = sqlx::query_as::<_, (String, String, String, bool, String, String)>(
        "SELECT id, name, comment, public, owner, created_at FROM playlists WHERE id = ?",
    )
    .bind(uuid)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| FugueError::NotFound("Playlist not found".into()))?;

    let (id, name, comment, public, owner, created) = row;

    let tracks: Vec<(String,)> = sqlx::query_as(
        "SELECT track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(uuid)
    .fetch_all(db)
    .await?;

    let entry: Vec<Value> = tracks
        .iter()
        .map(|(track_id,)| json!({ "id": track_id }))
        .collect();

    Ok(json!({
        "playlist": {
            "id": encode_local_playlist_id(&id),
            "name": name,
            "comment": comment,
            "public": public,
            "owner": owner,
            "songCount": entry.len(),
            "duration": 0,
            "created": created,
            "changed": created,
            "entry": entry,
        }
    }))
}

pub async fn update_playlist(
    db: &SqlitePool,
    uuid: &str,
    name: Option<&str>,
    comment: Option<&str>,
    public: Option<bool>,
) -> Result<(), FugueError> {
    debug!("db update_playlist uuid={}", uuid);
    if let Some(name) = name {
        sqlx::query("UPDATE playlists SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(name)
            .bind(uuid)
            .execute(db)
            .await?;
    }
    if let Some(comment) = comment {
        sqlx::query("UPDATE playlists SET comment = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(comment)
            .bind(uuid)
            .execute(db)
            .await?;
    }
    if let Some(public) = public {
        sqlx::query("UPDATE playlists SET public = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(public)
            .bind(uuid)
            .execute(db)
            .await?;
    }
    Ok(())
}

pub async fn delete_playlist(db: &SqlitePool, uuid: &str) -> Result<(), FugueError> {
    debug!("db delete_playlist uuid={}", uuid);
    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ?")
        .bind(uuid)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(uuid)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn add_tracks_to_playlist(
    db: &SqlitePool,
    uuid: &str,
    track_ids: &[String],
) -> Result<(), FugueError> {
    debug!("db add_tracks uuid={} count={}", uuid, track_ids.len());
    // Get current max position
    let max_pos: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(position), -1) FROM playlist_tracks WHERE playlist_id = ?",
    )
    .bind(uuid)
    .fetch_one(db)
    .await?;

    let mut pos = max_pos.0 + 1;
    for track_id in track_ids {
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?, ?, ?)",
        )
        .bind(uuid)
        .bind(track_id)
        .bind(pos)
        .execute(db)
        .await?;
        pos += 1;
    }
    Ok(())
}

pub async fn remove_tracks_from_playlist(
    db: &SqlitePool,
    uuid: &str,
    indexes: &[i64],
) -> Result<(), FugueError> {
    debug!("db remove_tracks uuid={} indexes={:?}", uuid, indexes);
    for idx in indexes {
        sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ? AND position = ?")
            .bind(uuid)
            .bind(idx)
            .execute(db)
            .await?;
    }
    // Reindex positions to keep them contiguous
    let tracks: Vec<(i64,String)> = sqlx::query_as(
        "SELECT rowid, track_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(uuid)
    .fetch_all(db)
    .await?;

    for (i, (rowid, _)) in tracks.iter().enumerate() {
        sqlx::query("UPDATE playlist_tracks SET position = ? WHERE rowid = ?")
            .bind(i as i64)
            .bind(rowid)
            .execute(db)
            .await?;
    }
    Ok(())
}

/// Simple UUID v4 generation without adding a uuid crate dependency.
fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    // Set version 4
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    // Set variant
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
