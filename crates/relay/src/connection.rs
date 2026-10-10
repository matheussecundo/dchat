//! One client WebSocket: NIP-01 REQ / EVENT / CLOSE under the relay policy.

use crate::hub::Hub;
use crate::limits::{check_event, Rejection, RelayConfig, TokenBucket};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use protocol::{NostrEvent, NostrFilter};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Pings keep proxies from closing quiet connections and reveal dead clients.
const PING_INTERVAL: Duration = Duration::from_secs(30);
const MAX_SUB_ID_LEN: usize = 64;

pub async fn handle(socket: WebSocket, hub: Arc<Hub>, cfg: Arc<RelayConfig>, ip: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let Some(id) = hub.register(&ip, tx.clone(), cfg.max_connections_per_ip) else {
        let notice = json!(["NOTICE", "rate-limited: too many connections from your address"]).to_string();
        let _ = sink.send(Message::Text(notice.into())).await;
        let _ = sink.close().await;
        return;
    };

    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await;
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Default::default())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    });

    let mut bucket = TokenBucket::new(cfg.event_burst, cfg.events_per_sec, Instant::now());
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => handle_text(&text, id, &hub, &cfg, &tx, &mut bucket),
            Message::Binary(_) => send(&tx, json!(["NOTICE", "invalid: binary messages are not supported"])),
            Message::Close(_) => break,
            // Pongs are automatic in axum; nothing else to do.
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    hub.unregister(id);
    drop(tx);
    let _ = writer.await;
}

fn handle_text(
    text: &str,
    id: u64,
    hub: &Hub,
    cfg: &RelayConfig,
    tx: &mpsc::UnboundedSender<String>,
    bucket: &mut TokenBucket,
) {
    let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(text) else {
        send(tx, json!(["NOTICE", "invalid: not a JSON array"]));
        return;
    };
    match parts.first().and_then(Value::as_str) {
        Some("EVENT") => {
            let Some(event) = parts.get(1).and_then(|v| serde_json::from_value::<NostrEvent>(v.clone()).ok()) else {
                send(tx, json!(["NOTICE", "invalid: malformed EVENT"]));
                return;
            };
            let verdict = if !bucket.try_take(Instant::now()) {
                Err(Rejection::RateLimited)
            } else {
                check_event(&event, now_secs(), cfg)
            };
            match verdict {
                Ok(()) => {
                    send(tx, json!(["OK", event.id, true, ""]));
                    hub.publish(id, &event);
                }
                Err(rejection) => send(tx, json!(["OK", event.id, false, rejection.reason()])),
            }
        }
        Some("REQ") => {
            let Some(sub_id) = parts.get(1).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= MAX_SUB_ID_LEN) else {
                send(tx, json!(["NOTICE", "invalid: bad subscription id"]));
                return;
            };
            let filters: Option<Vec<NostrFilter>> = parts[2..]
                .iter()
                .map(|v| serde_json::from_value::<NostrFilter>(v.clone()).ok())
                .collect();
            let Some(filters) = filters.filter(|f| !f.is_empty() && f.len() <= cfg.max_filters_per_req) else {
                send(tx, json!(["CLOSED", sub_id, "invalid: bad or too many filters"]));
                return;
            };
            if hub.subscribe(id, sub_id, filters, cfg.max_subscriptions) {
                // Nothing is stored, so there is never a backlog to send.
                send(tx, json!(["EOSE", sub_id]));
            } else {
                send(tx, json!(["CLOSED", sub_id, "blocked: too many subscriptions"]));
            }
        }
        Some("CLOSE") => {
            if let Some(sub_id) = parts.get(1).and_then(Value::as_str) {
                hub.unsubscribe(id, sub_id);
            }
        }
        _ => send(tx, json!(["NOTICE", "invalid: unknown message type"])),
    }
}

fn send(tx: &mpsc::UnboundedSender<String>, value: Value) {
    let _ = tx.send(value.to_string());
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
