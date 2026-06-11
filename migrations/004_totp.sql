-- Schema v2.1 — 2FA TOTP (RFC 6238 / 4226).
-- Adds: users.totp_enabled, users.totp_secret (null until enabled).
-- Backup codes are derived but stored hashed for single-use revocation.

ALTER TABLE users ADD COLUMN totp_enabled  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN totp_secret   TEXT;
ALTER TABLE users ADD COLUMN totp_verified_at INTEGER;

CREATE TABLE IF NOT EXISTS user_backup_codes (
  user_id    TEXT NOT NULL REFERENCES users(id),
  code_hash  TEXT NOT NULL,
  used_at    INTEGER,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, code_hash)
);
CREATE INDEX IF NOT EXISTS idx_backup_user ON user_backup_codes(user_id);
