//! Activity: now playing, chat messages between friends.

use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;

/// Record what a user is currently playing (local or from a friend).
pub async fn set_now_playing(
    db: &SqlitePool,
    node_id: &str,
    user_name: &str,
    track_json: &serde_json::Value,
) -> Result<(), FugueError> {
    let json_str = serde_json::to_string(track_json)
        .map_err(|e| FugueError::Internal(format!("serialize now playing: {e}")))?;

    sqlx::query(
        "INSERT INTO now_playing (node_id, user_name, track_json, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(node_id, user_name) DO UPDATE SET
           track_json = excluded.track_json, updated_at = excluded.updated_at",
    )
    .bind(node_id)
    .bind(user_name)
    .bind(&json_str)
    .execute(db)
    .await?;

    debug!("social: now_playing updated for {}@{}", user_name, node_id);
    Ok(())
}

/// Get all currently playing entries (local + friends), excluding stale entries (>10 min).
pub async fn get_now_playing(
    db: &SqlitePool,
) -> Result<Vec<serde_json::Value>, FugueError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT user_name, node_id, track_json FROM now_playing
         WHERE updated_at > datetime('now', '-10 minutes')
         ORDER BY updated_at DESC",
    )
    .fetch_all(db)
    .await?;

    let entries: Vec<serde_json::Value> = rows
        .into_iter()
        .filter_map(|(user_name, node_id, json_str)| {
            let mut track: serde_json::Value = serde_json::from_str(&json_str).ok()?;
            if let Some(obj) = track.as_object_mut() {
                obj.insert("username".into(), serde_json::json!(user_name));
                obj.insert("nodeId".into(), serde_json::json!(node_id));
            }
            Some(track)
        })
        .collect();

    debug!("social: get_now_playing count={}", entries.len());
    Ok(entries)
}

/// Clear now playing for a user (when they stop playing).
pub async fn clear_now_playing(
    db: &SqlitePool,
    node_id: &str,
    user_name: &str,
) -> Result<(), FugueError> {
    sqlx::query("DELETE FROM now_playing WHERE node_id = ? AND user_name = ?")
        .bind(node_id)
        .bind(user_name)
        .execute(db)
        .await?;
    Ok(())
}

/// Add a chat message.
pub async fn add_chat_message(
    db: &SqlitePool,
    node_id: &str,
    user_name: &str,
    message: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO chat_messages (node_id, user_name, message) VALUES (?, ?, ?)",
    )
    .bind(node_id)
    .bind(user_name)
    .bind(message)
    .execute(db)
    .await?;

    debug!("social: chat message from {}@{}", user_name, node_id);
    Ok(())
}

/// Get recent chat messages (last hour by default).
pub async fn get_chat_messages(
    db: &SqlitePool,
    since_secs: u64,
) -> Result<Vec<serde_json::Value>, FugueError> {
    let since = format!("-{} seconds", since_secs);
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT user_name, node_id, message, created_at FROM chat_messages
         WHERE created_at > datetime('now', ?)
         ORDER BY created_at ASC",
    )
    .bind(&since)
    .fetch_all(db)
    .await?;

    let messages: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(user_name, node_id, message, time)| {
            serde_json::json!({
                "username": user_name,
                "nodeId": node_id,
                "message": message,
                "time": time,
            })
        })
        .collect();

    Ok(messages)
}
