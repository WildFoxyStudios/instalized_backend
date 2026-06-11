-- Parity v1.1: saved posts (bookmarks) + content reports.
CREATE TABLE IF NOT EXISTS saved_posts (
  user_id    TEXT NOT NULL REFERENCES users(id),
  post_id    TEXT NOT NULL REFERENCES posts(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (user_id, post_id)
);
CREATE INDEX IF NOT EXISTS idx_saved_user ON saved_posts(user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS reports (
  reporter_id TEXT NOT NULL REFERENCES users(id),
  post_id     TEXT NOT NULL REFERENCES posts(id),
  reason      TEXT NOT NULL DEFAULT '',
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (reporter_id, post_id)
);
CREATE INDEX IF NOT EXISTS idx_reports_post ON reports(post_id);
