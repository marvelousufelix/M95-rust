-- Session revocation table for logout support.
-- Tracks revoked JWT sessions by session_id and expiry.
-- Entries older than 24 hours are safe to delete per TOKEN_TTL_HOURS.

CREATE TABLE revoked_sessions (
  session_id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id),
  revoked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL
);

-- Index for cleanup queries: remove sessions older than 24 hours.
CREATE INDEX idx_revoked_sessions_expires_at ON revoked_sessions(expires_at);
