-- Personal services: a service that belongs to one user (src/services). NULL is
-- a shared service set up by an admin, as before. A personal service is seen,
-- changed, tested and fetched for its user only, whatever the viewer's role;
-- its row (and with it its snapshot and acknowledgements) goes with the user.
-- Its `visibility` stays 'users' with no `service_users` rows, so nothing but
-- the check on `owner_user_id` could ever show it to someone else.
ALTER TABLE services ADD COLUMN owner_user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
CREATE INDEX services_owner ON services (owner_user_id);
