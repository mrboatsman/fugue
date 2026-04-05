//! P2P social layer for Fugue.
//!
//! This module provides an optional peer-to-peer social layer powered by
//! [Iroh](https://iroh.computer). It enables direct connections between Fugue
//! instances for sharing libraries, playlists, and listening activity.
//!
//! # Architecture
//!
//! The social layer uses Iroh's QUIC transport with automatic NAT traversal,
//! so it works behind firewalls without any port forwarding. Connections are
//! established via **ticket exchange**: each node generates a ticket containing
//! its public key and relay/address information. Friends exchange tickets
//! out-of-band (e.g. messaging) and add them via the CLI.
//!
//! # Identity
//!
//! Each Fugue instance has a persistent ed25519 keypair stored in the SQLite
//! database. This keypair is created on first run and reused across restarts,
//! giving the node a stable identity. The public key serves as the node ID.
//!
//! # Ticket Flow
//!
//! 1. Alice runs `fugue ticket` to get her ticket string
//! 2. Alice sends the ticket to Bob (out-of-band)
//! 3. Bob runs `fugue friend add --name "Alice" <ticket>`
//! 4. Bob's node connects to Alice directly via Iroh (QUIC + relay)
//! 5. Both nodes exchange library metadata and CRDT operations
//!
//! # No Credentials Shared
//!
//! Friends never see each other's backend credentials. Fugue proxies
//! streams on behalf of friends — the P2P layer transfers audio data
//! directly, not Subsonic API credentials.
//!
//! # Submodules
//!
//! - [`node`] — Iroh endpoint creation, keypair management, ticket generation
//! - [`friends`] — friend list persistence (SQLite)
//! - [`service`] — background service that manages connections and gossip
//! - [`protocol`] — wire protocol for P2P messages
//! - [`library`] — shared library metadata exchange
//! - [`activity`] — listening activity (now playing) sharing
//! - [`bandwidth`] — adaptive bitrate measurement for P2P streaming
//! - [`collab_playlist`] — collaborative playlist management
//! - [`crdt`] — OR-Set CRDT for collaborative playlist sync

pub mod node;
pub mod friends;
pub mod library;
pub mod activity;
pub mod protocol;
pub mod service;
pub mod bandwidth;
pub mod collab_playlist;
pub mod crdt;
pub mod subsonic_bridge;
