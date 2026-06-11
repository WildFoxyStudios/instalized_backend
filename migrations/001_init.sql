-- Schema v1 — see ARCHITECTURE_SPEC.md §5. CIDs only; media bytes never stored.
CREATE TABLE IF NOT EXISTS users (
  id            TEXT PRIMARY KEY,
  email         TEXT UNIQUE,
  password_hash TEXT,
  google_sub    TEXT UNIQUE,
  username      TEXT UNIQUE NOT NULL,
  display_name  TEXT,
  bio           TEXT DEFAULT '',
  avatar_cid    TEXT,
  is_private    INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS refresh_tokens (
  id         TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL REFERENCES users(id),
  token_hash TEXT NOT NULL,
  device     TEXT DEFAULT '',
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_refresh_user ON refresh_tokens(user_id);
CREATE INDEX IF NOT EXISTS idx_refresh_hash ON refresh_tokens(token_hash);

CREATE TABLE IF NOT EXISTS follows (
  follower_id TEXT NOT NULL REFERENCES users(id),
  followee_id TEXT NOT NULL REFERENCES users(id),
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (follower_id, followee_id)
);
CREATE INDEX IF NOT EXISTS idx_follows_followee ON follows(followee_id);

CREATE TABLE IF NOT EXISTS posts (
  id            TEXT PRIMARY KEY,
  author_id     TEXT NOT NULL REFERENCES users(id),
  kind          TEXT NOT NULL CHECK (kind IN ('image','video','reel')),
  media_cid     TEXT NOT NULL,
  thumb_cid     TEXT,
  width         INTEGER,
  height        INTEGER,
  duration_ms   INTEGER,
  caption       TEXT DEFAULT '',
  like_count    INTEGER NOT NULL DEFAULT 0,
  comment_count INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  deleted_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_posts_author ON posts(author_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_posts_kind ON posts(kind, created_at DESC);

CREATE TABLE IF NOT EXISTS stories (
  id         TEXT PRIMARY KEY,
  author_id  TEXT NOT NULL REFERENCES users(id),
  media_cid  TEXT NOT NULL,
  thumb_cid  TEXT,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_stories_expiry ON stories(expires_at);
CREATE INDEX IF NOT EXISTS idx_stories_author ON stories(author_id);

CREATE TABLE IF NOT EXISTS likes (
  user_id    TEXT NOT NULL REFERENCES users(id),
  post_id    TEXT NOT NULL REFERENCES posts(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, post_id)
);
CREATE INDEX IF NOT EXISTS idx_likes_post ON likes(post_id);

CREATE TABLE IF NOT EXISTS comments (
  id         TEXT PRIMARY KEY,
  post_id    TEXT NOT NULL REFERENCES posts(id),
  author_id  TEXT NOT NULL REFERENCES users(id),
  body       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_comments_post ON comments(post_id, created_at DESC);

CREATE TABLE IF NOT EXISTS dm_threads (
  id         TEXT PRIMARY KEY,
  user_a     TEXT NOT NULL REFERENCES users(id),
  user_b     TEXT NOT NULL REFERENCES users(id),
  created_at INTEGER NOT NULL,
  UNIQUE (user_a, user_b)
);

CREATE TABLE IF NOT EXISTS dm_messages (
  id         TEXT PRIMARY KEY,
  thread_id  TEXT NOT NULL REFERENCES dm_threads(id),
  sender_id  TEXT NOT NULL REFERENCES users(id),
  body       TEXT DEFAULT '',
  media_cid  TEXT,
  created_at INTEGER NOT NULL,
  read_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_dm_messages_thread ON dm_messages(thread_id, created_at DESC);

CREATE TABLE IF NOT EXISTS live_streams (
  id         TEXT PRIMARY KEY,
  host_id    TEXT NOT NULL REFERENCES users(id),
  title      TEXT DEFAULT '',
  status     TEXT NOT NULL CHECK (status IN ('live','ended')),
  started_at INTEGER NOT NULL,
  ended_at   INTEGER
);

CREATE TABLE IF NOT EXISTS live_chunks (
  stream_id   TEXT NOT NULL REFERENCES live_streams(id),
  seq         INTEGER NOT NULL,
  cid         TEXT NOT NULL,
  duration_ms INTEGER NOT NULL DEFAULT 3000,
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (stream_id, seq)
);

CREATE TABLE IF NOT EXISTS pin_jobs (
  id         TEXT PRIMARY KEY,
  cid        TEXT NOT NULL UNIQUE,
  kind       TEXT NOT NULL DEFAULT 'media',
  status     TEXT NOT NULL CHECK (status IN ('pending','pinned','failed')) DEFAULT 'pending',
  attempts   INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  pinned_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_pin_jobs_status ON pin_jobs(status);

CREATE TABLE IF NOT EXISTS push_tokens (
  user_id    TEXT NOT NULL REFERENCES users(id),
  platform   TEXT NOT NULL,
  token      TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, token)
);

CREATE TABLE IF NOT EXISTS notifications (
  id         TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL REFERENCES users(id),
  kind       TEXT NOT NULL,
  actor_id   TEXT REFERENCES users(id),
  post_id    TEXT,
  created_at INTEGER NOT NULL,
  read_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_notifications_user ON notifications(user_id, created_at DESC);
