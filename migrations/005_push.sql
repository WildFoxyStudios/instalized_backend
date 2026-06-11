-- Schema v2.2 — push delivery (FCM HTTP v1) idempotency.
-- Tracks per-notification push-send state. Notifications stay "pending push"
-- until the worker has either shipped them or marked them dead-letter.

ALTER TABLE notifications ADD COLUMN push_sent_at   INTEGER;
ALTER TABLE notifications ADD COLUMN push_attempts  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notifications ADD COLUMN push_last_error TEXT;

CREATE INDEX IF NOT EXISTS idx_notifications_pending_push
  ON notifications(created_at)
  WHERE push_sent_at IS NULL AND read_at IS NULL;

-- Tracks per-token last-used timestamp and any deactivation reason. The
-- worker can soft-delete a token by setting deactivated_reason (instead
-- of DELETE) so a re-install on the same device id can resurrect it.
ALTER TABLE push_tokens ADD COLUMN deactivated_reason TEXT;
