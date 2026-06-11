//! `GET /s/:postId` — the share endpoint minted by the apps.
//! Bots: minimal HTML with og: tags (gateway URLs, ~1 read, no media bytes).
//! Humans: 302 to the SvelteKit deep link. Same content either way — no cloaking.

use crate::error::AppResult;
use crate::seo::bot_detect::is_bot;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::header::USER_AGENT;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub async fn share(
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let ua = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let deep_link = format!("{}/p/{}", state.cfg.web_base_url, post_id);

    if !is_bot(ua) {
        return Ok(Redirect::temporary(&deep_link).into_response());
    }

    let cfg = state.cfg.clone();
    let row = state.db.read.with(move |conn| {
        Ok(conn.query_row(
            "SELECT p.caption, p.kind, p.media_cid, COALESCE(p.thumb_cid, p.media_cid), u.username
             FROM posts p JOIN users u ON u.id = p.author_id
             WHERE p.id = ?1 AND p.deleted_at IS NULL",
            [&post_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?)
    })?;
    let (caption, kind, media_cid, thumb_cid, username) = row;

    let image_url = cfg.gateway_url(&thumb_cid);
    let title = format!("@{username}");
    let desc = if caption.is_empty() {
        format!("Post by @{username}")
    } else {
        caption.clone()
    };
    let video_meta = if kind != "image" {
        format!(
            r#"<meta property="og:video" content="{}"><meta property="og:video:type" content="video/mp4">"#,
            esc(&cfg.gateway_url(&media_cid))
        )
    } else {
        String::new()
    };

    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>{title}</title>
<meta name="description" content="{desc}">
<meta property="og:type" content="article">
<meta property="og:title" content="{title}">
<meta property="og:description" content="{desc}">
<meta property="og:image" content="{img}">
<meta property="og:url" content="{url}">
{video}
<meta name="twitter:card" content="summary_large_image">
<link rel="canonical" href="{url}">
</head><body><p><a href="{url}">{title}</a>: {desc}</p></body></html>"#,
        title = esc(&title),
        desc = esc(&desc),
        img = esc(&image_url),
        url = esc(&deep_link),
        video = video_meta,
    );
    Ok(Html(html).into_response())
}
