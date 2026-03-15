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
use crate::social::protocol::{GossipMessage, RequestMessage, ResponseMessage};

/// A well-known topic ID for the Fugue social network.
fn fugue_topic() -> TopicId {
    let hash = blake3::hash(b"fugue-social-v0");
    TopicId::from(*hash.as_bytes())
}

/// Start the social P2P service.
pub async fn start(
    endpoint: Endpoint,
    db: SqlitePool,
    display_name: String,
    backends: Vec<BackendClient>,
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

    let handle = SocialHandle {
        sender: Arc::new(tokio::sync::Mutex::new(sender)),
        display_name: display_name.clone(),
    };

    // Spawn gossip receiver
    let db_clone = db.clone();
    let handle_sender = handle.sender.clone();
    tokio::spawn(async move {
        let mut receiver: GossipReceiver = receiver;
        loop {
            match receiver.next().await {
                Some(Ok(Event::Received(msg))) => {
                    if let Some(gossip_msg) = GossipMessage::from_bytes(&msg.content) {
                        let node_id = msg.delivered_from.to_string();
                        handle_gossip_message(&db_clone, &node_id, gossip_msg).await;
                    }
                }
                Some(Ok(Event::NeighborUp(peer))) => {
                    info!("social: peer connected: {}", peer);
                    let _ = friends::update_last_seen(&db_clone, &peer.to_string()).await;

                    // When a peer connects, send CRDT ops for all collab playlists
                    let playlists: Vec<(String,)> = sqlx::query_as(
                        "SELECT id FROM collab_playlists",
                    )
                    .fetch_all(&db_clone)
                    .await
                    .unwrap_or_default();

                    for (pid,) in playlists {
                        if let Ok(ops) = crdt::get_all_ops(&db_clone, &pid).await {
                            if !ops.is_empty() {
                                let msg = GossipMessage::CrdtSync {
                                    playlist_id: pid.clone(),
                                    ops,
                                };
                                if let Err(e) = handle_sender.lock().await.broadcast(msg.to_bytes()).await {
                                    debug!("social: CRDT sync broadcast failed for {}: {}", pid, e);
                                }
                            }
                        }
                    }
                }
                Some(Ok(Event::NeighborDown(peer))) => {
                    debug!("social: peer disconnected: {}", peer);
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
    tokio::spawn(async move {
        loop {
            match endpoint_clone.accept().await {
                Some(incoming) => {
                    let db = db_clone2.clone();
                    let gossip = gossip_clone.clone();
                    let backends = backends_arc.clone();
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
                                    if let Err(e) = handle_connection(conn, &db, &backends).await {
                                        debug!("social: fugue connection error: {}", e);
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

    // Spawn periodic friend address refresh (picks up newly added friends)
    let db_clone3 = db.clone();
    let endpoint_clone2 = endpoint.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            refresh_friend_addresses(&db_clone3, &endpoint_clone2).await;
        }
    });

    info!("social service: started (display_name={})", display_name);
    Ok(handle)
}

/// Re-read friends from DB and register any new addresses with the endpoint.
async fn refresh_friend_addresses(db: &SqlitePool, endpoint: &Endpoint) {
    let friend_list = friends::list_friends(db).await.unwrap_or_default();
    let memory_lookup = iroh::address_lookup::MemoryLookup::default();
    let mut count = 0;

    for f in &friend_list {
        if let Ok(addr) = crate::social::node::parse_ticket(&f.ticket) {
            memory_lookup.add_endpoint_info(addr);
            count += 1;
        }
    }

    if count > 0 {
        endpoint.address_lookup().add(memory_lookup);
        debug!("social: refreshed {} friend addresses", count);
    }
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
            for track in tracks {
                let _ = collab_playlist::add_track(db, playlist_id, track).await;
            }
        }
    }
}

async fn handle_connection(
    conn: iroh::endpoint::Connection,
    db: &SqlitePool,
    backends: &[BackendClient],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut send, mut recv) = conn.accept_bi().await?;

    let request_bytes = recv.read_to_end(64 * 1024).await?;
    let request: RequestMessage = serde_json::from_slice(&request_bytes)?;

    debug!("social: direct request: {:?}", request);

    match request {
        RequestMessage::StreamTrack { track_id } => {
            debug!("social: stream request for track {}", track_id);
            if let Err(e) = stream_track_for_peer(backends, &track_id, &mut send).await {
                error!("social: stream failed for {}: {}", track_id, e);
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
        RequestMessage::StreamTrack { .. } => unreachable!(),
    };

    let response_bytes = serde_json::to_vec(&response)?;
    send.write_all(&response_bytes).await?;
    send.finish()?;

    Ok(())
}

/// Stream a track from our backends to a requesting peer.
/// Writes directly to the QUIC send stream to avoid buffering the whole file.
async fn stream_track_for_peer(
    backends: &[BackendClient],
    track_id: &str,
    send: &mut iroh::endpoint::SendStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (backend_idx, original_id) = crate::id::decode_id(track_id)?;

    let backend = backends
        .iter()
        .find(|b| b.index == backend_idx)
        .ok_or_else(|| format!("Backend {} not found", backend_idx))?;

    debug!("social: streaming track {} from backend {}", original_id, backend.name);

    let resp = backend
        .request_stream("stream", &[("id", &original_id)])
        .await
        .map_err(|e| format!("Backend stream failed: {e}"))?;

    // Stream chunks from backend to QUIC send stream
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Read chunk: {e}"))?;
        send.write_all(&chunk).await.map_err(|e| format!("Write to peer: {e}"))?;
        total += chunk.len();
    }
    send.finish().map_err(|e| format!("Finish send: {e}"))?;

    debug!("social: streamed {} bytes for track {}", total, track_id);
    Ok(())
}

/// Handle for interacting with the social service from HTTP handlers.
#[derive(Clone)]
pub struct SocialHandle {
    sender: Arc<tokio::sync::Mutex<GossipSender>>,
    display_name: String,
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
}
