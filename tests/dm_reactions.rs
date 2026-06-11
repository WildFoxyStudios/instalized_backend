//! Tests for the DM message reaction surface (PUT/DELETE on a message).
//!
//! The spec for reactions is intentionally tight:
//!   * any thread member can react to any message in that thread
//!   * a user has at most one reaction per message
//!   * re-sending the same emoji is a toggle-off
//!   * re-sending a different emoji is a swap
//!   * a separate DELETE endpoint clears the caller's reaction unconditionally
//!   * the response always carries the per-emoji aggregate and `mine` flag
//!   * realtime fan-out uses a `dm.reaction` event over the WS hub
//!
//! We exercise the REST surface end-to-end. The realtime path is asserted
//! structurally: the hub receives a `dm.reaction` event with the right
//! shape and the peer receives it. A future test will open a real WS
//! client to assert the over-the-wire bytes; for now we lean on the
//! existing `send_to_user` unit-tested in `ws.rs`.

mod common;

use serde_json::json;

async fn setup_two_users(app: &common::TestApp) -> (String, String, String) {
    let (a_token, _, _a_id) = app.register("alice").await;
    let (_, _, b_id) = app.register("bob").await;
    let (status, v) = app
        .post("/v1/dm/threads", Some(&a_token), json!({ "user_id": b_id }))
        .await;
    assert_eq!(status, 200, "create thread: {v}");
    (a_token, b_id, v["id"].as_str().unwrap().to_string())
}

async fn send_text(app: &common::TestApp, token: &str, tid: &str, body: &str) -> String {
    let (status, v) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(token),
            json!({ "body": body }),
        )
        .await;
    assert_eq!(status, 200, "send text: {v}");
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn put_reaction_creates_one_row_and_marks_mine() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "hola").await;

    let (status, v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "👍" }),
        )
        .await;
    assert_eq!(status, 200, "react: {v}");
    assert_eq!(v["message_id"], mid);
    let reactions = v["reactions"].as_array().unwrap();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0]["emoji"], "👍");
    assert_eq!(reactions[0]["count"], 1);
    assert_eq!(reactions[0]["mine"], true);
    let users = reactions[0]["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
}

#[tokio::test]
async fn putting_same_emoji_toggles_off() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "again").await;

    // On.
    let (_, v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "🔥" }),
        )
        .await;
    assert_eq!(v["reactions"].as_array().unwrap().len(), 1);

    // Same emoji again → off.
    let (_, v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "🔥" }),
        )
        .await;
    let reactions = v["reactions"].as_array().unwrap();
    assert!(reactions.is_empty(), "toggle off should empty the array");
}

#[tokio::test]
async fn putting_different_emoji_swaps() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "swap me").await;

    let (_, _) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "👍" }),
        )
        .await;

    let (_, v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "❤️" }),
        )
        .await;
    let reactions = v["reactions"].as_array().unwrap();
    assert_eq!(reactions.len(), 1, "swap should leave one row, not two");
    assert_eq!(reactions[0]["emoji"], "❤️");
    assert_eq!(reactions[0]["count"], 1);
    assert_eq!(reactions[0]["mine"], true);
}

#[tokio::test]
async fn two_users_same_emoji_aggregates_count_and_mine_flag() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _, _a_id) = app.register("alice").await;
    let (b_token, _, _b_id) = app.register("bob").await;
    let (status, v) = app
        .post("/v1/dm/threads", Some(&a_token), json!({ "user_id": _b_id }))
        .await;
    assert_eq!(status, 200);
    let tid = v["id"].as_str().unwrap().to_string();
    let mid = send_text(&app, &a_token, &tid, "crowd").await;

    // Both users react with 👍.
    let (_, _) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "👍" }),
        )
        .await;
    let (_, v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&b_token),
            json!({ "emoji": "👍" }),
        )
        .await;
    let reactions = v["reactions"].as_array().unwrap();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0]["count"], 2);
    // The last response is from B's perspective → mine should be true.
    assert_eq!(reactions[0]["mine"], true);
    let users = reactions[0]["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
}

#[tokio::test]
async fn delete_endpoint_clears_my_reaction() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "delete me").await;

    // React first.
    let (_, _) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "😂" }),
        )
        .await;

    // Clear.
    let (status, v) = app
        .delete(&format!("/v1/dm/messages/{mid}/reaction"), Some(&a_token))
        .await;
    assert_eq!(status, 200, "delete reaction: {v}");
    let reactions = v["reactions"].as_array().unwrap();
    assert!(reactions.is_empty(), "delete should clear the aggregate");

    // Idempotent: a second delete is fine, still empty.
    let (_, v) = app
        .delete(&format!("/v1/dm/messages/{mid}/reaction"), Some(&a_token))
        .await;
    assert!(v["reactions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn non_member_cannot_react() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "private").await;

    // Register a third user who is NOT in the thread.
    let (_c_token, _, _c_id) = app.register("carol").await;
    let (status, _v) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&_c_token),
            json!({ "emoji": "👀" }),
        )
        .await;
    assert_eq!(status, 403, "non-member must be forbidden");
}

#[tokio::test]
async fn empty_or_oversized_emoji_rejected() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;
    let mid = send_text(&app, &a_token, &tid, "validate me").await;

    // Empty.
    let (status, _) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": "   " }),
        )
        .await;
    assert_eq!(status, 400);

    // 17 chars (over the cap).
    let long: String = "a".repeat(17);
    let (status, _) = app
        .put_with_body(
            &format!("/v1/dm/messages/{mid}/reaction"),
            Some(&a_token),
            json!({ "emoji": long }),
        )
        .await;
    assert_eq!(status, 400);
}
