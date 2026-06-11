-- 003_social_v2.sql — moderation, archive, highlights, comment threads.
-- Applied by db::migrate() before the server starts (see db/mod.rs).

-- Block list: blocker hides blocked's content; blocked can't see blocker.
CREATE TABLE IF NOT EXISTS blocks (
    blocker_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (blocker_id, blocked_id)
);
CREATE INDEX IF NOT EXISTS idx_blocks_blocker ON blocks(blocker_id);

-- Mute: hide their posts/stories from feed without unfollowing.
CREATE TABLE IF NOT EXISTS mutes (
    muter_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    muted_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (muter_id, muted_id)
);

-- Highlights: persistent groups of stories on a profile.
CREATE TABLE IF NOT EXISTS highlights (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    cover_cid TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_highlights_user ON highlights(user_id);

-- Highlights contain ordered story references. We use a row per
-- (highlight, story) so the order is preserved and stories can be
-- added/removed without rewriting the highlights row.
CREATE TABLE IF NOT EXISTS highlight_stories (
    highlight_id TEXT NOT NULL REFERENCES highlights(id) ON DELETE CASCADE,
    story_id TEXT NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    PRIMARY KEY (highlight_id, story_id)
);
CREATE INDEX IF NOT EXISTS idx_hs_hl ON highlight_stories(highlight_id, seq);

-- Posts: add archive flag. The sweeper from 002 already hard-deletes
-- after 30 days; archive just hides from the profile grid.
ALTER TABLE posts ADD COLUMN is_archived INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_posts_author_archived
    ON posts(author_id, is_archived, created_at DESC);

-- Comments: add parent_id for reply threads, and a separate likes
-- table so likes on comments don't pollute the post-likes counter.
ALTER TABLE comments ADD COLUMN parent_id TEXT REFERENCES comments(id) ON DELETE CASCADE;
CREATE INDEX IF NOT EXISTS idx_comments_parent ON comments(parent_id, created_at);

CREATE TABLE IF NOT EXISTS comment_likes (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    comment_id TEXT NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, comment_id)
);

-- Per-user notification + privacy preferences (cached client-side too,
-- but the server is the source of truth for push delivery).
CREATE TABLE IF NOT EXISTS user_privacy (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    private_account INTEGER NOT NULL DEFAULT 0,
    show_activity_status INTEGER NOT NULL DEFAULT 1,
    allow_mentions INTEGER NOT NULL DEFAULT 1,
    allow_story_replies INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS user_notif_prefs (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    posts INTEGER NOT NULL DEFAULT 1,
    stories INTEGER NOT NULL DEFAULT 1,
    lives INTEGER NOT NULL DEFAULT 1,
    dms INTEGER NOT NULL DEFAULT 1,
    video_calls INTEGER NOT NULL DEFAULT 1,
    pause_all INTEGER NOT NULL DEFAULT 0
);

-- Post tags (for the hashtag grid). Cheap to compute: parse from caption.
CREATE TABLE IF NOT EXISTS post_tags (
    post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    PRIMARY KEY (post_id, tag)
);
CREATE INDEX IF NOT EXISTS idx_post_tags_tag ON post_tags(tag);

-- Post mentions.
CREATE TABLE IF NOT EXISTS post_mentions (
    post_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    username TEXT NOT NULL,
    PRIMARY KEY (post_id, username)
);
CREATE INDEX IF NOT EXISTS idx_post_mentions_user ON post_mentions(username);
