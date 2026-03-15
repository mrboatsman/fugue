-- Collaborative playlists (shared between Fugue nodes via gossip)

CREATE TABLE IF NOT EXISTS collab_playlists (
    id          TEXT PRIMARY KEY,     -- shared UUID across all nodes
    name        TEXT NOT NULL,
    created_by  TEXT NOT NULL,        -- node_id of creator
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS collab_playlist_tracks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id TEXT NOT NULL,
    track_id    TEXT NOT NULL,         -- namespaced ID on the owner's node
    owner_node  TEXT NOT NULL,         -- node_id that owns/can stream this track
    title       TEXT NOT NULL,
    artist      TEXT,
    album       TEXT,
    duration    INTEGER,
    position    INTEGER NOT NULL,
    added_by    TEXT NOT NULL,         -- node_id that added this track
    added_at    TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (playlist_id) REFERENCES collab_playlists(id) ON DELETE CASCADE,
    UNIQUE(playlist_id, track_id, owner_node)
);

CREATE INDEX IF NOT EXISTS idx_collab_tracks_playlist ON collab_playlist_tracks(playlist_id, position);
