//! Party mode: synchronized group playback.
//!
//! One host controls playback; followers track the host's state with
//! sub-second accuracy via gossip heartbeats and clock offset estimation.
//!
//! Sessions are ephemeral (in-memory only) — they don't survive restarts,
//! which is appropriate for a live listening feature.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::debug;

use crate::social::protocol::{PartyPlaybackState, PartyTrack};

fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

// ── Session types ───────────────────────────────────────────────

/// A member of a party session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyMember {
    pub node_id: String,
    pub display_name: String,
}

/// A party session hosted by this node.
#[derive(Debug)]
pub struct PartySession {
    pub session_id: String,
    pub host_name: String,
    pub host_node_id: String,
    pub members: Vec<PartyMember>,
    pub seq: u64,
    /// Current playback state (for heartbeat broadcasts).
    pub state: PartyPlaybackState,
    pub track: Option<PartyTrack>,
    pub position_secs: f64,
    /// DJ's wall-clock timestamp (ms) when position_secs was read.
    /// Travels end-to-end for accurate extrapolation across all hops.
    pub dj_timestamp_ms: u64,
    /// Last known playlist (stored for direct-poll fallback).
    pub playlist: Vec<PartyTrack>,
    pub playlist_index: usize,
    pub queue: Vec<PartyTrack>,
    pub queue_index: usize,
    pub playing_from_queue: bool,
    /// DJ Moosic's EndpointAddr (base64) for direct follower connections.
    pub dj_endpoint_addr: Option<String>,
}

/// A single NTP-style clock measurement.
#[derive(Debug, Clone)]
pub struct ClockSample {
    pub offset_ms: i64,
    pub rtt_ms: u64,
}

/// State for a node that is following a party session.
#[derive(Debug)]
pub struct FollowingSession {
    pub session_id: String,
    pub host_node_id: String,
    pub host_name: String,
    /// Running clock offset estimate: `host_time - local_time` in ms.
    pub clock_offset_ms: i64,
    /// Estimated round-trip time in ms.
    pub rtt_ms: u64,
    /// Recent NTP-style samples for weighted-median filtering.
    clock_samples: VecDeque<ClockSample>,
    /// Last applied sequence number.
    pub last_seq: u64,
}

impl FollowingSession {
    pub fn new(session_id: String, host_node_id: String, host_name: String) -> Self {
        Self {
            session_id,
            host_node_id,
            host_name,
            clock_offset_ms: 0,
            rtt_ms: 0,
            clock_samples: VecDeque::with_capacity(10),
            last_seq: 0,
        }
    }

    /// NTP 4-timestamp clock offset estimation.
    /// T1 = client_send_ms, T2 = server_recv_ms, T3 = server_send_ms,
    /// T4 = client_recv_ms (local time when pong arrived).
    ///
    /// offset = ((T2 - T1) + (T3 - T4)) / 2   (host_time - local_time)
    /// rtt    = (T4 - T1) - (T3 - T2)
    pub fn add_clock_sample(&mut self, t1: u64, t2: u64, t3: u64, t4: u64) {
        let rtt = (t4 as i64 - t1 as i64) - (t3 as i64 - t2 as i64);
        let rtt = rtt.max(0) as u64;
        let offset = ((t2 as i64 - t1 as i64) + (t3 as i64 - t4 as i64)) / 2;

        if self.clock_samples.len() >= 10 {
            self.clock_samples.pop_front();
        }
        self.clock_samples.push_back(ClockSample { offset_ms: offset, rtt_ms: rtt });

        // Weighted median: prefer low-RTT samples (less jitter).
        // Sort by offset, weight inversely by RTT.
        self.clock_offset_ms = weighted_median(&self.clock_samples);
        // RTT as exponential moving average
        if self.rtt_ms == 0 {
            self.rtt_ms = rtt;
        } else {
            self.rtt_ms = (self.rtt_ms * 7 + rtt * 3) / 10;
        }
        debug!("clock: offset={}ms rtt={}ms (sample: offset={}ms rtt={}ms)",
            self.clock_offset_ms, self.rtt_ms, offset, rtt);
    }

    /// Legacy: update from one-way timestamp (gossip path, less accurate).
    pub fn update_clock_offset(&mut self, host_timestamp_ms: u64) {
        let local_now_ms = now_ms();
        let sample = host_timestamp_ms as i64 - local_now_ms as i64;
        // Treat one-way as a high-RTT sample (less trusted)
        if self.clock_samples.len() >= 10 {
            self.clock_samples.pop_front();
        }
        self.clock_samples.push_back(ClockSample {
            offset_ms: sample,
            rtt_ms: 500, // assume high RTT for one-way estimates
        });
        self.clock_offset_ms = weighted_median(&self.clock_samples);
    }
}

/// A remote party seen via gossip (for discovery by late joiners).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveParty {
    pub session_id: String,
    pub host_name: String,
    pub host_node_id: String,
    /// Last time we saw a heartbeat (PartyCreate, PartySync, or PartyQueueSync).
    #[serde(skip)]
    pub last_seen_ms: u64,
}

/// Top-level party state for a Fugue node.
/// A node can host at most one session and follow at most one session
/// (but not both simultaneously).
#[derive(Debug, Default)]
pub struct PartyState {
    pub hosting: Option<PartySession>,
    pub following: Option<FollowingSession>,
    /// Remote parties seen via gossip. Entries expire after 60s without a heartbeat.
    pub active_parties: Vec<ActiveParty>,
}

impl PartyState {
    /// Create a new party session hosted by this node.
    pub fn create_session(&mut self, host_name: String, host_node_id: String) -> &PartySession {
        let session_id = uuid_v4();
        self.following = None; // can't follow while hosting
        self.hosting = Some(PartySession {
            session_id,
            host_name,
            host_node_id,
            members: Vec::new(),
            seq: 0,
            state: PartyPlaybackState::Stopped,
            track: None,
            position_secs: 0.0,
            dj_timestamp_ms: 0,
            playlist: Vec::new(),
            playlist_index: 0,
            queue: Vec::new(),
            queue_index: 0,
            playing_from_queue: false,
            dj_endpoint_addr: None,
        });
        self.hosting.as_ref().unwrap()
    }

    /// End the hosted session.
    pub fn end_session(&mut self) -> Option<String> {
        self.hosting.take().map(|s| s.session_id)
    }

    /// Start following a session.
    pub fn follow(
        &mut self,
        session_id: String,
        host_node_id: String,
        host_name: String,
    ) {
        self.hosting = None; // can't host while following
        self.following = Some(FollowingSession::new(session_id, host_node_id, host_name));
    }

    /// Stop following.
    pub fn unfollow(&mut self) -> Option<String> {
        self.following.take().map(|s| s.session_id)
    }

    /// Record or refresh a remote party seen via gossip.
    pub fn touch_active_party(&mut self, session_id: &str, host_name: &str, host_node_id: &str) {
        let now = now_ms();
        if let Some(p) = self.active_parties.iter_mut().find(|p| p.session_id == session_id) {
            p.last_seen_ms = now;
            p.host_name = host_name.to_string();
        } else {
            self.active_parties.push(ActiveParty {
                session_id: session_id.to_string(),
                host_name: host_name.to_string(),
                host_node_id: host_node_id.to_string(),
                last_seen_ms: now,
            });
        }
    }

    /// Remove a party (when PartyEnd is received).
    pub fn remove_active_party(&mut self, session_id: &str) {
        self.active_parties.retain(|p| p.session_id != session_id);
    }

    /// Prune parties with no heartbeat in the last 60 seconds.
    pub fn prune_stale_parties(&mut self) {
        let cutoff = now_ms().saturating_sub(60_000);
        self.active_parties.retain(|p| p.last_seen_ms > cutoff);
    }

    /// Get active parties (excluding our own if hosting).
    pub fn discover_parties(&mut self) -> Vec<ActiveParty> {
        self.prune_stale_parties();
        let own_session = self.hosting.as_ref().map(|h| h.session_id.as_str());
        self.active_parties
            .iter()
            .filter(|p| own_session != Some(p.session_id.as_str()))
            .cloned()
            .collect()
    }

    /// Add a member to the hosted session.
    pub fn add_member(&mut self, node_id: &str, display_name: &str) {
        if let Some(ref mut session) = self.hosting {
            // Don't add duplicates
            if !session.members.iter().any(|m| m.node_id == node_id) {
                session.members.push(PartyMember {
                    node_id: node_id.to_string(),
                    display_name: display_name.to_string(),
                });
            }
        }
    }

    /// Remove a member from the hosted session.
    pub fn remove_member(&mut self, node_id: &str) {
        if let Some(ref mut session) = self.hosting {
            session.members.retain(|m| m.node_id != node_id);
        }
    }
}

// ── Clock utilities ─────────────────────────────────────────────

/// Current time in milliseconds since UNIX epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Compute weighted median: lower-RTT samples are preferred (less jitter).
/// Weight = 1 / (rtt + 1). Finds the offset at which cumulative weight >= half.
fn weighted_median(samples: &VecDeque<ClockSample>) -> i64 {
    if samples.is_empty() {
        return 0;
    }
    let mut entries: Vec<(i64, f64)> = samples
        .iter()
        .map(|s| (s.offset_ms, 1.0 / (s.rtt_ms as f64 + 1.0)))
        .collect();
    entries.sort_unstable_by_key(|&(offset, _)| offset);
    let total_weight: f64 = entries.iter().map(|&(_, w)| w).sum();
    let half = total_weight / 2.0;
    let mut cumulative = 0.0;
    for &(offset, weight) in &entries {
        cumulative += weight;
        if cumulative >= half {
            return offset;
        }
    }
    entries.last().map(|&(o, _)| o).unwrap_or(0)
}

// ── Track resolution ────────────────────────────────────────────

/// Resolve a `PartyTrack` to a local Subsonic song ID.
///
/// Strategy:
/// 1. Dedup fingerprint lookup in `dedup_groups` → canonical_id
/// 2. Metadata search in `tracks` by artist + title (case-insensitive)
/// 3. `None` if not found (caller should skip this track)
pub async fn resolve_track(db: &SqlitePool, track: &PartyTrack) -> Option<String> {
    // Strategy 1: dedup fingerprint
    if let Some(ref fp) = track.fingerprint {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT canonical_id FROM dedup_groups WHERE fingerprint = ?",
        )
        .bind(fp)
        .fetch_optional(db)
        .await
        .ok()?;
        if let Some((canonical_id,)) = row {
            debug!("party: resolved track by fingerprint: {} → {}", fp, canonical_id);
            return Some(canonical_id);
        }
    }

    // Strategy 2: metadata search — try exact match on title + artist
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM tracks WHERE LOWER(title) = LOWER(?) AND LOWER(artist) = LOWER(?) LIMIT 1",
    )
    .bind(&track.title)
    .bind(&track.artist)
    .fetch_optional(db)
    .await
    .ok()?;

    if let Some((id,)) = row {
        debug!(
            "party: resolved track by metadata: {} - {} → {}",
            track.artist, track.title, id
        );
        return Some(id);
    }

    debug!(
        "party: could not resolve track: {} - {} ({})",
        track.artist,
        track.title,
        track.fingerprint.as_deref().unwrap_or("no fingerprint")
    );
    None
}

// ── Status serialization ────────────────────────────────────────

/// Serializable party status for the admin endpoint.
#[derive(Serialize)]
pub struct PartyStatus {
    pub mode: &'static str, // "off", "hosting", "following"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<PartyMember>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<PartyPlaybackState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<PartyTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_secs: Option<f64>,
}

impl PartyStatus {
    pub fn from_state(party: &PartyState) -> Self {
        match (&party.hosting, &party.following) {
            (Some(h), _) => Self {
                mode: "hosting",
                session_id: Some(h.session_id.clone()),
                host_name: Some(h.host_name.clone()),
                members: Some(h.members.clone()),
                state: Some(h.state),
                track: h.track.clone(),
                position_secs: Some(h.position_secs),
            },
            (_, Some(f)) => Self {
                mode: "following",
                session_id: Some(f.session_id.clone()),
                host_name: Some(f.host_name.clone()),
                members: None,
                state: None,
                track: None,
                position_secs: None,
            },
            _ => Self {
                mode: "off",
                session_id: None,
                host_name: None,
                members: None,
                state: None,
                track: None,
                position_secs: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weighted_median() {
        let mut samples = VecDeque::new();
        // Empty → 0
        assert_eq!(weighted_median(&samples), 0);
        // Single sample
        samples.push_back(ClockSample { offset_ms: 100, rtt_ms: 10 });
        assert_eq!(weighted_median(&samples), 100);
        // Low-RTT sample should be preferred
        samples.push_back(ClockSample { offset_ms: 50, rtt_ms: 1 });
        samples.push_back(ClockSample { offset_ms: 200, rtt_ms: 100 });
        // 50 has weight 0.5, 100 has weight ~0.09, 200 has weight ~0.01
        // sorted by offset: 50 (w=0.5), 100 (w=0.09), 200 (w=0.01)
        // total ≈ 0.6, half ≈ 0.3. Cumulative passes half at offset=50
        assert_eq!(weighted_median(&samples), 50);
    }

    #[test]
    fn test_ntp_clock_offset() {
        let mut session = FollowingSession::new(
            "test".into(),
            "node1".into(),
            "Alice".into(),
        );
        assert_eq!(session.clock_offset_ms, 0);

        // Simulate NTP: host clock is 50ms ahead, 10ms network each way
        // T1=1000, T2=1060 (host received: 1000+10+50), T3=1061, T4=1071 (1061-50+10+50=1071)
        // Actually let's be precise: T4 = T1 + RTT = 1000 + 20 = 1020... but host is 50 ahead
        // T1=1000 (client sends)
        // T2=1060 (server receives: client_time+10 in server_clock = 1000+10+50 = 1060)
        // T3=1061 (server sends, 1ms processing)
        // T4=1021 (client receives: T3-50+10 = 1021)
        // offset = ((T2-T1)+(T3-T4))/2 = ((60)+(40))/2 = 50 ✓
        // RTT = (T4-T1)-(T3-T2) = (21)-(1) = 20 ✓
        session.add_clock_sample(1000, 1060, 1061, 1021);
        assert_eq!(session.clock_offset_ms, 50);
        assert_eq!(session.rtt_ms, 20);
    }

    #[test]
    fn test_party_state_lifecycle() {
        let mut state = PartyState::default();
        assert!(state.hosting.is_none());
        assert!(state.following.is_none());

        // Create session
        let session = state.create_session("Alice".into(), "node_a".into());
        assert_eq!(session.host_name, "Alice");

        // Add members
        state.add_member("node_b", "Bob");
        state.add_member("node_c", "Carol");
        assert_eq!(state.hosting.as_ref().unwrap().members.len(), 2);

        // No duplicates
        state.add_member("node_b", "Bob");
        assert_eq!(state.hosting.as_ref().unwrap().members.len(), 2);

        // Remove member
        state.remove_member("node_b");
        assert_eq!(state.hosting.as_ref().unwrap().members.len(), 1);

        // End session
        let sid = state.end_session();
        assert!(sid.is_some());
        assert!(state.hosting.is_none());

        // Follow
        state.follow("sess1".into(), "node_x".into(), "Xavier".into());
        assert!(state.following.is_some());
        assert_eq!(state.following.as_ref().unwrap().host_name, "Xavier");

        // Unfollow
        let sid = state.unfollow();
        assert_eq!(sid.unwrap(), "sess1");
        assert!(state.following.is_none());
    }
}
