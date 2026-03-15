//! P2P protocol: message types exchanged between Fugue nodes.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::social::collab_playlist::PlaylistOp;
use crate::social::crdt::CrdtOp;

/// Messages sent over gossip between Fugue peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GossipMessage {
    /// Announce what a user is currently playing.
    NowPlaying {
        display_name: String,
        track: serde_json::Value,
    },
    /// A user stopped playing.
    StoppedPlaying {
        display_name: String,
    },
    /// Chat message.
    Chat {
        display_name: String,
        message: String,
    },
    /// Library summary announcement.
    LibrarySummary {
        display_name: String,
        artist_count: i64,
        album_count: i64,
        track_count: i64,
    },
    /// Collaborative playlist operation (legacy, kept for compat).
    Playlist {
        op: PlaylistOp,
    },
    /// CRDT sync: send operations for a collaborative playlist.
    CrdtSync {
        playlist_id: String,
        ops: Vec<CrdtOp>,
    },
}

impl GossipMessage {
    pub fn to_bytes(&self) -> Bytes {
        Bytes::from(serde_json::to_vec(self).unwrap_or_default())
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Messages sent over direct QUIC streams for request/response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestMessage {
    /// Request the peer's full library index.
    GetLibrary,
    /// Request a specific album's tracks.
    GetAlbum { album_id: String },
    /// Request to stream a track (returns raw audio bytes).
    StreamTrack { track_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseMessage {
    /// Full library index.
    Library { data: serde_json::Value },
    /// Album tracks.
    Album { data: serde_json::Value },
    /// Error response.
    Error { message: String },
}
