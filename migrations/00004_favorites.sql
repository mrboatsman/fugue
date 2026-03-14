-- Fugue-local favorites (cross-backend)

CREATE TABLE IF NOT EXISTS favorites (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    owner       TEXT NOT NULL,        -- fugue username
    item_id     TEXT NOT NULL,        -- namespaced ID (song, album, or artist from any backend)
    item_type   TEXT NOT NULL,        -- 'song', 'album', 'artist'
    starred_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(owner, item_id)
);

CREATE INDEX IF NOT EXISTS idx_favorites_owner ON favorites(owner, item_type);
