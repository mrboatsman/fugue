-- Social layer: identity and friends

CREATE TABLE IF NOT EXISTS identity (
    key         TEXT PRIMARY KEY,
    value       BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS friends (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    public_key  TEXT NOT NULL UNIQUE,
    ticket      TEXT NOT NULL,
    added_at    TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen   TEXT
);

CREATE TABLE IF NOT EXISTS now_playing (
    user_name   TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    track_json  TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (node_id, user_name)
);

CREATE TABLE IF NOT EXISTS chat_messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id     TEXT NOT NULL,
    user_name   TEXT NOT NULL,
    message     TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
