//! WebSocket integration tests: auth handshake, DM fan-out, live chunk relay
//! with pin-on-first-chunk (spec §7, §12).

mod common;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(app: &common::TestApp, token: &str) -> WsStream {
    let (mut ws, _) = tokio_tungstenite::connect_async(app.ws_url())
        .await
        .expect("ws connect");
    ws.send(Message::Text(
        json!({"v": 1, "type": "auth", "data": {"token": token}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let first = recv_type(&mut ws, "auth.ok").await;
    assert!(first["data"]["user_id"].is_string());
    ws
}

/// Read frames until one matches `typ` (5 s budget).
async fn recv_type(ws: &mut WsStream, typ: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for '{typ}'"))
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(raw) = frame {
            let v: Value = serde_json::from_str(raw.as_str()).unwrap();
            if v["type"] == typ {
                return v;
            }
        }
    }
}

#[tokio::test]
async fn ws_rejects_bad_auth() {
    let app = common::spawn(|_| {}).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(app.ws_url()).await.unwrap();
    ws.send(Message::Text(
        json!({"v": 1, "type": "auth", "data": {"token": "garbage"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let v = recv_type(&mut ws, "auth.err").await;
    assert_eq!(v["data"]["error"], "unauthorized");
}

#[tokio::test]
async fn ws_dm_realtime_fanout() {
    let app = common::spawn(|_| {}).await;
    let (a_tok, _, _) = app.register("ws_alice").await;
    let (b_tok, _, b_id) = app.register("ws_bob").await;

    let mut a = ws_connect(&app, &a_tok).await;
    let mut b = ws_connect(&app, &b_tok).await;

    // Alice sends over WS; Bob receives dm.new in realtime; Alice gets dm.ack.
    a.send(Message::Text(
        json!({"v": 1, "type": "dm.new", "data": {"to_user_id": b_id, "body": "hey bob"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    let received = recv_type(&mut b, "dm.new").await;
    assert_eq!(received["data"]["body"], "hey bob");
    let ack = recv_type(&mut a, "dm.ack").await;
    assert_eq!(ack["data"]["body"], "hey bob");

    // The message is also durable (REST history).
    let tid = received["data"]["thread_id"].as_str().unwrap();
    let (_, msgs) = app
        .get(&format!("/v1/dm/threads/{tid}/messages"), Some(&b_tok))
        .await;
    assert_eq!(msgs["items"][0]["body"], "hey bob");
}

#[tokio::test]
async fn ws_live_chunk_relay_and_first_chunk_pin() {
    let app = common::spawn(|_| {}).await;
    let (host_tok, _, host_id) = app.register("ws_host").await;
    let (viewer_tok, _, _) = app.register("ws_viewer").await;
    let (fan_tok, _, _) = app.register("ws_fan").await;

    // Fan follows the host and is online → gets live.start.
    let (status, _) = app.put(&format!("/v1/users/{host_id}/follow"), Some(&fan_tok)).await;
    assert_eq!(status, 200);
    let mut fan = ws_connect(&app, &fan_tok).await;

    let (status, stream) = app
        .post("/v1/live", Some(&host_tok), json!({"title": "en vivo!"}))
        .await;
    assert_eq!(status, 200);
    let sid = stream["id"].as_str().unwrap().to_string();

    let started = recv_type(&mut fan, "live.start").await;
    assert_eq!(started["data"]["stream_id"], sid.as_str());

    // Viewer joins the room.
    let mut viewer = ws_connect(&app, &viewer_tok).await;
    viewer
        .send(Message::Text(
            json!({"v": 1, "type": "live.join", "data": {"stream_id": sid}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    recv_type(&mut viewer, "live.joined").await;

    // Host announces chunk #0 over WS (3 s fMP4 segment CID).
    let mut host = ws_connect(&app, &host_tok).await;
    host.send(Message::Text(
        json!({"v": 1, "type": "live.chunk",
               "data": {"stream_id": sid, "seq": 0, "cid": "bafylivechunk0", "duration_ms": 3000}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    let chunk = recv_type(&mut viewer, "live.chunk").await;
    assert_eq!(chunk["data"]["seq"], 0);
    assert_eq!(chunk["data"]["cid"], "bafylivechunk0");

    // First chunk creates a pin job (manifesto: Pinata on first chunk).
    let (_, pin) = app.get("/v1/media/pins/bafylivechunk0", Some(&host_tok)).await;
    assert_eq!(pin["kind"], "live_first");

    // Chunk #1 does not create another live_first job.
    host.send(Message::Text(
        json!({"v": 1, "type": "live.chunk",
               "data": {"stream_id": sid, "seq": 1, "cid": "bafylivechunk1", "duration_ms": 3000}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let chunk = recv_type(&mut viewer, "live.chunk").await;
    assert_eq!(chunk["data"]["seq"], 1);
    let (status, _) = app.get("/v1/media/pins/bafylivechunk1", Some(&host_tok)).await;
    assert_eq!(status, 404);

    // Non-host cannot announce chunks.
    let (status, _) = app
        .post(
            &format!("/v1/live/{sid}/chunk"),
            Some(&viewer_tok),
            json!({"seq": 2, "cid": "bafyevil"}),
        )
        .await;
    assert_eq!(status, 403);

    // Late joiner catch-up via REST playlist.
    let (_, full) = app.get(&format!("/v1/live/{sid}"), Some(&viewer_tok)).await;
    assert_eq!(full["chunks"].as_array().unwrap().len(), 2);

    // End → viewers get live.end.
    let (status, _) = app
        .post(&format!("/v1/live/{sid}/end"), Some(&host_tok), json!({}))
        .await;
    assert_eq!(status, 200);
    let ended = recv_type(&mut viewer, "live.end").await;
    assert_eq!(ended["data"]["stream_id"], sid.as_str());
}
