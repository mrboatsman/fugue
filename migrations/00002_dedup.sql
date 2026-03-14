-- Deduplication tables for Phase 4

CREATE TABLE IF NOT EXISTS dedup_groups (
    fingerprint TEXT PRIMARY KEY,   -- normalized metadata fingerprint
    canonical_id TEXT NOT NULL,     -- synthetic namespaced ID (d:hash)
    entity_type TEXT NOT NULL,      -- 'track', 'album', or 'artist'
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS dedup_members (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT NOT NULL,
    namespaced_id TEXT NOT NULL,    -- original namespaced ID of the member
    backend_idx INTEGER NOT NULL,
    bitrate     INTEGER,
    format      TEXT,
    score       REAL DEFAULT 0.0,  -- computed quality score
    FOREIGN KEY (fingerprint) REFERENCES dedup_groups(fingerprint),
    UNIQUE(fingerprint, namespaced_id)
);

CREATE INDEX IF NOT EXISTS idx_dedup_members_fingerprint ON dedup_members(fingerprint);
CREATE INDEX IF NOT EXISTS idx_dedup_members_namespaced ON dedup_members(namespaced_id);
