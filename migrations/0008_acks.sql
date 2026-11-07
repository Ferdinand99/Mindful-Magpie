-- «Trenger deg» acknowledgements (src/needs.rs): one row per user, service and
-- need key. `level` and `score` are what the need had when the user
-- acknowledged it (taken from the server's latest snapshot, never from the
-- client): the need shows again when its level or score has risen since. `acked_at`
-- is the time (RFC 3339, UTC). The poller deletes the rows of a need that has
-- gone from a service's snapshot, so a need that comes back shows again. The
-- rows go with the user and with the service.
CREATE TABLE acks (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    need_key   TEXT NOT NULL,
    level      TEXT NOT NULL CHECK (level IN ('warn', 'bad')),
    score      REAL NOT NULL,
    acked_at   TEXT NOT NULL,
    PRIMARY KEY (user_id, service_id, need_key)
);
CREATE INDEX acks_service ON acks (service_id);
