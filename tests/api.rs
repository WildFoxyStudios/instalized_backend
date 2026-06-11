//! REST integration tests: auth, feed pagination, batched likes, stories sweeper,
//! SEO interceptor, pinning worker (against a mock IETF pinning service), privacy.

mod common;

use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn auth_register_login_refresh_rotation() {
    let app = common::spawn(|_| {}).await;
    let (access, refresh, _uid) = app.register("alice").await;

    // /me works with the access token.
    let (status, me) = app.get("/v1/users/me", Some(&access)).await;
    assert_eq!(status, 200);
    assert_eq!(me["username"], "alice");

    // Duplicate username → 409.
    let (status, _) = app
        .post(
            "/v1/auth/register",
            None,
            json!({"email": "other@example.com", "username": "alice", "password": "password123"}),
        )
        .await;
    assert_eq!(status, 409);

    // Wrong password → 401.
    let (status, _) = app
        .post(
            "/v1/auth/login",
            None,
            json!({"email": "alice@example.com", "password": "wrong-password"}),
        )
        .await;
    assert_eq!(status, 401);

    // Correct login → 200.
    let (status, _) = app
        .post(
            "/v1/auth/login",
            None,
            json!({"email": "alice@example.com", "password": "password123"}),
        )
        .await;
    assert_eq!(status, 200);

    // Refresh rotates: new pair issued, old refresh single-use.
    let (status, pair) = app
        .post("/v1/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(status, 200);
    let new_refresh = pair["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(new_refresh, refresh);

    let (status, _) = app
        .post("/v1/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert_eq!(status, 401, "rotated token must be rejected");

    let (status, _) = app
        .post("/v1/auth/refresh", None, json!({"refresh_token": new_refresh}))
        .await;
    assert_eq!(status, 200);

    // No token → 401.
    let (status, _) = app.get("/v1/users/me", None).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn feed_keyset_pagination() {
    let app = common::spawn(|_| {}).await;
    let (a_tok, _, _) = app.register("reader").await;
    let (b_tok, _, b_id) = app.register("author").await;

    // 25 posts by author.
    let mut created = Vec::new();
    for i in 0..25 {
        created.push(app.create_post(&b_tok, "image", &format!("bafyaaa{i:03}")).await);
    }

    // Feed before following: empty.
    let (_, v) = app.get("/v1/feed", Some(&a_tok)).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 0);

    let (status, _) = app.put(&format!("/v1/users/{b_id}/follow"), Some(&a_tok)).await;
    assert_eq!(status, 200);

    // Page 1: 20 items + cursor.
    let (_, p1) = app.get("/v1/feed", Some(&a_tok)).await;
    let items1 = p1["items"].as_array().unwrap();
    assert_eq!(items1.len(), 20);
    let cursor = p1["next_cursor"].as_str().expect("cursor present").to_string();

    // Page 2: remaining 5, no cursor.
    let (_, p2) = app
        .get(&format!("/v1/feed?cursor={cursor}"), Some(&a_tok))
        .await;
    let items2 = p2["items"].as_array().unwrap();
    assert_eq!(items2.len(), 5);
    assert!(p2["next_cursor"].is_null());

    // No duplicates, full coverage.
    let mut seen: Vec<String> = items1
        .iter()
        .chain(items2.iter())
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 25);
    for id in created {
        assert!(seen.contains(&id));
    }
}

#[tokio::test]
async fn likes_and_comments_batched_counters() {
    let app = common::spawn(|_| {}).await;
    let (a_tok, _, _) = app.register("liker").await;
    let (b_tok, _, _) = app.register("poster").await;
    let post_id = app.create_post(&b_tok, "image", "bafylikeme").await;

    let (status, v) = app.put(&format!("/v1/posts/{post_id}/like"), Some(&a_tok)).await;
    assert_eq!(status, 200);
    assert_eq!(v["liked"], true);

    // Idempotent.
    let (status, _) = app.put(&format!("/v1/posts/{post_id}/like"), Some(&a_tok)).await;
    assert_eq!(status, 200);

    let (_, c) = app
        .post(
            &format!("/v1/posts/{post_id}/comments"),
            Some(&a_tok),
            json!({"body": "nice shot"}),
        )
        .await;
    assert_eq!(c["body"], "nice shot");

    // Batched counters land after the flush interval (50 ms in tests).
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, post) = app.get(&format!("/v1/posts/{post_id}"), Some(&a_tok)).await;
    assert_eq!(post["like_count"], 1, "exactly one like despite double PUT");
    assert_eq!(post["comment_count"], 1);
    assert_eq!(post["liked_by_me"], true);

    // Poster gets a notification.
    let (_, notifs) = app.get("/v1/notifications", Some(&b_tok)).await;
    let kinds: Vec<&str> = notifs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"like") && kinds.contains(&"comment"));

    // Unlike drops the counter back.
    app.delete(&format!("/v1/posts/{post_id}/like"), Some(&a_tok)).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, post) = app.get(&format!("/v1/posts/{post_id}"), Some(&a_tok)).await;
    assert_eq!(post["like_count"], 0);
    assert_eq!(post["liked_by_me"], false);
}

#[tokio::test]
async fn stories_expire_via_sweeper() {
    let app = common::spawn(|c| c.story_sweep_secs = 1).await;
    let (tok, _, _) = app.register("storyteller").await;

    let (status, story) = app
        .post(
            "/v1/stories",
            Some(&tok),
            json!({"media_cid": "bafystory1", "ttl_secs": 1}),
        )
        .await;
    assert_eq!(status, 200);
    assert!(story["expires_at"].as_i64().unwrap() > 0);

    let (_, feed) = app.get("/v1/stories/feed", Some(&tok)).await;
    assert_eq!(feed["items"].as_array().unwrap().len(), 1);

    // TTL 1 s + sweeper every 1 s → hard-deleted from the DB (manifesto).
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
    let (_, feed) = app.get("/v1/stories/feed", Some(&tok)).await;
    assert_eq!(feed["items"].as_array().unwrap().len(), 0);

    let rows: i64 = app
        .state
        .db
        .read
        .with(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(rows, 0, "expired story must be hard-deleted");
}

#[tokio::test]
async fn seo_interceptor_bots_vs_humans() {
    let app = common::spawn(|_| {}).await;
    let (tok, _, _) = app.register("seo_user").await;
    let post_id = app.create_post(&tok, "image", "bafyseoimage").await;

    // Crawler → og: HTML with gateway-resolved image.
    let resp = app
        .http
        .get(format!("{}/s/{post_id}", app.base))
        .header("user-agent", "Twitterbot/1.0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.contains("og:image"), "missing og:image: {html}");
    assert!(html.contains("https://ipfs.io/ipfs/bafyseoimage"));
    assert!(html.contains("@seo_user"));

    // Human → redirect to the web app deep link.
    let resp = app
        .http
        .get(format!("{}/s/{post_id}", app.base))
        .header("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/125")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 307);
    let loc = resp.headers()["location"].to_str().unwrap();
    assert_eq!(loc, format!("http://web.test/p/{post_id}"));
}

async fn spawn_pin_mock() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = hits.clone();
    let mock = Router::new().route(
        "/pins",
        post(move || {
            hits2.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"requestid": "mock-1", "status": "pinned"})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    (format!("http://{addr}"), hits)
}

#[tokio::test]
async fn pinning_worker_pins_announced_cids() {
    let (mock_url, hits) = spawn_pin_mock().await;
    let app = common::spawn(|c| {
        c.pinning_api_url = Some(mock_url);
        c.pinning_token = "test-token".into();
        c.pin_worker_secs = 1;
    })
    .await;
    let (tok, _, _) = app.register("pinner").await;

    // Direct announcement.
    let (status, v) = app
        .post("/v1/media/announce", Some(&tok), json!({"cid": "bafyannounced"}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(v["status"], "pending");

    // Post creation enqueues its media CID too (spec §10).
    app.create_post(&tok, "image", "bafypostpin").await;

    // Worker tick (1 s) + margin.
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;

    let (_, v) = app.get("/v1/media/pins/bafyannounced", Some(&tok)).await;
    assert_eq!(v["status"], "pinned", "announced cid should be pinned: {v}");
    let (_, v) = app.get("/v1/media/pins/bafypostpin", Some(&tok)).await;
    assert_eq!(v["status"], "pinned");
    assert!(hits.load(Ordering::SeqCst) >= 2, "mock service must be hit");
}

#[tokio::test]
async fn private_account_locks_grid() {
    let app = common::spawn(|_| {}).await;
    let (a_tok, _, _) = app.register("stranger").await;
    let (b_tok, _, _) = app.register("privatey").await;
    app.create_post(&b_tok, "image", "bafyprivate").await;

    let (status, _) = app
        .request(
            reqwest::Method::PATCH,
            "/v1/users/me",
            Some(&b_tok),
            Some(json!({"is_private": true})),
        )
        .await;
    assert_eq!(status, 200);

    let (_, grid) = app.get("/v1/users/privatey/posts", Some(&a_tok)).await;
    assert_eq!(grid["locked"], true);
    assert_eq!(grid["items"].as_array().unwrap().len(), 0);

    // Owner still sees their own grid.
    let (_, grid) = app.get("/v1/users/privatey/posts", Some(&b_tok)).await;
    assert_eq!(grid["locked"], false);
    assert_eq!(grid["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dm_rest_flow() {
    let app = common::spawn(|_| {}).await;
    let (a_tok, _, _a_id) = app.register("dm_alice").await;
    let (b_tok, _, b_id) = app.register("dm_bob").await;

    let (status, thread) = app
        .post("/v1/dm/threads", Some(&a_tok), json!({"user_id": b_id}))
        .await;
    assert_eq!(status, 200);
    let tid = thread["id"].as_str().unwrap().to_string();

    let (status, msg) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_tok),
            json!({"body": "hola!"}),
        )
        .await;
    assert_eq!(status, 200);
    let msg_id = msg["id"].as_str().unwrap().to_string();

    // Bob sees the thread with 1 unread.
    let (_, threads) = app.get("/v1/dm/threads", Some(&b_tok)).await;
    let t = &threads["items"][0];
    assert_eq!(t["unread"], 1);
    assert_eq!(t["last_message"], "hola!");
    assert_eq!(t["peer"]["username"], "dm_alice");

    // Bob reads.
    let (_, read) = app
        .post(
            &format!("/v1/dm/threads/{tid}/read"),
            Some(&b_tok),
            json!({"upto_id": msg_id}),
        )
        .await;
    assert_eq!(read["read"], 1);

    let (_, threads) = app.get("/v1/dm/threads", Some(&b_tok)).await;
    assert_eq!(threads["items"][0]["unread"], 0);

    // Outsider can't read the thread.
    let (c_tok, _, _) = app.register("dm_eve").await;
    let (status, _) = app
        .get(&format!("/v1/dm/threads/{tid}/messages"), Some(&c_tok))
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn public_reads_without_token() {
    let app = common::spawn(|_| {}).await;
    let (tok, _, _) = app.register("public_author").await;
    let post_id = app.create_post(&tok, "image", "bafypublic").await;

    let (status, post) = app.get(&format!("/v1/posts/{post_id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(post["liked_by_me"], false);

    let (status, profile) = app.get("/v1/users/public_author", None).await;
    assert_eq!(status, 200);
    assert_eq!(profile["is_following"], false);
    let (status, grid) = app.get("/v1/users/public_author/posts", None).await;
    assert_eq!(status, 200);
    assert_eq!(grid["items"].as_array().unwrap().len(), 1);

    let (status, _) = app.get(&format!("/v1/posts/{post_id}/comments"), None).await;
    assert_eq!(status, 200);
    let (status, _) = app.get("/v1/feed", None).await;
    assert_eq!(status, 401);

    let (status, _) = app.get("/v1/media/upload-token", Some(&tok)).await;
    assert_eq!(status, 503);
    let app2 = common::spawn(|c| c.pinata_upload_jwt = Some("scoped-jwt".into())).await;
    let (tok2, _, _) = app2.register("uploader").await;
    let (status, v) = app2.get("/v1/media/upload-token", Some(&tok2)).await;
    assert_eq!(status, 200);
    assert_eq!(v["jwt"], "scoped-jwt");
}
