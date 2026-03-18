//! Subsonic API proxy layer.
//!
//! This module implements the core proxy that sits between Subsonic clients
//! and multiple Navidrome backends.
//!
//! # Fan-Out Pattern
//!
//! For browsing and search requests, Fugue sends the request to all backends
//! in parallel and merges the results. This is handled by the [`fanout`]
//! module — responses are collected concurrently via `tokio::join` and
//! combined into a single Subsonic response.
//!
//! # ID Namespacing
//!
//! Every item ID returned to clients is an opaque, namespaced token that
//! encodes both the backend index and the original ID (see [`crate::id`]).
//! When a client sends an ID back (e.g. to stream a song), Fugue decodes
//! it to determine which backend to route the request to. Clients never
//! see raw backend IDs.
//!
//! # Stream Proxying
//!
//! Audio streaming, downloads, and cover art are proxied via zero-buffer
//! passthrough — bytes flow directly from the backend HTTP response to the
//! client response with no intermediate buffering. Transcoding parameters
//! (`maxBitRate`, `format`) are forwarded transparently to the backend.
//!
//! # Submodules
//!
//! - [`backend`] — `BackendClient` for making authenticated Subsonic API calls
//! - [`fanout`] — parallel request dispatch and response merging
//! - [`router`] — axum router wiring for all Subsonic endpoints
//! - [`stream`] — streaming proxy (audio, downloads, cover art)

pub mod backend;
pub mod fanout;
pub mod router;
pub mod stream;
