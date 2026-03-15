//! Friend management: add, remove, list friends.

use sqlx::SqlitePool;
use tracing::{debug, info};

use crate::error::FugueError;

#[derive(Debug, Clone)]
pub struct Friend {
    pub id: i64,
    pub name: String,
    pub public_key: String,
    pub ticket: String,
    pub added_at: String,
    pub last_seen: Option<String>,
}

/// Add a friend by name and ticket string.
pub async fn add_friend(
    db: &SqlitePool,
    name: &str,
    public_key: &str,
    ticket: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO friends (name, public_key, ticket) VALUES (?, ?, ?)
         ON CONFLICT(public_key) DO UPDATE SET name = excluded.name, ticket = excluded.ticket",
    )
    .bind(name)
    .bind(public_key)
    .bind(ticket)
    .execute(db)
    .await?;

    info!("social: added friend {} ({})", name, public_key);
    Ok(())
}

/// Remove a friend by name.
pub async fn remove_friend(db: &SqlitePool, name: &str) -> Result<bool, FugueError> {
    let result = sqlx::query("DELETE FROM friends WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?;

    let removed = result.rows_affected() > 0;
    if removed {
        info!("social: removed friend {}", name);
    }
    Ok(removed)
}

/// List all friends.
pub async fn list_friends(db: &SqlitePool) -> Result<Vec<Friend>, FugueError> {
    let rows: Vec<(i64, String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, name, public_key, ticket, added_at, last_seen FROM friends ORDER BY name",
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, public_key, ticket, added_at, last_seen)| Friend {
            id,
            name,
            public_key,
            ticket,
            added_at,
            last_seen,
        })
        .collect())
}

/// Update last_seen for a friend.
pub async fn update_last_seen(db: &SqlitePool, public_key: &str) -> Result<(), FugueError> {
    sqlx::query("UPDATE friends SET last_seen = datetime('now') WHERE public_key = ?")
        .bind(public_key)
        .execute(db)
        .await?;
    debug!("social: updated last_seen for {}", public_key);
    Ok(())
}

/// Get a friend by public key.
pub async fn get_friend_by_key(
    db: &SqlitePool,
    public_key: &str,
) -> Result<Option<Friend>, FugueError> {
    let row: Option<(i64, String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, name, public_key, ticket, added_at, last_seen FROM friends WHERE public_key = ?",
    )
    .bind(public_key)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(id, name, public_key, ticket, added_at, last_seen)| Friend {
        id,
        name,
        public_key,
        ticket,
        added_at,
        last_seen,
    }))
}
