//! Social service: manages gossip, incoming connections, and friend sync.

use std::sync::Arc;

use futures::StreamExt;
use iroh::Endpoint;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender, JoinOptions};
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use sqlx::SqlitePool;
use tracing::{debug, error, info, warn};

use crate::proxy::backend::BackendClient;
use crate::social::activity;
use crate::social::collab_playlist;
use crate::social::crdt;
use crate::social::friends;
use crate::social::library;
use crate::social::party::{self, PartyState};
use crate::social::protocol::{GossipMessage, RequestMessage, ResponseMessage};

/// A well-known topic ID for the Fugue social network.
fn fugue_topic() -> TopicId {
    let hash = blake3::hash(b"fugue-social-v0");
    TopicId::from(*hash.as_bytes())
}

/// Start the social P2P service.
///
/// `subsonic_router` is the fully-built axum router (with state applied) used
/// to handle Subsonic-over-Iroh requests. It is `None` when the caller only
/// wants the social/gossip features without the Subsonic bridge.
pub async fn start(
    endpoint: Endpoint,
    db: SqlitePool,
    display_name: String,
    backends: Vec<BackendClient>,
    subsonic_router: Option<axum::Router>,
) -> Result<SocialHandle, Box<dyn std::error::Error + Send + Sync>> {
    let gossip = Gossip::builder().spawn(endpoint.clone());

    let friend_list = friends::list_friends(&db).await.unwrap_or_default();

    // Parse friend tickets and register their addresses with the endpoint
    let memory_lookup = iroh::address_lookup::MemoryLookup::default();
    let mut bootstrap_peers = Vec::new();
    for f in &friend_list {
        match crate::social::node::parse_ticket(&f.ticket) {
            Ok(addr) => {
                // Register the full address (relay + direct IPs) so iroh can reach them
                memory_lookup.add_endpoint_info(addr.clone());
                bootstrap_peers.push(addr.id);
                debug!("social: registered friend {} ({}) with {} addresses",
                    f.name, addr.id, addr.addrs.len());
            }
            Err(_) => {
                if let Ok(pk) = f.public_key.parse() {
                    bootstrap_peers.push(pk);
                    debug!("social: bootstrap friend {} by public key only", f.name);
                } else {
                    warn!("social: cannot parse ticket for friend {}", f.name);
                }
            }
        }
    }
    // Add the memory lookup to the endpoint so it can find friend addresses
    endpoint.address_lookup().add(memory_lookup);

    let topic = fugue_topic();
    let topic_handle = if bootstrap_peers.is_empty() {
        info!("social service: subscribing to topic (no friends yet)");
        gossip.subscribe(topic, vec![]).await?
    } else {
        info!(
            "social service: joining topic with {} bootstrap peers",
            bootstrap_peers.len()
        );
        let opts = JoinOptions::with_bootstrap(bootstrap_peers);
        gossip.subscribe_with_opts(topic, opts).await?
    };

    let (sender, receiver) = topic_handle.split();

    let (event_tx, _) = tokio::sync::broadcast::channel::<String>(64);

    let party_state = Arc::new(tokio::sync::RwLock::new(PartyState::default()));

    let handle = SocialHandle {
        sender: Arc::new(tokio::sync::Mutex::new(sender)),
        display_name: display_name.clone(),
        event_tx: event_tx.clone(),
        party: party_state.clone(),
        endpoint: endpoint.clone(),
        follow_poll_cancel: Arc::new(tokio::sync::Notify::new()),
    };

    // Spawn gossip receiver
    let db_clone = db.clone();
    let handle_sender = handle.sender.clone();
    let event_tx_clone = event_tx.clone();
    let party_state_clone = party_state.clone();
    tokio::spawn(async move {
        let mut receiver: GossipReceiver = receiver;
        loop {
            match receiver.next().await {
                Some(Ok(Event::Received(msg))) => {
                    debug!("social: gossip received {} bytes from {}", msg.content.len(), msg.delivered_from);
                    let gossip_msg = GossipMessage::from_bytes(&msg.content);
                    if gossip_msg.is_none() {
                        warn!("social: failed to deserialize gossip message ({} bytes)", msg.content.len());
                    }
                    if let Some(gossip_msg) = gossip_msg {
                        let node_id = msg.delivered_from.to_string();
                        // Push activity events to connected Moosic clients
                        match &gossip_msg {
                            GossipMessage::NowPlaying { display_name, track } => {
                                let event = serde_json::json!({
                                    "type": "now_playing",
                                    "name": display_name,
                                    "node_id": node_id,
                                    "title": track.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                    "artist": track.get("artist").and_then(|a| a.as_str()).unwrap_or(""),
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::StoppedPlaying { display_name } => {
                                let event = serde_json::json!({
                                    "type": "stopped_playing",
                                    "name": display_name,
                                    "node_id": node_id,
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::PartyCreate { session_id, display_name, node_id: host_node } => {
                                info!("social: received PartyCreate from {} (session {})", display_name, session_id);
                                party_state_clone.write().await.touch_active_party(session_id, display_name, host_node);
                                let event = serde_json::json!({
                                    "type": "party_create",
                                    "session_id": session_id,
                                    "display_name": display_name,
                                    "node_id": host_node,
                                });
                                match event_tx_clone.send(event.to_string()) {
                                    Ok(n) => debug!("social: party_create event sent to {} receivers", n),
                                    Err(_) => debug!("social: party_create event dropped (no receivers)"),
                                }
                            }
                            GossipMessage::PartyInvite { session_id, display_name, node_id: host_node } => {
                                let event = serde_json::json!({
                                    "type": "party_invite",
                                    "session_id": session_id,
                                    "display_name": display_name,
                                    "node_id": host_node,
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::PartyJoin { session_id, display_name, node_id: joiner_node } => {
                                let mut ps = party_state_clone.write().await;
                                ps.add_member(joiner_node, display_name);

                                // If we're hosting this session, push current playback
                                // state so the joiner gets the track immediately. The
                                // party_join event below also triggers the DJ's Moosic
                                // to broadcast the full queue.
                                let initial_sync = ps.hosting.as_mut()
                                    .filter(|h| h.session_id == *session_id)
                                    .map(|h| {
                                        h.seq += 1;
                                        (h.session_id.clone(), h.seq, h.state, h.track.clone(), h.position_secs)
                                    });
                                drop(ps);

                                if let Some((sid, seq, state, track, position_secs)) = initial_sync {
                                    info!("social: new joiner {} — pushing current state (seq={})", display_name, seq);
                                    let sync_msg = GossipMessage::PartySync {
                                        session_id: sid,
                                        seq,
                                        host_timestamp_ms: party::now_ms(),
                                        state,
                                        track,
                                        position_secs,
                                    };
                                    let _ = handle_sender.lock().await.broadcast(sync_msg.to_bytes()).await;
                                }

                                let event = serde_json::json!({
                                    "type": "party_join",
                                    "session_id": session_id,
                                    "display_name": display_name,
                                    "node_id": joiner_node,
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::PartyLeave { session_id, display_name, node_id: leaver_node } => {
                                party_state_clone.write().await.remove_member(leaver_node);
                                let event = serde_json::json!({
                                    "type": "party_leave",
                                    "session_id": session_id,
                                    "display_name": display_name,
                                    "node_id": leaver_node,
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::PartyEnd { session_id, display_name } => {
                                // Remove from active parties + clear following if applicable
                                {
                                    let mut ps = party_state_clone.write().await;
                                    ps.remove_active_party(session_id);
                                    if ps.following.as_ref().map_or(false, |f| f.session_id == *session_id) {
                                        ps.unfollow();
                                    }
                                }
                                let event = serde_json::json!({
                                    "type": "party_end",
                                    "session_id": session_id,
                                    "display_name": display_name,
                                });
                                let _ = event_tx_clone.send(event.to_string());
                            }
                            GossipMessage::PartySync {
                                session_id, seq, host_timestamp_ms, state, track, position_secs,
                            } => {
                                info!("social: received PartySync seq={} from gossip", seq);
                                let mut ps = party_state_clone.write().await;
                                // Keep the active party list fresh from heartbeats.
                                // Preserve existing host_name if known; fall back to node_id.
                                let known_name = ps.active_parties.iter()
                                    .find(|p| p.session_id == *session_id)
                                    .map(|p| p.host_name.clone());
                                let host_name = known_name.as_deref().unwrap_or(&node_id);
                                ps.touch_active_party(session_id, host_name, &node_id);
                                let should_emit = if let Some(ref mut f) = ps.following {
                                    info!("social: following session={}, msg session={}, last_seq={}", f.session_id, session_id, f.last_seq);
                                    if f.session_id == *session_id && *seq > f.last_seq {
                                        f.last_seq = *seq;
                                        f.update_clock_offset(*host_timestamp_ms);
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };
                                if should_emit {
                                    let offset = ps.following.as_ref().unwrap().clock_offset_ms;
                                    // Remap track ID for P2P streaming (skip if already remote)
                                    let remote_track = track.as_ref().map(|t| {
                                        let mut rt = t.clone();
                                        if !collab_playlist::is_remote_track_id(&rt.song_id) {
                                            rt.song_id = collab_playlist::encode_remote_track_id(&node_id, &rt.song_id);
                                        }
                                        rt
                                    });
                                    let event = serde_json::json!({
                                        "type": "party_sync",
                                        "session_id": session_id,
                                        "seq": seq,
                                        "host_timestamp_ms": host_timestamp_ms,
                                        "clock_offset_ms": offset,
                                        "state": state,
                                        "track": remote_track,
                                        "position_secs": position_secs,
                                    });
                                    let _ = event_tx_clone.send(event.to_string());
                                }
                            }
                            GossipMessage::PartyQueueSync {
                                session_id, seq, playlist, playlist_index,
                                queue, queue_index, playing_from_queue,
                            } => {
                                info!("social: received PartyQueueSync seq={}, {} playlist tracks", seq, playlist.len());
                                let ps = party_state_clone.read().await;
                                let is_following = ps.following.as_ref()
                                    .map_or(false, |f| f.session_id == *session_id);
                                if !is_following {
                                    info!("social: not following this session, dropping PartyQueueSync");
                                }
                                drop(ps);
                                if is_following {
                                    // Re-encode track IDs as remote IDs so the follower's
                                    // Fugue can route streaming back to the DJ's node via P2P.
                                    let dj_node = &node_id;
                                    let remote_playlist = remap_party_tracks_remote(dj_node, &playlist);
                                    let remote_queue = remap_party_tracks_remote(dj_node, &queue);
                                    let event = serde_json::json!({
                                        "type": "party_queue_sync",
                                        "session_id": session_id,
                                        "seq": seq,
                                        "playlist": remote_playlist,
                                        "playlist_index": playlist_index,
                                        "queue": remote_queue,
                                        "queue_index": queue_index,
                                        "playing_from_queue": playing_from_queue,
                                    });
                                    let _ = event_tx_clone.send(event.to_string());
                                }
                            }
                            _ => {}
                        }
                        handle_gossip_message(&db_clone, &node_id, gossip_msg).await;
                    }
                }
                Some(Ok(Event::NeighborUp(peer))) => {
                    info!("social: peer connected: {}", peer);
                    let _ = friends::update_last_seen(&db_clone, &peer.to_string()).await;
                    let event = serde_json::json!({
                        "type": "friend_online",
                        "node_id": peer.to_string(),
                    });
                    let _ = event_tx_clone.send(event.to_string());

                    // When a peer connects, sync all collab playlists
                    let playlists: Vec<(String, String)> = sqlx::query_as(
                        "SELECT id, name FROM collab_playlists",
                    )
                    .fetch_all(&db_clone)
                    .await
                    .unwrap_or_default();

                    for (pid, pname) in playlists {
                        // Try CRDT ops first
                        let ops = crdt::get_all_ops(&db_clone, &pid).await.unwrap_or_default();
                        if !ops.is_empty() {
                            let msg = GossipMessage::CrdtSync {
                                playlist_id: pid.clone(),
                                ops,
                            };
                            if let Err(e) = handle_sender.lock().await.broadcast(msg.to_bytes()).await {
                                debug!("social: CRDT sync broadcast failed for {}: {}", pid, e);
                            }
                        } else {
                            // Fallback: send FullSync for playlists with no CRDT ops yet
                            if let Ok(tracks) = collab_playlist::get_all_tracks(&db_clone, &pid).await {
                                if !tracks.is_empty() {
                                    let msg = GossipMessage::Playlist {
                                        op: collab_playlist::PlaylistOp::FullSync {
                                            playlist_id: pid.clone(),
                                            name: pname,
                                            tracks,
                                        },
                                    };
                                    if let Err(e) = handle_sender.lock().await.broadcast(msg.to_bytes()).await {
                                        debug!("social: FullSync broadcast failed for {}: {}", pid, e);
                                    }
                                }
                            }
                        }
                    }
                }
                Some(Ok(Event::NeighborDown(peer))) => {
                    debug!("social: peer disconnected: {}", peer);
                    let event = serde_json::json!({
                        "type": "friend_offline",
                        "node_id": peer.to_string(),
                    });
                    let _ = event_tx_clone.send(event.to_string());
                }
                Some(Ok(Event::Lagged)) => {
                    warn!("social: gossip receiver lagged, some messages missed");
                }
                Some(Err(e)) => {
                    error!("social: gossip receiver error: {}", e);
                    break;
                }
                None => {
                    info!("social: gossip stream ended");
                    break;
                }
            }
        }
    });

    // Spawn incoming connection handler — routes by ALPN
    let db_clone2 = db.clone();
    let backends_arc = Arc::new(backends);
    let endpoint_clone = endpoint.clone();
    let gossip_clone = gossip.clone();
    let router_clone = subsonic_router;
    let event_tx_for_conn = event_tx;
    let party_state_for_conn = party_state.clone();
    tokio::spawn(async move {
        loop {
            match endpoint_clone.accept().await {
                Some(incoming) => {
                    let db = db_clone2.clone();
                    let gossip = gossip_clone.clone();
                    let backends = backends_arc.clone();
                    let router = router_clone.clone();
                    let evt_tx = event_tx_for_conn.clone();
                    let party = party_state_for_conn.clone();
                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(conn) => {
                                let alpn = conn.alpn().to_vec();
                                if alpn == crate::social::node::GOSSIP_ALPN {
                                    debug!("social: incoming gossip connection");
                                    if let Err(e) = gossip.handle_connection(conn).await {
                                        debug!("social: gossip connection error: {}", e);
                                    }
                                } else if alpn == crate::social::node::FUGUE_ALPN {
                                    debug!("social: incoming fugue connection");
                                    if let Err(e) = handle_connection(conn, &db, &backends, &party).await {
                                        debug!("social: fugue connection error: {}", e);
                                    }
                                } else if alpn == crate::social::node::SUBSONIC_ALPN {
                                    debug!("social: incoming subsonic-over-iroh connection");
                                    if let Some(router) = router {
                                        // Long-lived connection: accept multiple bi-streams
                                        loop {
                                            match conn.accept_bi().await {
                                                Ok((send, recv)) => {
                                                    let r = router.clone();
                                                    let etx = evt_tx.clone();
                                                    tokio::spawn(async move {
                                                        // Peek at the request to check for event subscription
                                                        if let Err(e) = crate::social::subsonic_bridge::handle_stream_or_events(send, recv, r, etx).await {
                                                            debug!("subsonic-over-iroh stream error: {e}");
                                                        }
                                                    });
                                                }
                                                Err(_) => break,
                                            }
                                        }
                                    } else {
                                        debug!("social: subsonic bridge not configured, dropping connection");
                                    }
                                } else {
                                    debug!("social: unknown ALPN: {:?}", String::from_utf8_lossy(&alpn));
                                }
                            }
                            Err(e) => debug!("social: incoming connection failed: {}", e),
                        }
                    });
                }
                None => {
                    info!("social: endpoint closed");
                    break;
                }
            }
        }
    });

    // Spawn periodic friend address refresh + gossip re-join
    // This ensures newly added friends (or peers that came online after us)
    // are connected in the gossip mesh, not just in the address lookup.
    let db_clone3 = db.clone();
    let endpoint_clone2 = endpoint.clone();
    let rejoin_sender = handle.sender.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            interval.tick().await;
            let peers = refresh_friend_addresses(&db_clone3, &endpoint_clone2).await;
            if !peers.is_empty() {
                let sender = rejoin_sender.lock().await;
                if let Err(e) = sender.join_peers(peers.clone()).await {
                    debug!("social: gossip join_peers failed: {}", e);
                } else {
                    debug!("social: gossip re-joined {} peers", peers.len());
                }
            }
        }
    });

    info!("social service: started (display_name={})", display_name);
    Ok(handle)
}

/// Re-read friends from DB and register any new addresses with the endpoint.
/// Public alias for use from admin endpoints.
pub async fn refresh_friend_addresses_now(db: &SqlitePool, endpoint: &Endpoint) {
    refresh_friend_addresses(db, endpoint).await;
}

/// Refresh friend addresses and return the list of peer IDs for gossip re-join.
async fn refresh_friend_addresses(db: &SqlitePool, endpoint: &Endpoint) -> Vec<iroh::PublicKey> {
    let friend_list = friends::list_friends(db).await.unwrap_or_default();
    let memory_lookup = iroh::address_lookup::MemoryLookup::default();
    let mut peers = Vec::new();

    for f in &friend_list {
        if let Ok(addr) = crate::social::node::parse_ticket(&f.ticket) {
            memory_lookup.add_endpoint_info(addr.clone());
            peers.push(addr.id);
        }
    }

    if !peers.is_empty() {
        endpoint.address_lookup().add(memory_lookup);
        debug!("social: refreshed {} friend addresses", peers.len());
    }
    peers
}

/// Re-encode PartyTrack song_ids as remote track IDs pointing to the DJ's node.
/// This allows the follower's Fugue to stream via P2P back to the DJ,
/// reusing the same routing infrastructure as collaborative playlists.
/// Skips IDs that are already remote-encoded to prevent double-encoding.
fn remap_party_tracks_remote(
    dj_node: &str,
    tracks: &[crate::social::protocol::PartyTrack],
) -> Vec<crate::social::protocol::PartyTrack> {
    tracks
        .iter()
        .map(|t| {
            // Skip if already a remote ID (prevents double-encoding)
            let song_id = if collab_playlist::is_remote_track_id(&t.song_id) {
                t.song_id.clone()
            } else {
                collab_playlist::encode_remote_track_id(dj_node, &t.song_id)
            };
            crate::social::protocol::PartyTrack {
                song_id,
                title: t.title.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                fingerprint: t.fingerprint.clone(),
                track_number: t.track_number,
                duration_secs: t.duration_secs,
            }
        })
        .collect()
}

async fn handle_gossip_message(db: &SqlitePool, node_id: &str, msg: GossipMessage) {
    match msg {
        GossipMessage::NowPlaying {
            display_name,
            track,
        } => {
            debug!("social: {} is playing {:?}", display_name, track.get("title"));
            let _ = activity::set_now_playing(db, node_id, &display_name, &track).await;
        }
        GossipMessage::StoppedPlaying { display_name } => {
            debug!("social: {} stopped playing", display_name);
            let _ = activity::clear_now_playing(db, node_id, &display_name).await;
        }
        GossipMessage::Chat {
            display_name,
            message,
        } => {
            debug!("social: chat from {}: {}", display_name, message);
            let _ = activity::add_chat_message(db, node_id, &display_name, &message).await;
        }
        GossipMessage::LibrarySummary {
            display_name,
            artist_count,
            album_count,
            track_count,
        } => {
            debug!(
                "social: {} has {} artists, {} albums, {} tracks",
                display_name, artist_count, album_count, track_count
            );
        }
        GossipMessage::Playlist { op } => {
            handle_playlist_op(db, node_id, &op).await;
        }
        GossipMessage::CrdtSync { playlist_id, ops } => {
            debug!("social: received CRDT sync for {} ({} ops)", playlist_id, ops.len());
            // Ensure the playlist exists locally
            let _ = collab_playlist::create_playlist(db, &playlist_id, "(syncing...)", node_id).await;
            match crdt::merge_ops(db, &playlist_id, &ops).await {
                Ok(new) => {
                    if new > 0 {
                        debug!("social: applied {} new CRDT ops for {}", new, playlist_id);
                    }
                }
                Err(e) => error!("social: CRDT merge failed for {}: {}", playlist_id, e),
            }
        }
        // Party messages are handled in the gossip receive loop (above),
        // not here — they only need event dispatch, not DB persistence.
        GossipMessage::PartyCreate { .. }
        | GossipMessage::PartyInvite { .. }
        | GossipMessage::PartyJoin { .. }
        | GossipMessage::PartyLeave { .. }
        | GossipMessage::PartyEnd { .. }
        | GossipMessage::PartySync { .. }
        | GossipMessage::PartyQueueSync { .. } => {}
    }
}

/// Handle a collaborative playlist operation received via gossip.
/// Checks the sender's role before applying write operations.
async fn handle_playlist_op(db: &SqlitePool, sender_node: &str, op: &collab_playlist::PlaylistOp) {
    use collab_playlist::PlaylistOp;
    match op {
        PlaylistOp::Create { playlist_id, name } => {
            debug!("social: collab playlist created: {} ({})", name, playlist_id);
            let _ = collab_playlist::create_playlist(db, playlist_id, name, sender_node).await;
        }
        PlaylistOp::AddTrack { playlist_id, track } => {
            // Check if sender can edit
            let can = collab_playlist::can_edit(db, playlist_id, sender_node).await.unwrap_or(false);
            // Also allow if playlist is new (no members yet — creator is pushing initial state)
            let members = collab_playlist::list_members(db, playlist_id).await.unwrap_or_default();
            if can || members.is_empty() {
                debug!("social: collab track added to {}: {}", playlist_id, track.title);
                let _ = collab_playlist::add_track(db, playlist_id, track).await;
            } else {
                debug!("social: rejected AddTrack from {} (viewer) on {}", sender_node, playlist_id);
            }
        }
        PlaylistOp::RemoveTrack { playlist_id, track_id, owner_node } => {
            let can = collab_playlist::can_edit(db, playlist_id, sender_node).await.unwrap_or(false);
            if can {
                debug!("social: collab track removed from {}", playlist_id);
                let _ = collab_playlist::remove_track(db, playlist_id, track_id, owner_node).await;
            } else {
                debug!("social: rejected RemoveTrack from {} (viewer) on {}", sender_node, playlist_id);
            }
        }
        PlaylistOp::Rename { playlist_id, name } => {
            let can = collab_playlist::can_edit(db, playlist_id, sender_node).await.unwrap_or(false);
            if can {
                let _ = collab_playlist::rename_playlist(db, playlist_id, name).await;
            }
        }
        PlaylistOp::Delete { playlist_id } => {
            // Only owner can delete
            let role = collab_playlist::get_member_role(db, playlist_id, sender_node).await.unwrap_or(None);
            if role == Some(collab_playlist::Role::Owner) {
                let _ = collab_playlist::delete_playlist(db, playlist_id).await;
            } else {
                debug!("social: rejected Delete from {} (not owner) on {}", sender_node, playlist_id);
            }
        }
        PlaylistOp::FullSync { playlist_id, name, tracks } => {
            debug!("social: collab playlist full sync: {} ({} tracks)", name, tracks.len());
            let _ = collab_playlist::create_playlist(db, playlist_id, name, sender_node).await;

            // Backfill CRDT ops from the FullSync so the op log is complete.
            // This ensures rebuild_playlist doesn't lose these tracks.
            for (i, track) in tracks.iter().enumerate() {
                let op = crdt::CrdtOp {
                    op_id: format!("fullsync:{}:{}", sender_node, i),
                    timestamp: i as u64 + 1,
                    origin_node: track.added_by.clone(),
                    kind: crdt::CrdtOpKind::AddTrack { track: track.clone() },
                };
                let _ = crdt::store_op(db, playlist_id, &op).await;
            }

            // Also store the name
            let name_op = crdt::CrdtOp {
                op_id: format!("fullsync:{}:name", sender_node),
                timestamp: 0,
                origin_node: sender_node.to_string(),
                kind: crdt::CrdtOpKind::SetName { name: name.clone() },
            };
            let _ = crdt::store_op(db, playlist_id, &name_op).await;

            // Rebuild from the op log
            let _ = crdt::rebuild_playlist(db, playlist_id).await;
        }
    }
}

async fn handle_connection(
    conn: iroh::endpoint::Connection,
    db: &SqlitePool,
    backends: &[BackendClient],
    party_state: &Arc<tokio::sync::RwLock<PartyState>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut send, mut recv) = conn.accept_bi().await?;

    let request_bytes = recv.read_to_end(64 * 1024).await?;
    let request: RequestMessage = serde_json::from_slice(&request_bytes)?;

    debug!("social: direct request: {:?}", request);

    match request {
        RequestMessage::StreamTrack { track_id, max_bitrate, format } => {
            debug!("social: stream request for track {} (maxBitRate={}, format={})", track_id, max_bitrate, format);
            if let Err(e) = stream_from_backend_for_peer(backends, "stream", &track_id, max_bitrate, &format, &mut send).await {
                error!("social: stream failed for {}: {}", track_id, e);
            }
            return Ok(());
        }
        RequestMessage::StreamCoverArt { track_id } => {
            debug!("social: cover art request for track {}", track_id);
            if let Err(e) = stream_from_backend_for_peer(backends, "getCoverArt", &track_id, 0, "", &mut send).await {
                error!("social: cover art failed for {}: {}", track_id, e);
            }
            return Ok(());
        }
        _ => {}
    }

    let response = match request {
        RequestMessage::GetLibrary => {
            match library::build_library_summary(db).await {
                Ok(data) => ResponseMessage::Library { data },
                Err(e) => ResponseMessage::Error {
                    message: e.to_string(),
                },
            }
        }
        RequestMessage::GetAlbum { album_id } => {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT data_json FROM albums WHERE id = ?",
            )
            .bind(&album_id)
            .fetch_optional(db)
            .await?;

            match row {
                Some((json_str,)) => {
                    let data: serde_json::Value = serde_json::from_str(&json_str)?;
                    ResponseMessage::Album { data }
                }
                None => ResponseMessage::Error {
                    message: "Album not found".into(),
                },
            }
        }
        RequestMessage::GetPartyStatus => {
            let ps = party_state.read().await;
            match &ps.hosting {
                Some(session) => ResponseMessage::PartyStatus {
                    hosting: true,
                    session_id: Some(session.session_id.clone()),
                    host_name: Some(session.host_name.clone()),
                },
                None => ResponseMessage::PartyStatus {
                    hosting: false,
                    session_id: None,
                    host_name: None,
                },
            }
        }
        RequestMessage::GetPartyFullState { session_id: req_sid } => {
            let ps = party_state.read().await;
            match &ps.hosting {
                Some(session) if session.session_id == req_sid => {
                    ResponseMessage::PartyFullState {
                        found: true,
                        seq: session.seq,
                        state: Some(session.state),
                        track: session.track.clone(),
                        position_secs: session.position_secs,
                        playlist: session.playlist.clone(),
                        playlist_index: session.playlist_index,
                        queue: session.queue.clone(),
                        queue_index: session.queue_index,
                        playing_from_queue: session.playing_from_queue,
                    }
                }
                _ => ResponseMessage::PartyFullState {
                    found: false,
                    seq: 0,
                    state: None,
                    track: None,
                    position_secs: 0.0,
                    playlist: Vec::new(),
                    playlist_index: 0,
                    queue: Vec::new(),
                    queue_index: 0,
                    playing_from_queue: false,
                },
            }
        }
        RequestMessage::PartyTimePing { client_send_ms } => {
            let server_recv_ms = party::now_ms();
            ResponseMessage::PartyTimePong {
                client_send_ms,
                server_recv_ms,
                server_send_ms: party::now_ms(),
            }
        }
        RequestMessage::StreamTrack { .. }
        | RequestMessage::StreamCoverArt { .. } => unreachable!(),
    };

    let response_bytes = serde_json::to_vec(&response)?;
    send.write_all(&response_bytes).await?;
    send.finish()?;

    Ok(())
}

/// Stream content (audio or cover art) from our backend to a requesting peer.
/// Writes directly to the QUIC send stream to avoid buffering.
async fn stream_from_backend_for_peer(
    backends: &[BackendClient],
    endpoint: &str,
    track_id: &str,
    max_bitrate: u32,
    format: &str,
    send: &mut iroh::endpoint::SendStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (backend_idx, original_id) = crate::id::decode_id(track_id)?;

    let backend = backends
        .iter()
        .find(|b| b.index == backend_idx)
        .ok_or_else(|| format!("Backend {} not found", backend_idx))?;

    let mut params: Vec<(&str, String)> = vec![("id", original_id.clone())];
    let br_str;
    if max_bitrate > 0 {
        br_str = max_bitrate.to_string();
        params.push(("maxBitRate", br_str.clone()));
        debug!("social: streaming {} {} from backend {} (maxBitRate={}, format={})",
            endpoint, original_id, backend.name, max_bitrate, format);
    } else {
        debug!("social: streaming {} {} from backend {} (raw)", endpoint, original_id, backend.name);
    }
    if !format.is_empty() && format != "raw" && format != "auto" {
        params.push(("format", format.to_string()));
    }

    let param_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let resp = backend
        .request_stream(endpoint, &param_refs)
        .await
        .map_err(|e| format!("Backend {} failed: {e}", endpoint))?;

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Read chunk: {e}"))?;
        send.write_all(&chunk).await.map_err(|e| format!("Write to peer: {e}"))?;
        total += chunk.len();
    }
    send.finish().map_err(|e| format!("Finish send: {e}"))?;

    debug!("social: streamed {} bytes ({}) for {}", total, endpoint, track_id);
    Ok(())
}

/// Handle for interacting with the social service from HTTP handlers.
#[derive(Clone)]
pub struct SocialHandle {
    sender: Arc<tokio::sync::Mutex<GossipSender>>,
    display_name: String,
    /// Broadcast channel for pushing live events to connected Moosic clients.
    event_tx: tokio::sync::broadcast::Sender<String>,
    /// In-memory party mode state.
    pub party: Arc<tokio::sync::RwLock<PartyState>>,
    /// Iroh endpoint for direct QUIC queries to friends.
    endpoint: Endpoint,
    /// Cancel token for the party follow-polling task.
    follow_poll_cancel: Arc<tokio::sync::Notify>,
}

impl SocialHandle {
    pub async fn broadcast_now_playing(&self, track: &serde_json::Value) {
        let msg = GossipMessage::NowPlaying {
            display_name: self.display_name.clone(),
            track: track.clone(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast now_playing failed: {}", e);
        }
    }

    pub async fn broadcast_stopped_playing(&self) {
        let msg = GossipMessage::StoppedPlaying {
            display_name: self.display_name.clone(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast stopped failed: {}", e);
        }
    }

    pub async fn broadcast_chat(&self, message: &str) {
        let msg = GossipMessage::Chat {
            display_name: self.display_name.clone(),
            message: message.to_string(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast chat failed: {}", e);
        }
    }

    /// Get a lock on the gossip sender for direct broadcasting.
    pub async fn sender(&self) -> tokio::sync::MutexGuard<'_, GossipSender> {
        self.sender.lock().await
    }

    /// Broadcast a collaborative playlist operation to all friends.
    pub async fn broadcast_playlist_op(&self, op: collab_playlist::PlaylistOp) {
        let msg = GossipMessage::Playlist { op };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast playlist op failed: {}", e);
        }
    }

    /// Subscribe to live events (now playing, friend online/offline, party).
    /// Returns a receiver that yields newline-delimited JSON strings.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.event_tx.subscribe()
    }

    // ── Party event push (instant, local event channel) ───────

    /// Push a party sync event to the local event channel.
    /// This makes it available to any QUIC subscriber (follower's Fugue
    /// connected via admin/events) without waiting for gossip or polling.
    pub fn push_party_sync_event(
        &self,
        session_id: &str,
        seq: u64,
        state: crate::social::protocol::PartyPlaybackState,
        track: Option<&crate::social::protocol::PartyTrack>,
        position_secs: f64,
        dj_timestamp_ms: u64,
    ) {
        let event = serde_json::json!({
            "type": "party_sync",
            "session_id": session_id,
            "seq": seq,
            "host_timestamp_ms": party::now_ms(),
            "dj_timestamp_ms": dj_timestamp_ms,
            "clock_offset_ms": 0,
            "state": state,
            "track": track,
            "position_secs": position_secs,
        });
        let _ = self.event_tx.send(event.to_string());
    }

    /// Push a party queue sync event to the local event channel.
    pub fn push_party_queue_sync_event(
        &self,
        session_id: &str,
        seq: u64,
        playlist: &[crate::social::protocol::PartyTrack],
        playlist_index: usize,
        queue: &[crate::social::protocol::PartyTrack],
        queue_index: usize,
        playing_from_queue: bool,
    ) {
        let event = serde_json::json!({
            "type": "party_queue_sync",
            "session_id": session_id,
            "seq": seq,
            "playlist": playlist,
            "playlist_index": playlist_index,
            "queue": queue,
            "queue_index": queue_index,
            "playing_from_queue": playing_from_queue,
        });
        let _ = self.event_tx.send(event.to_string());
    }

    // ── Party mode (gossip broadcast) ──────────────────────────

    /// Broadcast that this node created a party session.
    pub async fn broadcast_party_create(&self, session_id: &str, node_id: &str) {
        info!("social: broadcasting PartyCreate (session={}, node={})", session_id, node_id);
        let msg = GossipMessage::PartyCreate {
            session_id: session_id.to_string(),
            display_name: self.display_name.clone(),
            node_id: node_id.to_string(),
        };
        let sender = self.sender.lock().await;
        let bytes = msg.to_bytes();
        info!("social: broadcasting PartyCreate ({} bytes)", bytes.len());
        match sender.broadcast(bytes).await {
            Ok(()) => info!("social: PartyCreate broadcast sent"),
            Err(e) => error!("social: broadcast party_create failed: {}", e),
        }
    }

    /// Broadcast a party invite.
    pub async fn broadcast_party_invite(&self, session_id: &str, node_id: &str) {
        let msg = GossipMessage::PartyInvite {
            session_id: session_id.to_string(),
            display_name: self.display_name.clone(),
            node_id: node_id.to_string(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast party_invite failed: {}", e);
        }
    }

    /// Broadcast that this node joined a party.
    pub async fn broadcast_party_join(&self, session_id: &str, node_id: &str) {
        let msg = GossipMessage::PartyJoin {
            session_id: session_id.to_string(),
            display_name: self.display_name.clone(),
            node_id: node_id.to_string(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast party_join failed: {}", e);
        }
    }

    /// Broadcast that this node left a party.
    pub async fn broadcast_party_leave(&self, session_id: &str, node_id: &str) {
        let msg = GossipMessage::PartyLeave {
            session_id: session_id.to_string(),
            display_name: self.display_name.clone(),
            node_id: node_id.to_string(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast party_leave failed: {}", e);
        }
    }

    /// Broadcast that the hosted party has ended.
    pub async fn broadcast_party_end(&self, session_id: &str) {
        let msg = GossipMessage::PartyEnd {
            session_id: session_id.to_string(),
            display_name: self.display_name.clone(),
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            debug!("social: broadcast party_end failed: {}", e);
        }
    }

    /// Broadcast the DJ's playlist + queue state to all followers.
    pub async fn broadcast_party_queue_sync(
        &self,
        session_id: &str,
        seq: u64,
        playlist: Vec<crate::social::protocol::PartyTrack>,
        playlist_index: usize,
        queue: Vec<crate::social::protocol::PartyTrack>,
        queue_index: usize,
        playing_from_queue: bool,
    ) {
        let msg = GossipMessage::PartyQueueSync {
            session_id: session_id.to_string(),
            seq,
            playlist,
            playlist_index,
            queue,
            queue_index,
            playing_from_queue,
        };
        let sender = self.sender.lock().await;
        if let Err(e) = sender.broadcast(msg.to_bytes()).await {
            warn!("social: broadcast party_queue_sync failed: {}", e);
        }
    }

    /// Broadcast an authoritative playback sync message (host only).
    pub async fn broadcast_party_sync(
        &self,
        session_id: &str,
        seq: u64,
        state: crate::social::protocol::PartyPlaybackState,
        track: Option<crate::social::protocol::PartyTrack>,
        position_secs: f64,
    ) {
        let msg = GossipMessage::PartySync {
            session_id: session_id.to_string(),
            seq,
            host_timestamp_ms: party::now_ms(),
            state,
            track,
            position_secs,
        };
        let bytes = msg.to_bytes();
        let sender = self.sender.lock().await;
        match sender.broadcast(bytes).await {
            Ok(()) => debug!("social: PartySync broadcast sent (seq={})", seq),
            Err(e) => warn!("social: broadcast party_sync failed: {}", e),
        }
    }

    // ── Follow-mode direct polling ─────────────────────────────

    /// Start a background task that polls the host's Fugue directly via QUIC
    /// every 3 seconds. This bypasses gossip entirely and ensures the follower
    /// gets party state updates even when gossip mesh is broken.
    pub fn start_follow_poll(
        &self,
        session_id: String,
        host_node_id: String,
    ) {
        // Cancel any previous poll task
        self.follow_poll_cancel.notify_waiters();
        let cancel = self.follow_poll_cancel.clone();

        let endpoint = self.endpoint.clone();
        let event_tx = self.event_tx.clone();
        let party = self.party.clone();

        let host_pk: iroh::PublicKey = match host_node_id.parse() {
            Ok(pk) => pk,
            Err(e) => {
                error!("follow-poll: invalid host node_id {}: {}", host_node_id, e);
                return;
            }
        };

        tokio::spawn(async move {
            info!("follow-poll: started for session {} (host {})", session_id, host_node_id);
            let mut last_seq = 0u64;
            let mut last_playlist_hash = 0u64;

            // Open a push subscription to the DJ's Fugue event stream.
            // This gives us instant party_sync/party_queue_sync events.
            let (push_tx, mut push_rx) = tokio::sync::mpsc::channel::<String>(64);
            let push_endpoint = endpoint.clone();
            let push_cancel = cancel.clone();
            tokio::spawn(async move {
                loop {
                    match subscribe_host_events(&push_endpoint, host_pk).await {
                        Ok(mut recv) => {
                            info!("follow-poll: push subscription connected");
                            let mut buf = Vec::new();
                            let mut byte = [0u8; 1];
                            loop {
                                tokio::select! {
                                    _ = push_cancel.notified() => return,
                                    result = recv.read(&mut byte) => {
                                        match result {
                                            Ok(Some(1)) => {
                                                if byte[0] == b'\n' {
                                                    if let Ok(line) = String::from_utf8(std::mem::take(&mut buf)) {
                                                        if push_tx.send(line).await.is_err() {
                                                            return;
                                                        }
                                                    }
                                                } else {
                                                    buf.push(byte[0]);
                                                }
                                            }
                                            _ => break, // stream died, reconnect
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            debug!("follow-poll: push subscribe failed: {}, retrying in 5s", e);
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            });

            // Adaptive poll interval: fast when drifting, slow when synced.
            // Push subscription handles real-time events; poll is for NTP calibration
            // and catching missed events.
            let mut poll_secs = 3u64;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(poll_secs));
            let mut push_connected = false;
            let mut consecutive_push_events = 0u32;

            loop {
                // Wait for either: push event, poll tick, or cancellation.
                // Push events arrive instantly; poll is the slow fallback for
                // NTP clock calibration and missed events.
                tokio::select! {
                    _ = cancel.notified() => {
                        info!("follow-poll: cancelled");
                        return;
                    }
                    push_event = push_rx.recv() => {
                        if let Some(mut event_json) = push_event {
                            // Forward push event directly to our Moosic client.
                            // Remap all track IDs for P2P streaming.
                            let is_party = event_json.contains("party_sync") || event_json.contains("party_queue_sync");
                            if is_party {
                                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&event_json) {
                                    // Remap playlist/queue arrays (party_queue_sync)
                                    for key in &["playlist", "queue"] {
                                        if let Some(arr) = val.get(*key).and_then(|v| v.as_array()) {
                                            let tracks: Vec<crate::social::protocol::PartyTrack> =
                                                arr.iter().filter_map(|t| serde_json::from_value(t.clone()).ok()).collect();
                                            let remapped = remap_party_tracks_remote(&host_node_id, &tracks);
                                            val[*key] = serde_json::to_value(&remapped).unwrap_or_default();
                                        }
                                    }
                                    // Remap single track (party_sync) — skip if already remote
                                    if let Some(track_val) = val.get("track").cloned() {
                                        if let Ok(mut t) = serde_json::from_value::<crate::social::protocol::PartyTrack>(track_val) {
                                            if !collab_playlist::is_remote_track_id(&t.song_id) {
                                                t.song_id = collab_playlist::encode_remote_track_id(&host_node_id, &t.song_id);
                                            }
                                            val["track"] = serde_json::to_value(&t).unwrap_or_default();
                                        }
                                    }
                                    event_json = val.to_string();
                                }
                            }
                            if is_party {
                                let _ = event_tx.send(event_json);
                                if !push_connected {
                                    push_connected = true;
                                    info!("follow-poll: push stream active, extending poll interval");
                                }
                                consecutive_push_events += 1;
                                // When push is reliably delivering, extend poll to 10s
                                // (only needed for NTP calibration)
                                if consecutive_push_events > 3 && poll_secs < 10 {
                                    poll_secs = 10;
                                    interval = tokio::time::interval(
                                        std::time::Duration::from_secs(poll_secs));
                                    debug!("follow-poll: adaptive interval → {}s (push healthy)", poll_secs);
                                }
                            }
                        }
                        continue; // Don't run the poll — wait for next event
                    }
                    _ = interval.tick() => {}
                }

                // Check if we're still following
                {
                    let ps = party.read().await;
                    if !ps.following.as_ref().map_or(false, |f| f.session_id == session_id) {
                        info!("follow-poll: no longer following, stopping");
                        return;
                    }
                }

                // NTP time ping — run concurrently with state poll
                let ping_fut = ntp_ping(&endpoint, host_pk);
                let poll_fut = poll_host_state(&endpoint, host_pk, &session_id);

                let (ping_result, poll_result) = tokio::join!(ping_fut, poll_fut);

                // Process NTP ping result
                if let Ok((t1, t2, t3, t4)) = ping_result {
                    let mut ps = party.write().await;
                    if let Some(ref mut f) = ps.following {
                        f.add_clock_sample(t1, t2, t3, t4);
                    }
                }

                // Read clock offset for events
                let clock_offset_ms = {
                    let ps = party.read().await;
                    ps.following.as_ref().map(|f| f.clock_offset_ms).unwrap_or(0)
                };

                match poll_result {
                    Ok(state) => {
                        if !state.found {
                            info!("follow-poll: host session ended");
                            let event = serde_json::json!({
                                "type": "party_end",
                                "session_id": &session_id,
                                "display_name": "",
                            });
                            let _ = event_tx.send(event.to_string());
                            let mut ps = party.write().await;
                            if ps.following.as_ref().map_or(false, |f| f.session_id == session_id) {
                                ps.unfollow();
                            }
                            return;
                        }

                        // Emit party_sync if seq advanced
                        if state.seq > last_seq {
                            last_seq = state.seq;
                            // Remap the current track ID for P2P streaming (skip if already remote)
                            let remote_track = state.track.map(|t| {
                                let mut rt = t;
                                if !collab_playlist::is_remote_track_id(&rt.song_id) {
                                    rt.song_id = collab_playlist::encode_remote_track_id(&host_node_id, &rt.song_id);
                                }
                                rt
                            });
                            let event = serde_json::json!({
                                "type": "party_sync",
                                "session_id": &session_id,
                                "seq": state.seq,
                                "host_timestamp_ms": party::now_ms(),
                                "dj_timestamp_ms": state.dj_timestamp_ms,
                                "clock_offset_ms": clock_offset_ms,
                                "state": state.state,
                                "track": remote_track,
                                "position_secs": state.position_secs,
                            });
                            let _ = event_tx.send(event.to_string());
                        }

                        // Emit party_queue_sync if playlist changed
                        let playlist_hash = {
                            use std::hash::{Hash, Hasher};
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            state.playlist.len().hash(&mut h);
                            state.playlist_index.hash(&mut h);
                            for t in &state.playlist { t.song_id.hash(&mut h); }
                            state.queue.len().hash(&mut h);
                            state.queue_index.hash(&mut h);
                            for t in &state.queue { t.song_id.hash(&mut h); }
                            state.playing_from_queue.hash(&mut h);
                            h.finish()
                        };
                        if playlist_hash != last_playlist_hash && !state.playlist.is_empty() {
                            last_playlist_hash = playlist_hash;
                            // Re-encode track IDs as remote so follower streams via P2P
                            let remote_playlist = remap_party_tracks_remote(&host_node_id, &state.playlist);
                            let remote_queue = remap_party_tracks_remote(&host_node_id, &state.queue);
                            let event = serde_json::json!({
                                "type": "party_queue_sync",
                                "session_id": &session_id,
                                "seq": state.seq,
                                "playlist": remote_playlist,
                                "playlist_index": state.playlist_index,
                                "queue": remote_queue,
                                "queue_index": state.queue_index,
                                "playing_from_queue": state.playing_from_queue,
                            });
                            let _ = event_tx.send(event.to_string());
                        }
                    }
                    Err(e) => {
                        debug!("follow-poll: query failed: {}", e);
                        // Push might be down — tighten poll interval
                        if poll_secs > 3 {
                            poll_secs = 3;
                            interval = tokio::time::interval(
                                std::time::Duration::from_secs(poll_secs));
                            push_connected = false;
                            consecutive_push_events = 0;
                            debug!("follow-poll: adaptive interval → {}s (poll failed)", poll_secs);
                        }
                    }
                }
            }
        });
    }

    /// Stop the follow-poll background task.
    pub fn stop_follow_poll(&self) {
        self.follow_poll_cancel.notify_waiters();
    }
}

/// Query a host's Fugue for the full party state via SUBSONIC_ALPN.
/// Uses the same HTTP-over-QUIC bridge that Moosic uses, which handles
/// long-lived connections correctly (unlike FUGUE_ALPN which drops after one request).
async fn poll_host_state(
    endpoint: &Endpoint,
    host_pk: iroh::PublicKey,
    _session_id: &str,
) -> Result<PollResult, Box<dyn std::error::Error + Send + Sync>> {
    let conn = endpoint
        .connect(host_pk, crate::social::node::SUBSONIC_ALPN)
        .await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    // Use the Subsonic bridge request format (same as Moosic and party-discover)
    let req = serde_json::json!({
        "endpoint": "admin/party-full-state",
        "params": {}
    });
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read the response header line (JSON + newline)
    let mut header_buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match recv.read(&mut byte).await? {
            Some(1) => {
                if byte[0] == b'\n' { break; }
                header_buf.push(byte[0]);
                if header_buf.len() > 16 * 1024 {
                    return Err("response header too large".into());
                }
            }
            _ => return Err("stream ended before header".into()),
        }
    }
    let header: serde_json::Value = serde_json::from_slice(&header_buf)?;
    let status = header["status"].as_u64().unwrap_or(0) as u16;
    if status != 200 {
        return Err(format!("HTTP {}", status).into());
    }

    // Read body
    let body_bytes = recv.read_to_end(4 * 1024 * 1024).await?;
    let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;

    let found = body.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
    if !found {
        return Ok(PollResult { found: false, ..PollResult::default() });
    }

    Ok(PollResult {
        found: true,
        seq: body.get("seq").and_then(|v| v.as_u64()).unwrap_or(0),
        state: body.get("state").and_then(|v| serde_json::from_value(v.clone()).ok()),
        track: body.get("track").and_then(|v| serde_json::from_value(v.clone()).ok()),
        position_secs: body.get("position_secs").and_then(|v| v.as_f64()).unwrap_or(0.0),
        dj_timestamp_ms: body.get("dj_timestamp_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        playlist: body.get("playlist").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
        playlist_index: body.get("playlist_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        queue: body.get("queue").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default(),
        queue_index: body.get("queue_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        playing_from_queue: body.get("playing_from_queue").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

#[derive(Default)]
struct PollResult {
    found: bool,
    seq: u64,
    state: Option<crate::social::protocol::PartyPlaybackState>,
    track: Option<crate::social::protocol::PartyTrack>,
    position_secs: f64,
    dj_timestamp_ms: u64,
    playlist: Vec<crate::social::protocol::PartyTrack>,
    playlist_index: usize,
    queue: Vec<crate::social::protocol::PartyTrack>,
    queue_index: usize,
    playing_from_queue: bool,
}

/// NTP-style time ping via QUIC to the host's Fugue.
/// Returns (T1, T2, T3, T4) timestamps in milliseconds.
/// NTP-style time ping via SUBSONIC_ALPN (admin/party-time-ping).
/// Returns (T1, T2, T3, T4) timestamps in milliseconds.
async fn ntp_ping(
    endpoint: &Endpoint,
    host_pk: iroh::PublicKey,
) -> Result<(u64, u64, u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    let t1 = party::now_ms();

    let conn = endpoint
        .connect(host_pk, crate::social::node::SUBSONIC_ALPN)
        .await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req = serde_json::json!({
        "endpoint": "admin/party-time-ping",
        "params": { "t1": t1.to_string() }
    });
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read response header line
    let mut header_buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match recv.read(&mut byte).await? {
            Some(1) => {
                if byte[0] == b'\n' { break; }
                header_buf.push(byte[0]);
            }
            _ => return Err("stream ended before header".into()),
        }
    }

    // Read body
    let body_bytes = recv.read_to_end(4096).await?;
    let t4 = party::now_ms();

    let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
    let t2 = body.get("t2").and_then(|v| v.as_u64()).unwrap_or(0);
    let t3 = body.get("t3").and_then(|v| v.as_u64()).unwrap_or(0);

    Ok((t1, t2, t3, t4))
}

/// Open a push event subscription to the host's Fugue via admin/events.
/// Returns the recv stream for reading newline-delimited JSON events.
async fn subscribe_host_events(
    endpoint: &Endpoint,
    host_pk: iroh::PublicKey,
) -> Result<iroh::endpoint::RecvStream, Box<dyn std::error::Error + Send + Sync>> {
    let conn = endpoint
        .connect(host_pk, crate::social::node::SUBSONIC_ALPN)
        .await?;
    let (mut send, recv) = conn.open_bi().await?;

    let req = serde_json::json!({
        "endpoint": "admin/events",
        "params": {}
    });
    let req_bytes = serde_json::to_vec(&req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    Ok(recv)
}
