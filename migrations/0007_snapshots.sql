-- The latest reading of each service (src/integrations/poller.rs): one row per
-- service, overwritten on every fetch, so a restart shows the last values (with
-- their age) at once instead of an empty card. `json` is a serialised
-- `Snapshot`; `fetched_at` is its time (RFC 3339, UTC). The row goes with its
-- service.
CREATE TABLE service_snapshots (
    service_id INTEGER PRIMARY KEY REFERENCES services(id) ON DELETE CASCADE,
    json       TEXT NOT NULL,
    fetched_at TEXT NOT NULL
);
