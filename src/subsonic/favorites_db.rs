use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;

pub async fn star(
    db: &SqlitePool,
    owner: &str,
    item_id: &str,
    item_type: &str,
) -> Result<(), FugueError> {
    debug!("db star owner={} item_id={} type={}", owner, item_id, item_type);
    sqlx::query(
        "INSERT OR IGNORE INTO favorites (owner, item_id, item_type) VALUES (?, ?, ?)",
    )
    .bind(owner)
    .bind(item_id)
    .bind(item_type)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn unstar(
    db: &SqlitePool,
    owner: &str,
    item_id: &str,
) -> Result<(), FugueError> {
    debug!("db unstar owner={} item_id={}", owner, item_id);
    sqlx::query("DELETE FROM favorites WHERE owner = ? AND item_id = ?")
        .bind(owner)
        .bind(item_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn get_starred_ids(
    db: &SqlitePool,
    owner: &str,
    item_type: &str,
) -> Result<Vec<(String, String)>, FugueError> {
    debug!("db get_starred_ids owner={} type={}", owner, item_type);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_id, starred_at FROM favorites WHERE owner = ? AND item_type = ? ORDER BY starred_at DESC",
    )
    .bind(owner)
    .bind(item_type)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn is_starred(
    db: &SqlitePool,
    owner: &str,
    item_id: &str,
) -> Result<bool, FugueError> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM favorites WHERE owner = ? AND item_id = ?")
            .bind(owner)
            .bind(item_id)
            .fetch_optional(db)
            .await?;
    Ok(row.is_some())
}
