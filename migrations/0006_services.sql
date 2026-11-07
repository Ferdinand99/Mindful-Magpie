-- Services: admin-managed instances of an integration kind (src/integrations).
-- `settings` is JSON without secrets; `secret` holds the one secret of the
-- service sealed with vole_kit::crypto (purpose magpie.service.secret) and is
-- never read back into a page. `visibility` says who sees the service: every
-- signed-in user, admins only, or admins plus the users in `service_users`.
CREATE TABLE services (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    name       TEXT NOT NULL,
    url        TEXT,
    settings   TEXT NOT NULL,
    secret     TEXT,
    interval_s INTEGER NOT NULL,
    visibility TEXT NOT NULL CHECK (visibility IN ('all', 'admin', 'users')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- The users who see a service whose visibility is 'users'. A row goes with its
-- service and with its user.
CREATE TABLE service_users (
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (service_id, user_id)
);

CREATE INDEX service_users_user ON service_users (user_id);
