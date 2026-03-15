-- Collaborative playlist membership with roles

CREATE TABLE IF NOT EXISTS collab_playlist_members (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    name        TEXT NOT NULL,        -- display name
    role        TEXT NOT NULL,        -- 'owner', 'collab', 'viewer'
    joined_at   TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (playlist_id) REFERENCES collab_playlists(id) ON DELETE CASCADE,
    UNIQUE(playlist_id, node_id)
);
