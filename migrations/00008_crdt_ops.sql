-- CRDT operation log for collaborative playlists

CREATE TABLE IF NOT EXISTS crdt_ops (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id TEXT NOT NULL,
    op_id       TEXT NOT NULL,         -- unique: "{node_id}:{lamport_clock}"
    timestamp   INTEGER NOT NULL,      -- lamport timestamp
    origin_node TEXT NOT NULL,         -- node that created this op
    op_json     TEXT NOT NULL,         -- serialized CrdtOpKind
    received_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(playlist_id, op_id)
);

CREATE INDEX IF NOT EXISTS idx_crdt_ops_playlist ON crdt_ops(playlist_id, timestamp);
