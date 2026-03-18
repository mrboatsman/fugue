DISCLAIMER: This is a vibe coded project I take no pride in this,
it just solved an issue for me

# Fugue

A smart Subsonic API proxy that merges multiple Navidrome instances into one
unified music library. Connect any Subsonic-compatible client to Fugue and
browse, search, and stream music from all your servers,
yours and your friends', as if it were a single collection.

> *In music, a **fugue** is a compositional technique where multiple
> independent voices enter one by one, each carrying the same subject, weaving
> together into a unified whole. Each voice is distinct, yet together they form
> a single coherent piece. — That's exactly what this proxy does: multiple
> independent Navidrome servers, each with their own library, woven together
> into one seamless music collection.*

```
Clients (Sonixd, DSub, Amperfy, play:Sub, SubTUI, etc.)
        |
        v
   +-----------+
   |   Fugue   |  <-- Subsonic API proxy
   |   Proxy   |
   +-----+-----+
         | Subsonic API calls
   +---------------+-----------+
   v               v           v
Navidrome A   Navidrome B   Navidrome C
(yours)       (friend 1)    (friend 2)
```

Navidrome handles indexing, transcoding, and storage. Fugue handles merging,
routing, and local state.

## Features

- **Multi-backend aggregation** — fan-out requests to all backends, merge results
- **Deduplication** — same album on multiple servers appears once, best source picked automatically
- **Proxy-local playlists** — playlists stored in Fugue's DB, can span multiple backends
- **Proxy-local favorites** — stars stored locally, consistent across all backends
- **Background caching** — browsing and search served from SQLite cache
- **Stream proxying** — zero-buffer passthrough from correct backend to client
- **P2P social** — optional Iroh-based layer for sharing libraries with friends
- **Collaborative playlists** — CRDT-based playlists that sync across peers
- **Client compatibility** — GET/POST, XML/JSON, `.view` paths, token+salt and plaintext auth

For technical details on any feature, run `cargo doc --no-deps --open`.

## Subsonic API Coverage

| Category   | Endpoints |
|------------|-----------|
| System     | `ping`, `getLicense`, `getScanStatus`, `getUser` |
| Browsing   | `getArtists`, `getArtist`, `getAlbum`, `getSong`, `getIndexes`, `getMusicFolders`, `getGenres` |
| Info       | `getArtistInfo`, `getArtistInfo2`, `getAlbumInfo`, `getAlbumInfo2`, `getSimilarSongs`, `getSimilarSongs2`, `getTopSongs`, `getLyrics` |
| Search     | `search2`, `search3` |
| Lists      | `getAlbumList`, `getAlbumList2`, `getRandomSongs`, `getStarred`, `getStarred2` |
| Media      | `stream`, `download`, `getCoverArt` |
| Playlists  | `getPlaylists`, `getPlaylist`, `createPlaylist`, `updatePlaylist`, `deletePlaylist` |
| Annotation | `star`, `unstar`, `setRating`, `scrobble` |
| Bookmarks  | `getBookmarks`, `createBookmark`, `deleteBookmark` |
| Other      | `getNowPlaying`, `getPlayQueue`, `savePlayQueue`, `getInternetRadioStations` |

## Getting Started

### Configuration

Copy the example config and edit it:

```bash
cp fugue.toml.example fugue.toml
```

```toml
[server]
host = "0.0.0.0"
port = 4533

[auth]
[[auth.users]]
username = "admin"
password = "your-password"

[[backends]]
name = "my-server"
url = "http://localhost:4533"
username = "navidrome-user"
password = "navidrome-pass"
weight = 10
```

All settings can be overridden via environment variables with the `FUGUE_` prefix
(e.g. `FUGUE_SERVER_PORT=4535`).

### Build & Run

```bash
cargo build --release
cargo run
```

Or with Docker:

```bash
docker compose up -d
```

Or standalone Docker:

```bash
touch fugue.db
docker run -d -p 4533:4533 -v ./fugue.toml:/etc/fugue/fugue.toml:ro -v ./ ghcr.io/mrboatsman/fugue:latest
```

### Connect a Client

Point your Subsonic client at `http://your-host:4533` using the credentials
from `[auth.users]` in your config.

### Enable Social / P2P

```toml
[social]
enabled = true
display_name = "Anders"
```

Then use the CLI to exchange tickets and manage friends (see CLI Commands below).
For technical details on the P2P architecture, see `cargo doc` for the `social` module.

## CLI Commands

| Command | Description |
|---------|-------------|
| `fugue serve` | Start the proxy server (default) |
| `fugue check` | Check backend connectivity |
| `fugue sync` | Force a cache refresh on the running server |
| `fugue ticket` | Show your Fugue ticket for sharing with friends |
| `fugue status` | Show status of backends, cache, and social network |
| `fugue friend add --name <name> <ticket>` | Add a friend by ticket |
| `fugue friend remove <name>` | Remove a friend |
| `fugue friend list` | List all friends |
| `fugue api-key create --user <user> [--label <label>]` | Create a new API key |
| `fugue api-key list --user <user>` | List API keys for a user |
| `fugue api-key revoke <hash_prefix>` | Revoke an API key |
| `fugue playlist create <name>` | Create a collaborative playlist |
| `fugue playlist invite <id> [--role collab\|viewer]` | Generate invite code |
| `fugue playlist join <code>` | Join a collaborative playlist |
| `fugue playlist list` | List collaborative playlists |
| `fugue playlist members <id>` | Show members of a playlist |
| `fugue playlist leave <id>` | Leave a collaborative playlist |
| `fugue playlist sync <id>` | Force sync a playlist |

## Settings Reference

| Setting | Env Var | Default | Description |
|---------|---------|---------|-------------|
| `server.host` | `FUGUE_SERVER_HOST` | `0.0.0.0` | Listen address |
| `server.port` | `FUGUE_SERVER_PORT` | `4533` | Listen port |
| `server.log_level` | `FUGUE_SERVER_LOG_LEVEL` | `info` | Log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `auth.users[].username` | — | — | Proxy login username |
| `auth.users[].password` | — | — | Proxy login password |
| `backends[].name` | — | — | Display name for this backend |
| `backends[].url` | — | — | Backend Subsonic API URL |
| `backends[].username` | — | — | Backend login username |
| `backends[].password` | — | — | Backend login password |
| `backends[].weight` | — | `0` | Source preference for dedup (higher = preferred) |
| `cache.db_path` | `FUGUE_CACHE_DB_PATH` | `fugue.db` | SQLite database path |
| `cache.refresh_interval_secs` | `FUGUE_CACHE_REFRESH_INTERVAL_SECS` | `300` | Seconds between cache refreshes |
| `social.enabled` | `FUGUE_SOCIAL_ENABLED` | `false` | Enable P2P social layer |
| `social.display_name` | `FUGUE_SOCIAL_DISPLAY_NAME` | `Fugue User` | Your name visible to friends |
| `social.streaming.max_serve_bitrate` | `FUGUE_SOCIAL_STREAMING_MAX_SERVE_BITRATE` | `0` | Max bitrate (kbps) to serve friends (`0` = no limit) |
| `social.streaming.serve_format` | `FUGUE_SOCIAL_STREAMING_SERVE_FORMAT` | `raw` | Format for friends (`raw` = original) |
| `social.streaming.preferred_bitrate` | `FUGUE_SOCIAL_STREAMING_PREFERRED_BITRATE` | `0` | Preferred bitrate from friends (`0` = auto/adaptive) |
| `social.streaming.preferred_format` | `FUGUE_SOCIAL_STREAMING_PREFERRED_FORMAT` | `auto` | Preferred format from friends (`auto` = accept any) |

## Tech Stack

- **Rust** + **axum** + **tokio** — async HTTP server
- **reqwest** — HTTP client with connection pooling (rustls)
- **sqlx** + **SQLite** — local playlists, favorites, and migration management
- **serde** + **serde_json** — Subsonic JSON parsing and response serialization
- **iroh** — P2P networking (QUIC, CRDT documents, gossip) for social features
- **figment** — TOML + environment variable config
- **tracing** — structured logging
- **clap** — CLI
