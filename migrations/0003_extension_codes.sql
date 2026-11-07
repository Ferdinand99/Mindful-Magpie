-- One-time codes of the browser extension's "Koble til Magpie" flow. The code
-- itself is never stored, only its SHA-256, together with the PKCE challenge
-- the extension sent. A code lives two minutes and is deleted when it is used
-- (or tried). `expires_at` is a Unix time in seconds.
CREATE TABLE extension_codes (
    code_hash      TEXT PRIMARY KEY,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_challenge TEXT NOT NULL,
    device         TEXT NOT NULL,
    expires_at     INTEGER NOT NULL
);
CREATE INDEX extension_codes_user ON extension_codes (user_id);
CREATE INDEX extension_codes_expires ON extension_codes (expires_at);
