-- Schema v2.4 — DM message reactions (emoji-style).
-- Pre-MVP, messages were text-only with no engagement. Going forward we let
-- participants in a thread react to any message in that thread with a single
-- emoji each. The first user to react with a new emoji "wins" that emoji for
-- the message — re-reacting with the same emoji is a toggle (delete).
--
-- Constraints:
--   * a user can have at most ONE reaction per message (their row is upserted)
--   * only thread members can react (enforced in the handler, not in SQLite)
--   * emoji is normalized server-side to NFC and trimmed; max 16 chars to keep
--     the column small and to thwart abuse (no ZWJ piles / long sequences)
--   * reaction_id is a public id; (message_id, user_id) is the natural key.

CREATE TABLE IF NOT EXISTS dm_reactions (
    id          TEXT PRIMARY KEY,
    message_id  TEXT NOT NULL REFERENCES dm_messages(id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL REFERENCES users(id)    ON DELETE CASCADE,
    emoji       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

-- One reaction per (message, user). The handler upserts on conflict.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_dm_reaction_user_msg
    ON dm_reactions(message_id, user_id);

-- "Give me the reaction aggregate for a message" — used by messages_list
-- to surface a small {emoji, count, mine} map alongside each message.
CREATE INDEX IF NOT EXISTS idx_dm_reactions_msg
    ON dm_reactions(message_id, emoji);
