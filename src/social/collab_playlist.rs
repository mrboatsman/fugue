//! Collaborative playlists: shared between Fugue nodes via gossip.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;

fn to_iso8601(ts: &str) -> String {
    if ts.contains('T') { return ts.to_string(); }
    ts.replace(' ', "T") + "Z"
}

/// A track in a collaborative playlist, with ownership info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollabTrack {
    pub track_id: String,      // namespaced ID on the owner's node
    pub owner_node: String,    // node_id that can stream this track
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<i64>,
    pub added_by: String,      // node_id that added this
}

/// Gossip operations for collaborative playlists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum PlaylistOp {
    /// Create a new collaborative playlist.
    Create {
        playlist_id: String,
        name: String,
    },
    /// Add a track to the playlist.
    AddTrack {
        playlist_id: String,
        track: CollabTrack,
    },
    /// Remove a track from the playlist by position.
    RemoveTrack {
        playlist_id: String,
        track_id: String,
        owner_node: String,
    },
    /// Rename the playlist.
    Rename {
        playlist_id: String,
        name: String,
    },
    /// Delete the playlist.
    Delete {
        playlist_id: String,
    },
    /// Full sync: send all tracks (used when a new friend joins).
    FullSync {
        playlist_id: String,
        name: String,
        tracks: Vec<CollabTrack>,
    },
}

// --- DB operations ---

pub async fn create_playlist(
    db: &SqlitePool,
    playlist_id: &str,
    name: &str,
    created_by: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT OR IGNORE INTO collab_playlists (id, name, created_by) VALUES (?, ?, ?)",
    )
    .bind(playlist_id)
    .bind(name)
    .bind(created_by)
    .execute(db)
    .await?;
    debug!("collab: created playlist {} ({})", name, playlist_id);
    Ok(())
}

pub async fn add_track(
    db: &SqlitePool,
    playlist_id: &str,
    track: &CollabTrack,
) -> Result<(), FugueError> {
    // Get next position
    let max_pos: (i64,) = sqlx::query_as(
        "SELECT COALESCE(MAX(position), -1) FROM collab_playlist_tracks WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_one(db)
    .await?;

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
    .bind(max_pos.0 + 1)
    .bind(&track.added_by)
    .execute(db)
    .await?;

    // Update playlist timestamp
    sqlx::query("UPDATE collab_playlists SET updated_at = datetime('now') WHERE id = ?")
        .bind(playlist_id)
        .execute(db)
        .await?;

    debug!("collab: added track '{}' to playlist {}", track.title, playlist_id);
    Ok(())
}

pub async fn remove_track(
    db: &SqlitePool,
    playlist_id: &str,
    track_id: &str,
    owner_node: &str,
) -> Result<(), FugueError> {
    sqlx::query(
        "DELETE FROM collab_playlist_tracks WHERE playlist_id = ? AND track_id = ? AND owner_node = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .bind(owner_node)
    .execute(db)
    .await?;

    // Reindex positions
    let tracks: Vec<(i64,)> = sqlx::query_as(
        "SELECT rowid FROM collab_playlist_tracks WHERE playlist_id = ? ORDER BY position",
    )
    .bind(playlist_id)
    .fetch_all(db)
    .await?;

    for (i, (rowid,)) in tracks.iter().enumerate() {
        sqlx::query("UPDATE collab_playlist_tracks SET position = ? WHERE rowid = ?")
            .bind(i as i64)
            .bind(rowid)
            .execute(db)
            .await?;
    }

    Ok(())
}

pub async fn rename_playlist(
    db: &SqlitePool,
    playlist_id: &str,
    name: &str,
) -> Result<(), FugueError> {
    sqlx::query("UPDATE collab_playlists SET name = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(name)
        .bind(playlist_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_playlist(db: &SqlitePool, playlist_id: &str) -> Result<(), FugueError> {
    sqlx::query("DELETE FROM collab_playlist_tracks WHERE playlist_id = ?")
        .bind(playlist_id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM collab_playlists WHERE id = ?")
        .bind(playlist_id)
        .execute(db)
        .await?;
    debug!("collab: deleted playlist {}", playlist_id);
    Ok(())
}

/// Get all collaborative playlists.
pub async fn list_playlists(db: &SqlitePool) -> Result<Vec<serde_json::Value>, FugueError> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, created_by, created_at FROM collab_playlists ORDER BY name",
    )
    .fetch_all(db)
    .await?;

    let mut playlists = Vec::new();
    for (id, name, created_by, created_at) in rows {
        let track_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM collab_playlist_tracks WHERE playlist_id = ?",
        )
        .bind(&id)
        .fetch_one(db)
        .await?;

        let duration: (Option<i64>,) = sqlx::query_as(
            "SELECT SUM(duration) FROM collab_playlist_tracks WHERE playlist_id = ?",
        )
        .bind(&id)
        .fetch_one(db)
        .await?;

        // Encode as collab playlist ID: "c:{uuid}"
        let encoded_id = encode_collab_id(&id);

        playlists.push(serde_json::json!({
            "id": encoded_id,
            "name": format!("[Collab] {}", name),
            "comment": format!("Collaborative playlist (created by {})", created_by),
            "public": true,
            "owner": created_by,
            "songCount": track_count.0,
            "duration": duration.0.unwrap_or(0),
            "created": to_iso8601(&created_at),
            "changed": to_iso8601(&created_at),
        }));
    }

    Ok(playlists)
}

/// Get a specific collaborative playlist with all tracks.
pub async fn get_playlist(
    db: &SqlitePool,
    playlist_id: &str,
) -> Result<Option<serde_json::Value>, FugueError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, created_by, created_at FROM collab_playlists WHERE id = ?",
    )
    .bind(playlist_id)
    .fetch_optional(db)
    .await?;

    let (id, name, created_by, created_at) = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    let tracks: Vec<(String, String, String, Option<String>, Option<String>, Option<i64>, i64, String)> =
        sqlx::query_as(
            "SELECT track_id, owner_node, title, artist, album, duration, position, added_by
             FROM collab_playlist_tracks WHERE playlist_id = ? ORDER BY position",
        )
        .bind(playlist_id)
        .fetch_all(db)
        .await?;

    let entries: Vec<serde_json::Value> = tracks
        .iter()
        .map(|(track_id, owner_node, title, artist, album, duration, _pos, _added_by)| {
            // Encode the track ID with owner info so we can route streaming
            let stream_id = encode_remote_track_id(owner_node, track_id);
            serde_json::json!({
                "id": stream_id,
                "title": title,
                "artist": artist.as_deref().unwrap_or(""),
                "album": album.as_deref().unwrap_or(""),
                "duration": duration.unwrap_or(0),
                "isDir": false,
                "coverArt": stream_id,
            })
        })
        .collect();

    let encoded_id = encode_collab_id(&id);

    Ok(Some(serde_json::json!({
        "playlist": {
            "id": encoded_id,
            "name": format!("[Collab] {}", name),
            "comment": format!("Collaborative playlist (created by {})", created_by),
            "public": true,
            "owner": created_by,
            "songCount": entries.len(),
            "duration": tracks.iter().filter_map(|t| t.5).sum::<i64>(),
            "created": to_iso8601(&created_at),
            "changed": to_iso8601(&created_at),
            "entry": entries,
        }
    })))
}

/// Get all tracks as CollabTrack for full sync.
pub async fn get_all_tracks(
    db: &SqlitePool,
    playlist_id: &str,
) -> Result<Vec<CollabTrack>, FugueError> {
    let rows: Vec<(String, String, String, Option<String>, Option<String>, Option<i64>, String)> =
        sqlx::query_as(
            "SELECT track_id, owner_node, title, artist, album, duration, added_by
             FROM collab_playlist_tracks WHERE playlist_id = ? ORDER BY position",
        )
        .bind(playlist_id)
        .fetch_all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(track_id, owner_node, title, artist, album, duration, added_by)| CollabTrack {
            track_id,
            owner_node,
            title,
            artist,
            album,
            duration,
            added_by,
        })
        .collect())
}

// --- ID encoding for collaborative content ---

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encode a collaborative playlist ID: base64("c:{uuid}")
pub fn encode_collab_id(uuid: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("c:{uuid}").as_bytes())
}

/// Decode a collaborative playlist ID.
pub fn decode_collab_id(encoded: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    raw.strip_prefix("c:").map(|s| s.to_string())
}

/// Encode a remote track ID: base64("r:{owner_node}:{track_id}")
/// This tells the streaming layer to fetch via Iroh from the owner node.
pub fn encode_remote_track_id(owner_node: &str, track_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("r:{owner_node}:{track_id}").as_bytes())
}

/// Decode a remote track ID. Returns (owner_node, track_id).
pub fn decode_remote_track_id(encoded: &str) -> Option<(String, String)> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    let rest = raw.strip_prefix("r:")?;
    let (owner, track) = rest.split_once(':')?;
    Some((owner.to_string(), track.to_string()))
}

/// Check if an ID is a remote track ID.
pub fn is_remote_track_id(encoded: &str) -> bool {
    decode_remote_track_id(encoded).is_some()
}

// --- Membership & Invite Codes ---

/// Roles for collaborative playlist members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Collab,
    Viewer,
}

impl Role {
    pub fn as_str(&self) -> &str {
        match self {
            Role::Owner => "owner",
            Role::Collab => "collab",
            Role::Viewer => "viewer",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "collab" => Some(Self::Collab),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    pub fn can_edit(&self) -> bool {
        matches!(self, Role::Owner | Role::Collab)
    }
}

/// Generate an invite code for a playlist with a specific role.
/// Format: base64("i:{playlist_id}:{role}:{name}")
pub fn generate_invite(playlist_id: &str, role: Role, name: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("i:{}:{}:{}", playlist_id, role.as_str(), name).as_bytes())
}

/// Parse an invite code. Returns (playlist_id, role, name).
pub fn parse_invite(code: &str) -> Option<(String, Role, String)> {
    let bytes = URL_SAFE_NO_PAD.decode(code).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    let rest = raw.strip_prefix("i:")?;
    // Split: playlist_id:role:name (name may contain colons)
    let (playlist_id, remainder) = rest.split_once(':')?;
    let (role_str, name) = remainder.split_once(':')?;
    let role = Role::from_str(role_str)?;
    Some((playlist_id.to_string(), role, name.to_string()))
}

/// Add a member to a collaborative playlist.
pub async fn add_member(
    db: &SqlitePool,
    playlist_id: &str,
    node_id: &str,
    name: &str,
    role: Role,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO collab_playlist_members (playlist_id, node_id, name, role)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(playlist_id, node_id) DO UPDATE SET role = excluded.role, name = excluded.name",
    )
    .bind(playlist_id)
    .bind(node_id)
    .bind(name)
    .bind(role.as_str())
    .execute(db)
    .await?;
    debug!("collab: added member {} ({}) as {:?} to {}", name, node_id, role, playlist_id);
    Ok(())
}

/// Remove a member from a collaborative playlist.
pub async fn remove_member(
    db: &SqlitePool,
    playlist_id: &str,
    node_id: &str,
) -> Result<(), FugueError> {
    sqlx::query("DELETE FROM collab_playlist_members WHERE playlist_id = ? AND node_id = ?")
        .bind(playlist_id)
        .bind(node_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Get a member's role in a playlist. Returns None if not a member.
pub async fn get_member_role(
    db: &SqlitePool,
    playlist_id: &str,
    node_id: &str,
) -> Result<Option<Role>, FugueError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM collab_playlist_members WHERE playlist_id = ? AND node_id = ?",
    )
    .bind(playlist_id)
    .bind(node_id)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|(r,)| Role::from_str(&r)))
}

/// List members of a collaborative playlist.
pub async fn list_members(
    db: &SqlitePool,
    playlist_id: &str,
) -> Result<Vec<(String, String, Role)>, FugueError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, name, role FROM collab_playlist_members WHERE playlist_id = ? ORDER BY role, name",
    )
    .bind(playlist_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(node_id, name, role)| {
            Some((node_id, name, Role::from_str(&role)?))
        })
        .collect())
}

/// Check if a node can edit a playlist (owner or collab role).
pub async fn can_edit(
    db: &SqlitePool,
    playlist_id: &str,
    node_id: &str,
) -> Result<bool, FugueError> {
    match get_member_role(db, playlist_id, node_id).await? {
        Some(role) => Ok(role.can_edit()),
        None => Ok(false),
    }
}

/// Generate a per-playlist gossip topic ID.
pub fn playlist_topic(playlist_id: &str) -> iroh_gossip::proto::TopicId {
    let hash = blake3::hash(format!("fugue-playlist-{}", playlist_id).as_bytes());
    iroh_gossip::proto::TopicId::from(*hash.as_bytes())
}
