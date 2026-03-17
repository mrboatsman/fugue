-- Playback reporting: track position and state per user

CREATE TABLE IF NOT EXISTS playback_reports (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_name   TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    media_id    TEXT NOT NULL,
    position_ms INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL DEFAULT 'playing',  -- playing, paused, stopped
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(node_id, user_name, media_id)
);
