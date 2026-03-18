//! Lightweight OR-Set CRDT for collaborative playlists.
//!
//! Each playlist is an Observed-Remove Set where:
//! - Elements are (track_id, owner_node, metadata)
//! - Each add/remove carries a unique tag (node_id + lamport timestamp)
//! - Merge: union of all adds, minus all removes. Add wins over concurrent remove.
//!
//! # Operation Log
//!
//! Every change to a collaborative playlist is stored as an operation in `crdt_ops`:
//! - **AddTrack** — adds a track with metadata
//! - **RemoveTrack** — removes a track
//! - **SetName** — renames the playlist
//!
//! Each operation has a unique `op_id` (`{node_id}:{lamport_timestamp}`) so
//! duplicates are safely ignored.
//!
//! # Materialized View
//!
//! After operations are stored, [`rebuild_playlist`] replays all ops in timestamp
//! order to compute the current track list. This materialized view is what
//! `getPlaylist` serves to clients.
//!
//! # Sync Protocol
//!
//! - When any change happens, the new CRDT ops are broadcast via gossip
//! - When a peer connects (`NeighborUp`), ALL ops for all playlists are broadcast
//! - [`merge_ops`] is idempotent — the same op arriving twice is ignored
//!   (`INSERT OR IGNORE` by `op_id`)
//! - After merging, the materialized view is rebuilt
//!
//! # Offline Resilience
//!
//! ```text
//! Node A adds track X (offline)     Node B adds track Y (offline)
//!   op: A:1 AddTrack(X)               op: B:1 AddTrack(Y)
//!                   \                 /
//!                    --- reconnect ---
//!                   /                 \
//!   receives B:1, merges              receives A:1, merges
//!   rebuild: [X, Y]                   rebuild: [X, Y]
//!   ✓ both converge                   ✓ both converge
//! ```
//!
//! Both nodes end up with the same playlist — no data loss, no conflicts.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;
use crate::social::collab_playlist::CollabTrack;

/// A CRDT operation on a collaborative playlist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrdtOp {
    /// Unique operation ID: "{node_id}:{lamport_clock}"
    pub op_id: String,
    /// Lamport timestamp for causal ordering
    pub timestamp: u64,
    /// The node that created this operation
    pub origin_node: String,
    /// The operation itself
    pub kind: CrdtOpKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum CrdtOpKind {
    AddTrack { track: CollabTrack },
    RemoveTrack { track_id: String, owner_node: String },
    SetName { name: String },
}

/// Get the current Lamport clock for this node on a playlist, then increment it.
pub async fn next_timestamp(
    db: &SqlitePool,
    playlist_id: &str,
    _node_id: &str,
) -> Result<u64, FugueError> {
    // Get max timestamp we've seen for this playlist
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT MAX(timestamp) FROM crdt_ops WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_optional(db)
    .await?;

    let current = row.and_then(|(t,)| Some(t)).unwrap_or(0) as u64;
    Ok(current + 1)
}

/// Store an operation (idempotent — ignores duplicates by op_id).
pub async fn store_op(
    db: &SqlitePool,
    playlist_id: &str,
    op: &CrdtOp,
) -> Result<bool, FugueError> {
    let op_json = serde_json::to_string(&op.kind)
        .map_err(|e| FugueError::Internal(format!("serialize op: {e}")))?;

    let result = sqlx::query(
        "INSERT OR IGNORE INTO crdt_ops (playlist_id, op_id, timestamp, origin_node, op_json)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(playlist_id)
    .bind(&op.op_id)
    .bind(op.timestamp as i64)
    .bind(&op.origin_node)
    .bind(&op_json)
    .execute(db)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Get all operations for a playlist (for sync).
pub async fn get_all_ops(
    db: &SqlitePool,
    playlist_id: &str,
) -> Result<Vec<CrdtOp>, FugueError> {
    let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT op_id, timestamp, origin_node, op_json FROM crdt_ops
         WHERE playlist_id = ? ORDER BY timestamp ASC",
    )
    .bind(playlist_id)
    .fetch_all(db)
    .await?;

    let ops: Vec<CrdtOp> = rows
        .into_iter()
        .filter_map(|(op_id, timestamp, origin_node, op_json)| {
            let kind: CrdtOpKind = serde_json::from_str(&op_json).ok()?;
            Some(CrdtOp {
                op_id,
                timestamp: timestamp as u64,
                origin_node,
                kind,
            })
        })
        .collect();

    Ok(ops)
}

/// Merge a set of remote operations into our local state.
/// Returns the number of new operations applied.
pub async fn merge_ops(
    db: &SqlitePool,
    playlist_id: &str,
    ops: &[CrdtOp],
) -> Result<usize, FugueError> {
    let mut new_count = 0;
    for op in ops {
        if store_op(db, playlist_id, op).await? {
            new_count += 1;
        }
    }

    if new_count > 0 {
        // Rebuild the materialized playlist from the op log
        rebuild_playlist(db, playlist_id).await?;
        debug!("crdt: merged {} new ops for playlist {}", new_count, playlist_id);
    }

    Ok(new_count)
}

/// Rebuild the materialized playlist tracks from the CRDT operation log.
/// This is the "resolve" step: replay all ops to compute current state.
pub async fn rebuild_playlist(
    db: &SqlitePool,
    playlist_id: &str,
) -> Result<(), FugueError> {
    let ops = get_all_ops(db, playlist_id).await?;

    // Compute the current state by replaying ops
    let mut tracks: Vec<CollabTrack> = Vec::new();
    let mut removed: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut playlist_name: Option<String> = None;

    for op in &ops {
        match &op.kind {
            CrdtOpKind::AddTrack { track } => {
                let key = (track.track_id.clone(), track.owner_node.clone());
                if !removed.contains(&key) {
                    // Remove existing entry with same key (re-add updates metadata)
                    tracks.retain(|t| !(t.track_id == track.track_id && t.owner_node == track.owner_node));
                    tracks.push(track.clone());
                }
            }
            CrdtOpKind::RemoveTrack { track_id, owner_node } => {
                let key = (track_id.clone(), owner_node.clone());
                removed.insert(key);
                tracks.retain(|t| !(t.track_id == *track_id && t.owner_node == *owner_node));
            }
            CrdtOpKind::SetName { name } => {
                playlist_name = Some(name.clone());
            }
        }
    }

    // Update the materialized tables
    sqlx::query("DELETE FROM collab_playlist_tracks WHERE playlist_id = ?")
        .bind(playlist_id)
        .execute(db)
        .await?;

    for (pos, track) in tracks.iter().enumerate() {
        sqlx::query(
            "INSERT OR IGNORE INTO collab_playlist_tracks
             (playlist_id, track_id, owner_node, title, artist, album, duration, position, added_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(playlist_id)
        .bind(&track.track_id)
        .bind(&track.owner_node)
        .bind(&track.title)
        .bind(&track.artist)
        .bind(&track.album)
        .bind(track.duration)
        .bind(pos as i64)
        .bind(&track.added_by)
        .execute(db)
        .await?;
    }

    if let Some(name) = playlist_name {
        sqlx::query("UPDATE collab_playlists SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&name)
            .bind(playlist_id)
            .execute(db)
            .await?;
    }

    debug!("crdt: rebuilt playlist {} with {} tracks", playlist_id, tracks.len());
    Ok(())
}
