use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMessage, EncryptedPayload, ServerMessage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

#[derive(Default)]
pub struct Room {
    pub initiator: Option<(String, mpsc::UnboundedSender<ServerMessage>)>,
    pub responder: Option<(String, mpsc::UnboundedSender<ServerMessage>)>,
}

#[derive(Clone, Default)]
pub struct AppState {
    pub rooms: Arc<RwLock<HashMap<String, Room>>>,
}

pub async fn handle_websocket(socket: WebSocket, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Forward messages from channel to websocket client
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(err) => {
                    error!("Failed to serialize server message: {err}");
                    continue;
                }
            };
            if ws_sender.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    let client_id = uuid::Uuid::new_v4().to_string();
    let mut current_room_id: Option<String> = None;

    while let Some(result) = ws_receiver.next().await {
        let msg = match result {
            Ok(Message::Text(text)) => text,
            Ok(Message::Ping(_)) => {
                let _ = tx.send(ServerMessage::Pong);
                continue;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(err) => {
                warn!("Invalid message received: {err}");
                let _ = tx.send(ServerMessage::Error {
                    message: "Invalid JSON format".into(),
                });
                continue;
            }
        };

        match client_msg {
            ClientMessage::Join { room_id } => {
                if let Some(ref old_room) = current_room_id {
                    leave_room(&state, old_room, &client_id).await;
                }
                current_room_id = Some(room_id.clone());
                join_room(&state, &room_id, &client_id, tx.clone()).await;
            }
            ClientMessage::Signal { room_id, payload } => {
                forward_signal(&state, &room_id, &client_id, payload).await;
            }
            ClientMessage::Leave { room_id } => {
                leave_room(&state, &room_id, &client_id).await;
                current_room_id = None;
            }
            ClientMessage::Ping => {
                let _ = tx.send(ServerMessage::Pong);
            }
        }
    }

    // Client disconnected, cleanup
    if let Some(room_id) = current_room_id {
        leave_room(&state, &room_id, &client_id).await;
    }

    send_task.abort();
}

async fn join_room(
    state: &AppState,
    room_id: &str,
    client_id: &str,
    tx: mpsc::UnboundedSender<ServerMessage>,
) {
    let mut rooms = state.rooms.write().await;
    let room = rooms.entry(room_id.to_string()).or_default();

    if room.initiator.is_none() {
        room.initiator = Some((client_id.to_string(), tx.clone()));
        let _ = tx.send(ServerMessage::Joined {
            room_id: room_id.to_string(),
            peer_count: 1,
            is_initiator: true,
        });
        info!("Client joined as initiator in room (1 peer)");
    } else if room.responder.is_none() {
        room.responder = Some((client_id.to_string(), tx.clone()));
        let _ = tx.send(ServerMessage::Joined {
            room_id: room_id.to_string(),
            peer_count: 2,
            is_initiator: false,
        });
        // Notify the initiator that peer joined
        if let Some((_, ref init_tx)) = room.initiator {
            let _ = init_tx.send(ServerMessage::PeerJoined);
        }
        info!("Client joined as responder in room (2 peers). PeerJoined sent to initiator.");
    } else {
        warn!("Room is full. Rejecting client.");
        let _ = tx.send(ServerMessage::Error {
            message: "Room is full (maximum 2 participants allowed for ephemeral P2P)".into(),
        });
    }
}

async fn forward_signal(
    state: &AppState,
    room_id: &str,
    client_id: &str,
    payload: EncryptedPayload,
) {
    let rooms = state.rooms.read().await;
    if let Some(room) = rooms.get(room_id) {
        let target_tx = if let Some((ref id, ref tx)) = room.initiator {
            if id == client_id {
                room.responder.as_ref().map(|(_, tx)| tx)
            } else {
                Some(tx)
            }
        } else if let Some((ref id, ref tx)) = room.responder {
            if id == client_id {
                room.initiator.as_ref().map(|(_, tx)| tx)
            } else {
                Some(tx)
            }
        } else {
            None
        };

        if let Some(tx) = target_tx {
            let _ = tx.send(ServerMessage::Signal { payload });
        }
    }
}

async fn leave_room(state: &AppState, room_id: &str, client_id: &str) {
    let mut rooms = state.rooms.write().await;
    let mut should_remove = false;

    if let Some(room) = rooms.get_mut(room_id) {
        if let Some((ref id, _)) = room.initiator {
            if id == client_id {
                room.initiator = None;
                if let Some((_, ref resp_tx)) = room.responder {
                    let _ = resp_tx.send(ServerMessage::PeerLeft);
                }
            }
        }

        if let Some((ref id, _)) = room.responder {
            if id == client_id {
                room.responder = None;
                if let Some((_, ref init_tx)) = room.initiator {
                    let _ = init_tx.send(ServerMessage::PeerLeft);
                }
            }
        }

        if room.initiator.is_none() && room.responder.is_none() {
            should_remove = true;
        }
    }

    if should_remove {
        rooms.remove(room_id);
        info!("Room destroyed (0 peers left). Zero persistence maintained.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::EncryptedPayload;

    #[tokio::test]
    async fn test_room_lifecycle_and_zero_persistence() {
        let state = AppState::default();
        let room_id = "test-room-123";

        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let (tx3, mut rx3) = mpsc::unbounded_channel();

        // 1. Peer 1 joins as initiator
        join_room(&state, room_id, "peer1", tx1).await;
        match rx1.recv().await.unwrap() {
            ServerMessage::Joined { peer_count, is_initiator, .. } => {
                assert_eq!(peer_count, 1);
                assert!(is_initiator);
            }
            other => panic!("Unexpected msg: {:?}", other),
        }

        // 2. Peer 2 joins as responder
        join_room(&state, room_id, "peer2", tx2).await;
        match rx2.recv().await.unwrap() {
            ServerMessage::Joined { peer_count, is_initiator, .. } => {
                assert_eq!(peer_count, 2);
                assert!(!is_initiator);
            }
            other => panic!("Unexpected msg: {:?}", other),
        }
        // Peer 1 should receive PeerJoined
        match rx1.recv().await.unwrap() {
            ServerMessage::PeerJoined => {}
            other => panic!("Expected PeerJoined for peer 1, got {:?}", other),
        }

        // 3. Peer 3 attempts to join -> rejected (room is full)
        join_room(&state, room_id, "peer3", tx3).await;
        match rx3.recv().await.unwrap() {
            ServerMessage::Error { .. } => {}
            other => panic!("Expected Error for peer 3, got {:?}", other),
        }

        // 4. Signal forwarding between peers
        let payload = EncryptedPayload {
            nonce: "testnonce".into(),
            ciphertext: "testciphertext".into(),
        };
        forward_signal(&state, room_id, "peer1", payload.clone()).await;
        match rx2.recv().await.unwrap() {
            ServerMessage::Signal { payload: p } => {
                assert_eq!(p, payload);
            }
            other => panic!("Expected Signal for peer 2, got {:?}", other),
        }

        // 5. Peer 2 leaves -> Peer 1 receives PeerLeft
        leave_room(&state, room_id, "peer2").await;
        match rx1.recv().await.unwrap() {
            ServerMessage::PeerLeft => {}
            other => panic!("Expected PeerLeft, got {:?}", other),
        }

        // 6. Peer 1 leaves -> Room is destroyed from state (zero persistence)
        leave_room(&state, room_id, "peer1").await;
        let rooms = state.rooms.read().await;
        assert!(!rooms.contains_key(room_id), "Room must be destroyed when all peers leave");
    }
}
