-- Cache tables for Phase 3

CREATE TABLE IF NOT EXISTS backends (
    idx         INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    url         TEXT NOT NULL,
    last_synced TEXT
);

CREATE TABLE IF NOT EXISTS artists (
    id          TEXT PRIMARY KEY,  -- namespaced ID
    backend_idx INTEGER NOT NULL,
    original_id TEXT NOT NULL,
    name        TEXT NOT NULL,
    name_norm   TEXT NOT NULL,     -- lowercased, trimmed for matching
    album_count INTEGER DEFAULT 0,
    data_json   TEXT NOT NULL,     -- full JSON blob from backend
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (backend_idx) REFERENCES backends(idx)
);

CREATE TABLE IF NOT EXISTS albums (
    id          TEXT PRIMARY KEY,  -- namespaced ID
    backend_idx INTEGER NOT NULL,
    original_id TEXT NOT NULL,
    name        TEXT NOT NULL,
    name_norm   TEXT NOT NULL,
    artist      TEXT,
    artist_id   TEXT,              -- namespaced
    year        INTEGER,
    genre       TEXT,
    song_count  INTEGER DEFAULT 0,
    duration    INTEGER DEFAULT 0,
    data_json   TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (backend_idx) REFERENCES backends(idx)
);

CREATE TABLE IF NOT EXISTS tracks (
    id          TEXT PRIMARY KEY,  -- namespaced ID
    backend_idx INTEGER NOT NULL,
    original_id TEXT NOT NULL,
    title       TEXT NOT NULL,
    title_norm  TEXT NOT NULL,
    artist      TEXT,
    album       TEXT,
    album_id    TEXT,              -- namespaced
    track_number INTEGER,
    duration    INTEGER,
    bitrate     INTEGER,
    content_type TEXT,
    suffix      TEXT,
    data_json   TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (backend_idx) REFERENCES backends(idx)
);

CREATE TABLE IF NOT EXISTS cache_meta (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_artists_backend ON artists(backend_idx);
CREATE INDEX IF NOT EXISTS idx_artists_name_norm ON artists(name_norm);
CREATE INDEX IF NOT EXISTS idx_albums_backend ON albums(backend_idx);
CREATE INDEX IF NOT EXISTS idx_albums_name_norm ON albums(name_norm);
CREATE INDEX IF NOT EXISTS idx_albums_artist_id ON albums(artist_id);
CREATE INDEX IF NOT EXISTS idx_tracks_backend ON tracks(backend_idx);
CREATE INDEX IF NOT EXISTS idx_tracks_title_norm ON tracks(title_norm);
CREATE INDEX IF NOT EXISTS idx_tracks_album_id ON tracks(album_id);
