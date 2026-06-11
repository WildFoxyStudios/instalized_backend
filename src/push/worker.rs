//! Push worker: drains `notifications` whose `push_sent_at IS NULL AND
//! read_at IS NULL`, fans out via FCM to all of the recipient's
//! `push_tokens` that aren't soft-deleted, and writes the result back.

use crate::db::now;
use crate::error::AppResult;
use crate::push::fcm::FcmClient;
use crate::state::AppState;

/// Hard cap on retries before we give up on a notification. The user's
/// device may be offline, so we keep trying — but a permanently broken
/// payload would otherwise pin a row in pending forever.
const MAX_ATTEMPTS: i64 = 8;
const BATCH: i64 = 50;

pub fn spawn(state: AppState) {
    let Some(fcm) = state.fcm.as_ref().as_ref() else {
        tracing::info!("push: FCM not configured (FCM_SERVICE_ACCOUNT_PATH / FCM_PROJECT_ID missing) — worker disabled");
        return;
    };
    let fcm = fcm.clone();
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(state.cfg.push_worker_secs.max(1));
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = run_once(&state, &fcm).await {
                tracing::warn!("push worker pass failed: {e}");
            }
        }
    });
}

pub async fn run_once(state: &AppState, fcm: &FcmClient) -> AppResult<()> {
    // 1. Pull a batch of pending notifications. Skip rows the user has
    //    already read inside the app — sending a push for an
    //    already-consumed notification is noise.
    let rows: Vec<(
        String,    // notification id
        String,    // recipient user_id
        String,    // kind (like, comment, follow, dm, mention, live)
        Option<String>, // actor_id (the "from" of the headline)
        Option<String>, // post_id
        i64,       // attempts
    )> = state.db.read.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, user_id, kind, actor_id, post_id, push_attempts
             FROM notifications
             WHERE push_sent_at IS NULL
               AND read_at IS NULL
               AND push_attempts < ?1
             ORDER BY created_at
             LIMIT ?2",
        )?;
        let v = stmt
            .query_map(rusqlite::params![MAX_ATTEMPTS, BATCH], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    })?;

    for (notif_id, user_id, kind, actor_id, post_id, attempts) in rows {
        // Respect per-user notification preferences. We block every kind
        // explicitly so a typo in the kind name never silently bypasses
        // the user's choice.
        let mut want_push = true;
        if let Some(prefs) = read_user_prefs(state, &user_id) {
            want_push = !prefs.pause_all && prefs.allows_kind(&kind);
        }
        if !want_push {
            mark_skip(state, &notif_id, "user preference").await?;
            continue;
        }
        // 2. List active tokens.
        let tokens = list_active_tokens(state, &user_id);
        if tokens.is_empty() {
            // Nothing to send to — mark sent so we don't re-poll.
            mark_done(state, &notif_id).await?;
            continue;
        }
        // 3. Build a human-readable headline.
        let (title, body) = render_headline(&kind, actor_id.as_deref(), post_id.as_deref());
        let mut data = vec![
            ("notification_id".into(), notif_id.clone()),
            ("kind".into(), kind.clone()),
        ];
        if let Some(ref p) = post_id {
            data.push(("post_id".into(), p.clone()));
        }
        // 4. Fan out. Per-token failures are independent.
        let mut dead: Vec<String> = Vec::new();
        let mut last_err: Option<String> = None;
        for token in &tokens {
            match fcm.send_to_token(token, &title, &body, &data).await {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    // FCM's status code is the second token in our formatted
                    // error string ("fcm http 404 status=NOT_FOUND body=...").
                    let code = msg
                        .split_whitespace()
                        .find_map(|tok| tok.strip_prefix("status="))
                        .unwrap_or("")
                        .to_string();
                    if FcmClient::is_dead_token(&code) {
                        dead.push(token.clone());
                    }
                    last_err = Some(msg);
                }
            }
        }
        // 5. Soft-delete dead tokens (preserves the row for re-install).
        for t in &dead {
            deactivate_token(state, t, &kind).await;
        }
        // 6. Update the notification row.
        if dead.len() == tokens.len() && !tokens.is_empty() {
            // Every token is dead; we won't be able to deliver this one
            // ever, so don't keep trying.
            mark_done_with_error(state, &notif_id, "all tokens dead").await?;
        } else {
            let attempts_new = attempts + 1;
            if last_err.is_none() {
                mark_done(state, &notif_id).await?;
            } else if attempts_new >= MAX_ATTEMPTS {
                mark_done_with_error(
                    state,
                    &notif_id,
                    &last_err.unwrap_or_else(|| "unknown".into()),
                )
                .await?;
            } else {
                mark_retry(state, &notif_id, attempts_new, &last_err.unwrap_or_default())
                    .await?;
            }
        }
    }
    Ok(())
}

// ---------- per-user preference lookup ----------

struct UserPrefs {
    pause_all: bool,
    posts: bool,
    stories: bool,
    lives: bool,
    dms: bool,
    calls: bool,
}

impl UserPrefs {
    fn allows_kind(&self, kind: &str) -> bool {
        match kind {
            "like" | "comment" | "follow" | "mention" | "tag" | "post" => self.posts,
            "story" | "story_reply" => self.stories,
            "live" | "live_start" => self.lives,
            "dm" | "dm_request" => self.dms,
            "call" | "call_missed" => self.calls,
            // Unknown kinds are denied by default — better to under-notify
            // than to spam users with kinds the UI doesn't even render.
            _ => false,
        }
    }
}

fn read_user_prefs(state: &AppState, user_id: &str) -> Option<UserPrefs> {
    state
        .db
        .read
        .with(|conn| {
            let r = conn
                .query_row(
                    "SELECT pause_all, posts, stories, lives, dms, calls
                     FROM user_notif_prefs WHERE user_id = ?1",
                    [user_id],
                    |r| {
                        Ok(UserPrefs {
                            pause_all: r.get::<_, i64>(0)? != 0,
                            posts: r.get::<_, i64>(1)? != 0,
                            stories: r.get::<_, i64>(2)? != 0,
                            lives: r.get::<_, i64>(3)? != 0,
                            dms: r.get::<_, i64>(4)? != 0,
                            calls: r.get::<_, i64>(5)? != 0,
                        })
                    },
                )
                .ok();
            Ok(r)
        })
        .ok()
        .flatten()
}

// ---------- row helpers ----------

fn list_active_tokens(state: &AppState, user_id: &str) -> Vec<String> {
    state
        .db
        .read
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT token FROM push_tokens
                 WHERE user_id = ?1 AND deactivated_reason IS NULL",
            )?;
            let v = stmt
                .query_map([user_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap_or_default()
}

async fn deactivate_token(state: &AppState, token: &str, reason: &str) {
    let token = token.to_string();
    let reason = reason.to_string();
    let _ = state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE push_tokens SET deactivated_reason = ?1
                 WHERE token = ?2 AND deactivated_reason IS NULL",
                rusqlite::params![reason, token],
            )?;
            Ok(())
        })
        .await;
}

async fn mark_done(state: &AppState, notif_id: &str) -> AppResult<()> {
    let id = notif_id.to_string();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE notifications
                 SET push_sent_at = ?1, push_attempts = push_attempts + 1,
                     push_last_error = NULL
                 WHERE id = ?2",
                rusqlite::params![now(), id],
            )?;
            Ok(())
        })
        .await
}

async fn mark_done_with_error(state: &AppState, notif_id: &str, err: &str) -> AppResult<()> {
    let id = notif_id.to_string();
    let err = err.to_string();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE notifications
                 SET push_sent_at = ?1, push_attempts = push_attempts + 1,
                     push_last_error = ?2
                 WHERE id = ?3",
                rusqlite::params![now(), err, id],
            )?;
            Ok(())
        })
        .await
}

async fn mark_retry(state: &AppState, notif_id: &str, attempts: i64, err: &str) -> AppResult<()> {
    let id = notif_id.to_string();
    let err = err.to_string();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE notifications
                 SET push_attempts = ?1, push_last_error = ?2
                 WHERE id = ?3",
                rusqlite::params![attempts, err, id],
            )?;
            Ok(())
        })
        .await
}

async fn mark_skip(state: &AppState, notif_id: &str, reason: &str) -> AppResult<()> {
    let id = notif_id.to_string();
    let reason = reason.to_string();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE notifications
                 SET push_sent_at = ?1, push_last_error = ?2
                 WHERE id = ?3",
                rusqlite::params![now(), reason, id],
            )?;
            Ok(())
        })
        .await
}

// ---------- headline render ----------

/// Render a one-liner a phone can show. The user will tap it; we pack
/// enough context to drive the deep link client-side. The web (SvelteKit)
/// reads the same fields to show a notification in the bell.
fn render_headline(
    kind: &str,
    actor_id: Option<&str>,
    post_id: Option<&str>,
) -> (String, String) {
    let title = "hybridsocial".to_string();
    let body = match (kind, actor_id, post_id) {
        ("like", Some(_), Some(_)) => "Someone liked your post".to_string(),
        ("comment", Some(_), Some(_)) => "New comment on your post".to_string(),
        ("follow", Some(_), None) => "You have a new follower".to_string(),
        ("mention", Some(_), Some(_)) => "You were mentioned in a post".to_string(),
        ("mention", Some(_), None) => "You were mentioned in a comment".to_string(),
        ("story_reply", Some(_), None) => "Someone replied to your story".to_string(),
        ("live", Some(_), None) => "Someone you follow is live".to_string(),
        ("dm", Some(_), None) => "You have a new direct message".to_string(),
        (other, _, _) => format!("New notification ({other})"),
    };
    (title, body)
}
