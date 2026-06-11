//! Tests for the DM media surface (image, voice, etc.).
//! Native recording is deferred to v1.1; the test exercises the
//! backend + WS contract directly using fake CIDs and waveform strings.

mod common;

use serde_json::json;

/// Build a tiny but valid waveform string: 32 zero-valued samples.
fn waveform() -> String {
    let samples: Vec<i16> = (0..32).map(|i| (i * 4) as i16).collect();
    serde_json::to_string(&samples).unwrap()
}

async fn setup_two_users(app: &common::TestApp) -> (String, String, String) {
    let (a_token, _, _a_id) = app.register("alice").await;
    let (_, _, b_id) = app.register("bob").await;
    let (status, v) = app
        .post("/v1/dm/threads", Some(&a_token), json!({ "user_id": b_id }))
        .await;
    assert_eq!(status, 200, "create thread: {v}");
    (a_token, b_id, v["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn text_message_legacy_shape_still_works() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, v) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({ "body": "hola" }),
        )
        .await;
    assert_eq!(status, 200, "text send: {v}");
    assert_eq!(v["kind"], "text");
    assert!(v["duration_ms"].is_null());
    assert!(v["waveform"].is_null());
    assert!(v["media_cid"].is_null());

    // Roundtrip the list.
    let (_, v) = app
        .get(&format!("/v1/dm/threads/{tid}/messages"), Some(&a_token))
        .await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "text");
    assert_eq!(items[0]["body"], "hola");
}

#[tokio::test]
async fn voice_message_persists_duration_and_waveform() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, v) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "voice",
                "media_cid": "bafyvoice123",
                "duration_ms": 1234,
                "waveform": waveform(),
            }),
        )
        .await;
    assert_eq!(status, 200, "voice send: {v}");
    assert_eq!(v["kind"], "voice");
    assert_eq!(v["duration_ms"], 1234);
    assert!(v["waveform"].as_str().unwrap().starts_with('['));
    assert_eq!(v["media_cid"], "bafyvoice123");

    // The thread list shows it.
    let (_, v) = app
        .get(&format!("/v1/dm/threads/{tid}/messages"), Some(&a_token))
        .await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "voice");
    assert_eq!(items[0]["duration_ms"], 1234);
}

#[tokio::test]
async fn voice_message_rejected_without_duration() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "voice",
                "media_cid": "bafyvoice",
                "waveform": waveform(),
                // duration_ms missing
            }),
        )
        .await;
    assert_eq!(status, 400, "missing duration must be 400");
}

#[tokio::test]
async fn voice_message_rejected_without_waveform() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "voice",
                "media_cid": "bafyvoice",
                "duration_ms": 1000,
            }),
        )
        .await;
    assert_eq!(status, 400, "missing waveform must be 400");
}

#[tokio::test]
async fn voice_message_rejects_outrageous_duration() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "voice",
                "media_cid": "bafyvoice",
                "duration_ms": 86_400_000, // 24h
                "waveform": waveform(),
            }),
        )
        .await;
    assert_eq!(status, 400, "duration > 10min must be 400");
}

#[tokio::test]
async fn image_message_persists_thumb_and_dimensions() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, v) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "image",
                "media_cid": "bafyimg",
                "thumb_cid": "bafyimg-thumb",
                "width": 1080,
                "height": 1350,
            }),
        )
        .await;
    assert_eq!(status, 200, "image send: {v}");
    assert_eq!(v["kind"], "image");
    assert_eq!(v["thumb_cid"], "bafyimg-thumb");
    assert_eq!(v["width"], 1080);
    assert_eq!(v["height"], 1350);
}

#[tokio::test]
async fn image_message_rejects_invalid_thumb_cid() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "image",
                "media_cid": "bafyimg",
                "thumb_cid": "has spaces in cid",
                "width": 1080,
                "height": 1350,
            }),
        )
        .await;
    assert_eq!(status, 400, "invalid thumb_cid must be 400");
}

#[tokio::test]
async fn unknown_kind_is_rejected() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "smell-o-vision",
                "body": "what is that",
            }),
        )
        .await;
    assert_eq!(status, 400, "unknown kind must be 400");
}

#[tokio::test]
async fn voice_message_invalid_cid_is_rejected() {
    let app = common::spawn(|_| {}).await;
    let (a_token, _b_id, tid) = setup_two_users(&app).await;

    let (status, _) = app
        .post(
            &format!("/v1/dm/threads/{tid}/messages"),
            Some(&a_token),
            json!({
                "kind": "voice",
                "media_cid": "spaces in cid",
                "duration_ms": 500,
                "waveform": waveform(),
            }),
        )
        .await;
    assert_eq!(status, 400, "invalid media_cid must be 400");
}
