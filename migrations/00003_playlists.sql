-- Fugue-local playlists (cross-backend)

CREATE TABLE IF NOT EXISTS playlists (
    id          TEXT PRIMARY KEY,  -- uuid
    name        TEXT NOT NULL,
    comment     TEXT NOT NULL DEFAULT '',
    public      BOOLEAN NOT NULL DEFAULT 0,
    owner       TEXT NOT NULL,     -- fugue username
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS playlist_tracks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id TEXT NOT NULL,
    track_id    TEXT NOT NULL,     -- namespaced ID (can be from any backend)
    position    INTEGER NOT NULL,
    added_at    TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_playlist_tracks_playlist ON playlist_tracks(playlist_id, position);
