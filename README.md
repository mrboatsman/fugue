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

### Multi-Backend Aggregation
- Fan-out browsing and search requests to all backends in parallel
- Merge artist indexes, album lists, search results, and genres across servers
- Transparent ID namespacing — every item carries an opaque ID that Fugue can 
  route back to the correct backend

### Proxy-Local Playlists
- Playlists are stored in Fugue's own SQLite database
- A single playlist can contain tracks from multiple backends
- Remote playlists from backends are also visible alongside local ones
- Full CRUD: create, update, delete, add/remove tracks

### Proxy-Local Favorites
- Stars (favorites) are stored locally, consistent across all backends
- Starring a song auto-stars its parent album
- `getStarred2` and `getAlbumList2?type=starred` both serve from local favorites
- No dependency on backend-specific favorite state

### Background Caching
- A background task crawls all backends on startup and periodically after that
- Artists, albums, and tracks are stored in SQLite with pre-namespaced IDs
- Browsing (`getArtists`, `getAlbumList2`) and search (`search2`, `search3`)
  are served instantly from cache when fresh
- If the cache is stale or empty, requests fall back to live fan-out transparently
- Backends going temporarily offline don't break browsing — cached data is still
  served
- Refresh interval is configurable via `refresh_interval_secs` in `[cache]`
- Cache freshness window is 2x the refresh interval before falling back to
  live queries

### Deduplication
When the same music exists on multiple backends, Fugue detects the overlap and
presents a single unified library with no duplicates.

**How it works:**
- After each cache refresh, Fugue fingerprints every track and album using
  normalized metadata (`artist :: album :: title :: track_number`)
- Normalization is case-insensitive and strips noise like `(Remastered 2011)`,
  `[Deluxe Edition]`, etc., so the same album with slightly different names
  still matches
- Duplicate groups are stored in SQLite with a score per source based on
  bitrate, format quality (FLAC > Opus > MP3), and backend `weight`

**What the client sees:**
- Album lists and artist lists only show one copy of each duplicate — the
  highest-scored version
- When an artist exists on multiple backends (e.g. 3 albums on one, 5 on
  another with 2 overlapping), opening the artist merges albums from all
  backends and deduplicates, so the client sees the full combined discography
- Streaming automatically picks the best available source; if that backend
  goes down, Fugue falls back to the next best

**Example:**
```
Backend 1 (weight=10): Artist X → Album A, Album B, Album C
Backend 2 (weight=5):  Artist X → Album B, Album C, Album D, Album E

Client sees: Artist X → Album A, B, C, D, E  (5 albums, no duplicates)
Streaming Album B → picks Backend 1 (higher weight + same bitrate)
```

### Stream Proxying
- Audio streaming, downloads, and cover art are proxied from the correct backend
- Zero-buffer passthrough — bytes flow directly from backend to client
- Transcoding parameters (maxBitRate, format) are forwarded transparently

### Client Compatibility
- Supports both GET and POST requests
  (form-urlencoded body params merged automatically)
- All endpoints registered with both `/rest/X` and `/rest/X.view` paths
- XML and JSON response formats
- Tested with Sonixd, play:Sub, SubTUI, and other Subsonic clients

### Authentication
- Fugue has its own user/password list (configured in TOML)
- Clients authenticate against Fugue using standard Subsonic token+salt
  or plaintext auth
- Fugue authenticates to each backend independently using stored credentials

### Configurable Logging
- Log level configurable in
  `fugue.toml` (`error`, `warn`, `info`, `debug`, `trace`)
- `info`: minimal — server start, backend count, DB init
- `debug`: verbose — every request, routing decisions,
   fan-out results, DB operations

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
port = 4535
log_level = "info"

[auth]
[[auth.users]]
username = "admin"
password = "your-password"

[[backends]]
name = "my-server"
url = "http://localhost:4533"
username = "navidrome-user"
password = "navidrome-pass"
weight = 10  # higher = preferred source for deduplicated tracks

[[backends]]
name = "friend"
url = "http://friend.example.com:4533"
username = "shared-user"
password = "shared-pass"
weight = 5   # lower weight = fallback source

[cache]
db_path = "fugue.db"

[social]
enabled = true
display_name = "Anders"
```

#### Backend Weight

The `weight` value on each backend controls source preference when the same track
exists on multiple servers (deduplication). When Fugue detects duplicate tracks
across backends, it picks the source with the highest weight for streaming.
If that source is unavailable, it falls back to the next highest.

- Higher weight = preferred source
- Default is `0` if omitted
- Only matters when the same content exists on multiple backends

### Run with Cargo

```bash
cargo run
```

### Run with Docker

```bash
docker compose up -d
```

Or standalone:

```bash
docker run -d -p 4535:4533 -v ./fugue.toml:/etc/fugue/fugue.toml:ro fugue
```

### Check Backend Connectivity

```bash
cargo run -- check
```

Then point your Subsonic client at `http://your-host:4535` with the credentials
from `[auth.users]`.

### Social / P2P

Fugue includes an optional P2P social layer powered by
[Iroh](https://iroh.computer). Connect directly with friends to share
libraries, playlists, and listening activity — no ports to open, no backend
credentials to share.

Enable it in your config:
```toml
[social]
enabled = true
display_name = "Anders"
```

Then use the CLI to manage friends:
```bash
# Show your ticket (give this to friends)
fugue ticket

# Add a friend
fugue friend add --name "Pelle" <ticket>

# List friends and their status
fugue friend list

# Remove a friend
fugue friend remove Pelle
```

When social is enabled, Fugue generates a persistent identity (ed25519 keypair)
stored in the database. Friends connect directly via Iroh's QUIC transport with
automatic NAT traversal — works behind firewalls without any port forwarding.



#### Colaborative playlists
How the CRDT works

Operation log: Every change to a collaborative playlist is stored as an operation in crdt_ops:
- AddTrack — adds a track with metadata
- RemoveTrack — removes a track
- SetName — renames the playlist
- Each op has a unique op_id ({node_id}:{lamport_timestamp}) so duplicates are ignored

Materialized view: After operations are stored, rebuild_playlist replays all ops in timestamp order to compute the current track list. This is what getPlaylist serves.

Sync:
- When any change happens, the new CRDT ops are broadcast via gossip
- When a peer connects (NeighborUp), ALL ops for all playlists are broadcast
- merge_ops is idempotent — same op arriving twice is ignored (INSERT OR IGNORE)
- After merging, the materialized view is rebuilt

```
Offline resilience:
Node A adds track X (offline)     Node B adds track Y (offline)
  op: A:1 AddTrack(X)               op: B:1 AddTrack(Y)
                  \                 /
                   --- reconnect ---
                  /                 \
  receives B:1, merges              receives A:1, merges
  rebuild: [X, Y]                   rebuild: [X, Y]
  ✓ both converge                   ✓ both converge

```
Both nodes end up with the same playlist — no data loss, no conflicts.



## Tech Stack

- **Rust** + **axum** + **tokio** — async HTTP server
- **reqwest** — HTTP client with connection pooling (rustls)
- **sqlx** + **SQLite** — local playlists, favorites, and migration management
- **serde** + **serde_json** — Subsonic JSON parsing and response serialization
- **iroh** — P2P networking (QUIC, CRDT documents, gossip) for social features
- **figment** — TOML + environment variable config
- **tracing** — structured logging
- **clap** — CLI
