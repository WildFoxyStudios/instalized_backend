-- Schema v2.3 — DM media metadata.
-- Pre-MVP, every DM is a text row. Going forward we support image, voice
-- (and later video/reel-share). The columns are nullable so the migration
-- is forward-only: existing text messages get kind='text', no duration,
-- no waveform, no thumbnail.

ALTER TABLE dm_messages ADD COLUMN kind        TEXT NOT NULL DEFAULT 'text';
ALTER TABLE dm_messages ADD COLUMN duration_ms INTEGER;
ALTER TABLE dm_messages ADD COLUMN waveform    TEXT;        -- JSON array of i16 samples, e.g. "[0,12,34,..]"
ALTER TABLE dm_messages ADD COLUMN thumb_cid   TEXT;
ALTER TABLE dm_messages ADD COLUMN width       INTEGER;
ALTER TABLE dm_messages ADD COLUMN height      INTEGER;

-- Backfill: any pre-migration row was text by definition.
UPDATE dm_messages SET kind = 'text' WHERE kind IS NULL OR kind = '';

-- Useful index for "give me the media sent in this thread" (image/voice/etc).
CREATE INDEX IF NOT EXISTS idx_dm_messages_kind ON dm_messages(thread_id, kind, created_at DESC)
  WHERE kind <> 'text';
