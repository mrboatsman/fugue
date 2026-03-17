-- API key authentication for OpenSubsonic

CREATE TABLE IF NOT EXISTS api_keys (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash    TEXT NOT NULL UNIQUE,   -- SHA-256 hash of the key
    username    TEXT NOT NULL,          -- which user this key belongs to
    label       TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    last_used   TEXT
);
