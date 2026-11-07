-- One start page layout per user: a JSON document with a version number for
-- optimistic concurrency (a save must carry the version it was loaded at).
CREATE TABLE layouts (
    user_id    INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    version    INTEGER NOT NULL,
    doc        TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
