//! Per-socket session: auth-first-frame, outbound pump, inbound dispatch.

use crate::api;
use crate::auth::jwt;
use crate::state::AppState;
use crate::ws::protocol::{self, Envelope};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle(socket, state))
}

async fn handle(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    // First frame must be `auth` within 5 s.
    let user_id = match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(Some(Ok(Message::Text(raw)))) => {
            let token = protocol::parse(raw.as_str())
                .filter(|e| e.typ == "auth")
                .and_then(|e| e.data.get("token").and_then(|v| v.as_str().map(String::from)));
            match token.and_then(|t| jwt::verify(&state.cfg.jwt_secret, &t).ok()) {
                Some(claims) => claims.sub,
                None => {
                    let _ = sink
                        .send(Message::Text(
                            protocol::msg("auth.err", json!({"error": "unauthorized"})).into(),
                        ))
                        .await;
                    return;
                }
            }
        }
        _ => return,
    };

    let _ = sink
        .send(Message::Text(
            protocol::msg("auth.ok", json!({"user_id": user_id})).into(),
        ))
        .await;

    let (conn_id, mut rx) = state.hub.register(&user_id).await;

    // Outbound pump: hub queue → socket.
    let pump = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });

    // Inbound loop. A Close frame (sent by clients on backgrounding) ends the session.
    while let Some(Ok(frame)) = stream.next().await {
        match frame {
            Message::Text(raw) => {
                if let Some(env) = protocol::parse(raw.as_str()) {
                    dispatch(&state, &user_id, env).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    state.hub.unregister(&user_id, conn_id).await;
    pump.abort();
}

fn str_field(data: &serde_json::Value, key: &str) -> Option<String> {
    data.get(key).and_then(|v| v.as_str()).map(String::from)
}

async fn dispatch(state: &AppState, user_id: &str, env: Envelope) {
    let reply = |typ: &str, data: serde_json::Value| protocol::msg(typ, data);
    match env.typ.as_str() {
        "ping" => {
            state
                .hub
                .send_to_user(user_id, &reply("pong", json!({})))
                .await;
        }
        "dm.new" => {
            let to_user = str_field(&env.data, "to_user_id");
            let thread_id = str_field(&env.data, "thread_id");
            let body = str_field(&env.data, "body").unwrap_or_default();
            let media_cid = str_field(&env.data, "media_cid");
            match api::dm::send_message_core(state, user_id, to_user, thread_id, body, media_cid)
                .await
            {
                Ok(message) => {
                    state
                        .hub
                        .send_to_user(user_id, &reply("dm.ack", message))
                        .await;
                }
                Err(e) => {
                    state
                        .hub
                        .send_to_user(user_id, &reply("error", json!({"error": e.to_string()})))
                        .await;
                }
            }
        }
        "dm.read" => {
            let thread_id = str_field(&env.data, "thread_id").unwrap_or_default();
            let upto_id = str_field(&env.data, "upto_id").unwrap_or_default();
            let _ = api::dm::mark_read_core(state, user_id, &thread_id, &upto_id).await;
        }
        "live.join" => {
            if let Some(stream_id) = str_field(&env.data, "stream_id") {
                state
                    .hub
                    .join_room(&format!("live:{stream_id}"), user_id)
                    .await;
                state
                    .hub
                    .send_to_user(
                        user_id,
                        &reply("live.joined", json!({"stream_id": stream_id})),
                    )
                    .await;
            }
        }
        "live.leave" => {
            if let Some(stream_id) = str_field(&env.data, "stream_id") {
                state
                    .hub
                    .leave_room(&format!("live:{stream_id}"), user_id)
                    .await;
            }
        }
        "live.chunk" => {
            let stream_id = str_field(&env.data, "stream_id").unwrap_or_default();
            let seq = env.data.get("seq").and_then(|v| v.as_i64()).unwrap_or(-1);
            let cid = str_field(&env.data, "cid").unwrap_or_default();
            let duration_ms = env
                .data
                .get("duration_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(3000);
            if let Err(e) =
                api::live::announce_chunk_core(state, user_id, &stream_id, seq, &cid, duration_ms)
                    .await
            {
                state
                    .hub
                    .send_to_user(user_id, &reply("error", json!({"error": e.to_string()})))
                    .await;
            }
        }
        other => {
            state
                .hub
                .send_to_user(
                    user_id,
                    &reply("error", json!({"error": format!("unknown type: {other}")})),
                )
                .await;
        }
    }
}
