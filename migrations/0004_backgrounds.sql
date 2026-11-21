-- One uploaded start page background per user: only the metadata is here, the
-- picture itself is a file under <data_dir>/backgrounds/<user_id>/. The row goes
-- with its user; the file is removed by the sweep in src/layout/background.rs.
-- `id` is a prefix of the SHA-256 of the content (hex), `bytes` the size.
CREATE TABLE backgrounds (
    user_id    INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    id         TEXT NOT NULL,
    mime       TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
