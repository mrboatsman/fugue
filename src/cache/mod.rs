//! Background caching layer for merged library data.
//!
//! Fugue caches artists, albums, and tracks from all backends in a local
//! SQLite database. This allows browsing and search endpoints to respond
//! instantly without hitting backends on every request.
//!
//! # Refresh Cycle
//!
//! A background task crawls all backends on startup and periodically after
//! that (interval configurable via `cache.refresh_interval_secs`). Each
//! crawl fetches the full library from every backend and stores it with
//! pre-namespaced IDs.
//!
//! # Freshness
//!
//! The cache freshness window is **2x the refresh interval**. Within this
//! window, browsing and search requests (`getArtists`, `getAlbumList2`,
//! `search2`, `search3`) are served directly from cache. If the cache is
//! stale or empty, requests fall back to live fan-out transparently.
//!
//! # Offline Resilience
//!
//! Backends going temporarily offline don't break browsing — cached data
//! is still served. When the backend comes back, the next refresh cycle
//! picks up any changes.
//!
//! # Submodules
//!
//! - [`db`] — SQLite read/write operations for cached entities
//! - [`refresh`] — background task that drives the crawl cycle

pub mod db;
pub mod refresh;
