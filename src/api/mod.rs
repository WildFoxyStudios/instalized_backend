//! REST surface (spec §6). One submodule per resource; this file owns the router
//! and the shared keyset-pagination / row-mapping helpers.

pub mod auth;
pub mod dm;
pub mod feed;
pub mod live;
pub mod media;
pub mod notifications;
pub mod posts;
pub mod ratelimit;
pub mod stories;
pub mod users;

use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

pub fn router(state: AppState) -> Router {
    let cors = match state.cfg.web_base_url.parse::<HeaderValue>() {
        Ok(origin) => CorsLayer::new().allow_origin(origin),
        Err(_) => CorsLayer::new().allow_origin(Any),
    }
    .allow_methods(Any)
    .allow_headers(Any);

    // Auth surface gets its own per-IP rate limit (spec §15).
    let auth_routes = Router::new()
        .route("/v1/auth/register", post(auth::register))
        .route("/v1/auth/login", post(auth::login))
        .route("/v1/auth/google", post(auth::google))
        .route("/v1/auth/refresh", post(auth::refresh))
        .route("/v1/auth/logout", post(auth::logout))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit_auth,
        ));

    Router::new()
        .merge(auth_routes)
        .route("/healthz", get(healthz))
        .route("/s/{post_id}", get(crate::seo::share_page::share))
        .route("/v1/ws", get(crate::ws::session::ws_handler))
        // users
        .route("/v1/users/me", get(users::get_me).patch(users::patch_me))
        .route("/v1/users/{username}", get(users::get_user))
        .route("/v1/users/{username}/posts", get(users::user_posts))
        .route(
            "/v1/users/{id}/follow",
            put(users::follow).delete(users::unfollow),
        )
        // feed
        .route("/v1/feed", get(feed::home_feed))
        .route("/v1/reels", get(feed::reels))
        // posts
        .route("/v1/posts", post(posts::create_post))
        .route(
            "/v1/posts/{id}",
            get(posts::get_post).delete(posts::delete_post),
        )
        .route(
            "/v1/posts/{id}/like",
            put(posts::like).delete(posts::unlike),
        )
        .route(
            "/v1/posts/{id}/comments",
            get(posts::comments_list).post(posts::comment_create),
        )
        // stories
        .route("/v1/stories", post(stories::create_story))
        .route("/v1/stories/feed", get(stories::stories_feed))
        // dm
        .route(
            "/v1/dm/threads",
            get(dm::threads_list).post(dm::thread_create),
        )
        .route(
            "/v1/dm/threads/{id}/messages",
            get(dm::messages_list).post(dm::message_create),
        )
        .route("/v1/dm/threads/{id}/read", post(dm::mark_read))
        // live
        .route("/v1/live", post(live::create_stream))
        .route("/v1/live/{id}", get(live::get_stream))
        .route("/v1/live/{id}/chunk", post(live::post_chunk))
        .route("/v1/live/{id}/end", post(live::end_stream))
        // media
        .route("/v1/media/announce", post(media::announce))
        .route("/v1/media/pins/{cid}", get(media::pin_status))
        .route("/v1/media/upload-token", get(media::upload_token))
        // notifications / push
        .route("/v1/notifications", get(notifications::list))
        .route("/v1/push/register", post(notifications::register_push))
        .layer(cors)
        // Metadata-only API: media bytes never come here. 64 KiB is generous.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> AppResult<Json<Value>> {
    state
        .db
        .read
        .with(|conn| Ok(conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- pagination ----------

pub const DEFAULT_LIMIT: i64 = 20;
pub const MAX_LIMIT: i64 = 50;

#[derive(Debug, Deserialize)]
pub struct Page {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl Page {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    /// Keyset cursor "created_at,id". Sentinel sorts after every ULID ('~' > 'Z').
    pub fn keyset(&self) -> (i64, String) {
        self.cursor
            .as_deref()
            .and_then(|c| {
                let (ts, id) = c.split_once(',')?;
                Some((ts.parse().ok()?, id.to_string()))
            })
            .unwrap_or((i64::MAX, "~".to_string()))
    }
}

pub fn cursor_of(created_at: i64, id: &str) -> String {
    format!("{created_at},{id}")
}

// ---------- shared row mappers ----------

/// Column list matching `post_from_row`. Requires aliases `p` (posts), `u` (author)
/// and a `LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = :viewer`.
pub const POST_COLS: &str = "p.id, p.author_id, u.username, u.display_name, u.avatar_cid, \
     p.kind, p.media_cid, p.thumb_cid, p.width, p.height, p.duration_ms, p.caption, \
     p.like_count, p.comment_count, p.created_at, (l.user_id IS NOT NULL)";

pub fn post_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "author": {
            "id": r.get::<_, String>(1)?,
            "username": r.get::<_, String>(2)?,
            "display_name": r.get::<_, Option<String>>(3)?,
            "avatar_cid": r.get::<_, Option<String>>(4)?,
        },
        "kind": r.get::<_, String>(5)?,
        "media_cid": r.get::<_, String>(6)?,
        "thumb_cid": r.get::<_, Option<String>>(7)?,
        "width": r.get::<_, Option<i64>>(8)?,
        "height": r.get::<_, Option<i64>>(9)?,
        "duration_ms": r.get::<_, Option<i64>>(10)?,
        "caption": r.get::<_, String>(11)?,
        "like_count": r.get::<_, i64>(12)?,
        "comment_count": r.get::<_, i64>(13)?,
        "created_at": r.get::<_, i64>(14)?,
        "liked_by_me": r.get::<_, bool>(15)?,
    }))
}

/// Light CID sanity check (full validation happens client-side where the CID is built).
pub fn validate_cid(cid: &str) -> AppResult<()> {
    let ok = !cid.is_empty()
        && cid.len() <= 128
        && cid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(crate::error::AppError::bad_request("invalid cid"))
    }
}
