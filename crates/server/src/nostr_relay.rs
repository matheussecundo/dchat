use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use protocol::{NostrEvent, NostrFilter};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

#[derive(Clone, Debug)]
pub struct Subscription {
    pub sub_id: String,
    pub filters: Vec<NostrFilter>,
    pub tx: mpsc::UnboundedSender<String>,
}

/// In-memory Nostr mock relay state for local dev and automated E2E tests.
/// Ephemeral events (20000 <= kind < 30000) are routed in RAM and never stored to disk.
#[derive(Default, Clone)]
pub struct NostrRelayState {
    pub clients: Arc<RwLock<HashMap<String, HashMap<String, Subscription>>>>,
}

pub async fn handle_nostr_websocket(socket: WebSocket, state: NostrRelayState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let client_id = uuid::Uuid::new_v4().to_string();

    while let Some(result) = ws_receiver.next().await {
        let text = match result {
            Ok(Message::Text(t)) => t,
            Ok(Message::Ping(_)) => {
                let _ = tx.send(r#"["NOTICE","pong"]"#.into());
                continue;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let val: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let arr = match val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        let cmd = match arr[0].as_str() {
            Some(c) => c,
            None => continue,
        };

        match cmd {
            "REQ" => {
                if arr.len() >= 2 {
                    let sub_id = arr[1].as_str().unwrap_or("").to_string();
                    let mut filters = Vec::new();
                    for item in &arr[2..] {
                        if let Ok(f) = serde_json::from_value::<NostrFilter>(item.clone()) {
                            filters.push(f);
                        }
                    }

                    {
                        let mut clients = state.clients.write().await;
                        let sub_map = clients.entry(client_id.clone()).or_default();
                        sub_map.insert(
                            sub_id.clone(),
                            Subscription {
                                sub_id: sub_id.clone(),
                                filters,
                                tx: tx.clone(),
                            },
                        );
                    }

                    // Send EOSE
                    let eose = format!(r#"["EOSE","{}"]"#, sub_id);
                    let _ = tx.send(eose);
                }
            }
            "EVENT" => {
                if arr.len() >= 2 {
                    if let Ok(event) = serde_json::from_value::<NostrEvent>(arr[1].clone()) {
                        // Send OK response to publisher
                        let ok_msg = format!(r#"["OK","{}",true,""]"#, event.id);
                        let _ = tx.send(ok_msg);

                        // Broadcast to all matching subscribers (excluding the sender client)
                        let clients = state.clients.read().await;
                        for (other_client_id, sub_map) in clients.iter() {
                            if other_client_id == &client_id {
                                continue;
                            }
                            for (sub_id, sub) in sub_map {
                                let mut matches = false;
                                for f in &sub.filters {
                                    if f.matches(&event) {
                                        matches = true;
                                        break;
                                    }
                                }
                                if matches {
                                    if let Ok(event_val) = serde_json::to_value(&event) {
                                        let broadcast = serde_json::json!(["EVENT", sub_id, event_val]);
                                        if let Ok(json_str) = serde_json::to_string(&broadcast) {
                                            let _ = sub.tx.send(json_str);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "CLOSE" => {
                if arr.len() >= 2 {
                    let sub_id = arr[1].as_str().unwrap_or("");
                    let mut clients = state.clients.write().await;
                    if let Some(sub_map) = clients.get_mut(&client_id) {
                        sub_map.remove(sub_id);
                    }
                }
            }
            _ => {}
        }
    }

    // Client disconnected, cleanup subscriptions
    {
        let mut clients = state.clients.write().await;
        clients.remove(&client_id);
    }

    send_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::KIND_EPHEMERAL_SIGNAL;

    #[tokio::test]
    async fn test_in_memory_mock_nostr_relay_lifecycle() {
        let state = NostrRelayState::default();
        let (_tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, mut rx2) = mpsc::unbounded_channel::<String>();

        let client1 = "client-1";
        let client2 = "client-2";

        // Client 2 subscribes to topic "topic-123"
        {
            let mut clients = state.clients.write().await;
            let sub_map = clients.entry(client2.to_string()).or_default();
            sub_map.insert(
                "sub-2".into(),
                Subscription {
                    sub_id: "sub-2".into(),
                    filters: vec![NostrFilter {
                        kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
                        d_tags: Some(vec!["topic-123".into()]),
                        ..Default::default()
                    }],
                    tx: tx2.clone(),
                },
            );
        }

        // Client 1 sends an event on topic-123
        let event = NostrEvent {
            id: "evt-999".into(),
            pubkey: "author-1".into(),
            created_at: 12345,
            kind: KIND_EPHEMERAL_SIGNAL,
            tags: vec![vec!["d".into(), "topic-123".into()]],
            content: "encrypted_signal".into(),
            sig: "sig-1".into(),
        };

        // Simulate broadcast
        {
            let clients = state.clients.read().await;
            for (other_client, sub_map) in clients.iter() {
                if other_client == &client1 {
                    continue;
                }
                for (sub_id, sub) in sub_map {
                    if sub.filters.iter().any(|f| f.matches(&event)) {
                        let broadcast = serde_json::json!(["EVENT", sub_id, event]);
                        let _ = sub.tx.send(serde_json::to_string(&broadcast).unwrap());
                    }
                }
            }
        }

        // Client 2 should receive the event
        let msg = rx2.recv().await.expect("recv event");
        assert!(msg.contains("EVENT"));
        assert!(msg.contains("sub-2"));
        assert!(msg.contains("evt-999"));
        assert!(msg.contains("encrypted_signal"));

        // Cleanup
        {
            let mut clients = state.clients.write().await;
            clients.remove(client2);
            assert!(clients.is_empty());
        }
    }
}
