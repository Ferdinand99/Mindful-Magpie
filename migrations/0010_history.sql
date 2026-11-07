-- Magpie's own history of every number a service reports (src/history.rs), so
-- a card can draw a curve without the remote app keeping any history.
--
-- `service_history` holds the last 24 hours, at most 288 points per metric: a
-- service polled faster than every 5 minutes has one point per 5-minute slot
-- (`at` is the slot's start, the latest reading of the slot wins), a slower one
-- has a point per reading. `at` is Unix seconds. Older rows are deleted in the
-- write that adds a newer one.
--
-- `service_history_daily` keeps one row per metric and UTC day for 30 days:
-- the lowest, highest and mean reading of the day (`n` readings made the mean).
--
-- Both tables go with their service.
CREATE TABLE service_history (
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    metric     TEXT    NOT NULL,
    at         INTEGER NOT NULL,
    value      REAL    NOT NULL,
    PRIMARY KEY (service_id, metric, at)
) WITHOUT ROWID;

CREATE TABLE service_history_daily (
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    metric     TEXT    NOT NULL,
    day        TEXT    NOT NULL,
    min        REAL    NOT NULL,
    max        REAL    NOT NULL,
    avg        REAL    NOT NULL,
    n          INTEGER NOT NULL,
    PRIMARY KEY (service_id, metric, day)
) WITHOUT ROWID;
