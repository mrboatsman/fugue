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

    // ── Party mode ──────────────────────────────────────────────

    /// Host created a new party session.
    PartyCreate {
        session_id: String,
        display_name: String,
        node_id: String,
    },
    /// Host invites friends to join a session.
    PartyInvite {
        session_id: String,
        display_name: String,
        node_id: String,
    },
    /// A follower joined the session.
    PartyJoin {
        session_id: String,
        display_name: String,
        node_id: String,
    },
    /// A participant left the session.
    PartyLeave {
        session_id: String,
        display_name: String,
        node_id: String,
    },
    /// Host ended the session.
    PartyEnd {
        session_id: String,
        display_name: String,
    },
    /// Authoritative playback state from the host.
    /// Sent on every state change and periodically as heartbeat (~5s).
    PartySync {
        session_id: String,
        /// Monotonically increasing; followers discard messages with seq <= last applied.
        seq: u64,
        /// Host wall-clock ms since UNIX epoch when this message was created.
        host_timestamp_ms: u64,
        state: PartyPlaybackState,
        track: Option<PartyTrack>,
        /// Playback position in seconds at `host_timestamp_ms`.
        position_secs: f64,
    },
    /// DJ's full playlist + queue state. Sent when playlist/queue changes
    /// and as initial sync when a new follower joins.
    PartyQueueSync {
        session_id: String,
        seq: u64,
        playlist: Vec<PartyTrack>,
        playlist_index: usize,
        queue: Vec<PartyTrack>,
        queue_index: usize,
        playing_from_queue: bool,
    },
}

/// Playback state in a party session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyPlaybackState {
    Playing,
    Paused,
    Stopped,
}

/// Minimal track identity for cross-server matching in party mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyTrack {
    /// Dedup fingerprint ("t::artist::album::title::track_no") — primary match key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Subsonic song ID on the host's server (fallback / display).
    pub song_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
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
    StreamTrack {
        track_id: String,
        /// Requested max bitrate in kbps. 0 = no preference (sender decides).
        #[serde(default)]
        max_bitrate: u32,
        /// Requested format. Empty = no preference.
        #[serde(default)]
        format: String,
    },
    /// Request cover art for a track (returns image bytes).
    StreamCoverArt { track_id: String },
    /// Query if this peer is hosting a party.
    GetPartyStatus,
    /// Request full party state (track, position, playlist) for follower polling.
    GetPartyFullState { session_id: String },
    /// NTP-style time ping for clock offset estimation.
    PartyTimePing { client_send_ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseMessage {
    /// Full library index.
    Library { data: serde_json::Value },
    /// Album tracks.
    Album { data: serde_json::Value },
    /// Party hosting status.
    PartyStatus {
        hosting: bool,
        session_id: Option<String>,
        host_name: Option<String>,
    },
    /// Full party state for follower polling (bypasses gossip).
    PartyFullState {
        found: bool,
        seq: u64,
        state: Option<PartyPlaybackState>,
        track: Option<PartyTrack>,
        position_secs: f64,
        playlist: Vec<PartyTrack>,
        playlist_index: usize,
        queue: Vec<PartyTrack>,
        queue_index: usize,
        playing_from_queue: bool,
    },
    /// NTP-style time pong for clock offset estimation.
    PartyTimePong {
        client_send_ms: u64,
        server_recv_ms: u64,
        server_send_ms: u64,
    },
    /// Error response.
    Error { message: String },
}
