use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use protocol::crypto::{decrypt_json, encrypt_json, EncryptedPayload};
use protocol::{
    hash_room_topic, verify_event, ClientRelayMessage, NostrBurnerKey, NostrFilter,
    RelayClientMessage, SignalPayload, KIND_EPHEMERAL_SIGNAL, KEY_LENGTH,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

pub struct NostrRelayPool {
    pub burner_key: NostrBurnerKey,
    #[allow(dead_code)]
    pub room_id: String,
    pub key: [u8; KEY_LENGTH],
    pub topic: String,
    pub active_senders: Rc<RefCell<Vec<mpsc::UnboundedSender<String>>>>,
    pub seen_event_ids: Rc<RefCell<HashSet<String>>>,
    pub connected_count: Rc<RefCell<usize>>,
}

impl NostrRelayPool {
    pub fn new(
        room_id: String,
        key: [u8; KEY_LENGTH],
        relays: Vec<String>,
        on_signal: Rc<dyn Fn(String, SignalPayload)>,
        on_relay_connected: Rc<dyn Fn(usize)>,
    ) -> Result<Rc<Self>, String> {
        let burner_key = NostrBurnerKey::generate()
            .map_err(|e| format!("Failed to generate Nostr burner key: {e}"))?;
        let topic = hash_room_topic(&room_id);

        let pool = Rc::new(Self {
            burner_key,
            room_id,
            key,
            topic,
            active_senders: Rc::new(RefCell::new(Vec::new())),
            seen_event_ids: Rc::new(RefCell::new(HashSet::new())),
            connected_count: Rc::new(RefCell::new(0)),
        });

        log::info!(
            "Initializing Nostr relay pool with {} relays for topic {}",
            relays.len(),
            pool.topic
        );

        for relay_url in relays {
            let pool_c = pool.clone();
            let on_signal_c = on_signal.clone();
            let on_relay_connected_c = on_relay_connected.clone();
            let relay_url_c = relay_url.clone();

            wasm_bindgen_futures::spawn_local(async move {
                log::info!("Connecting to Nostr relay: {}", relay_url_c);
                let ws = match WebSocket::open(&relay_url_c) {
                    Ok(w) => w,
                    Err(err) => {
                        log::warn!("Failed to open Nostr relay WebSocket {}: {:?}", relay_url_c, err);
                        return;
                    }
                };

                let (mut ws_sink, mut ws_stream) = ws.split();
                let (tx, mut rx) = mpsc::unbounded::<String>();

                // Spawn forwarder task from tx to ws_sink
                wasm_bindgen_futures::spawn_local(async move {
                    while let Some(text) = rx.next().await {
                        if ws_sink.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                });

                // Send NIP-01 REQ subscription for kind 20001 ephemeral room topic
                let req_msg = ClientRelayMessage::Req {
                    sub_id: format!("sub-{}", &pool_c.topic[0..12]),
                    filters: vec![NostrFilter {
                        kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
                        d_tags: Some(vec![pool_c.topic.clone()]),
                        ..Default::default()
                    }],
                };

                if let Ok(json) = req_msg.to_json() {
                    let _ = tx.unbounded_send(json);
                }

                // Register sender in active pool
                pool_c.active_senders.borrow_mut().push(tx.clone());
                let count = {
                    let mut c = pool_c.connected_count.borrow_mut();
                    *c += 1;
                    *c
                };
                on_relay_connected_c(count);
                log::info!("Connected to Nostr relay: {} (active: {})", relay_url_c, count);

                // Send immediate presence broadcast on connection
                pool_c.broadcast_signal(&SignalPayload::Presence);

                // Listen for incoming relay messages
                while let Some(msg_res) = ws_stream.next().await {
                    let text = match msg_res {
                        Ok(Message::Text(t)) => t,
                        _ => continue,
                    };

                    let client_msg = match RelayClientMessage::from_json(&text) {
                        Ok(Some(m)) => m,
                        _ => continue,
                    };

                    if let RelayClientMessage::Event { event, .. } = client_msg {
                        // 1. Filter out our own events
                        if event.pubkey == pool_c.burner_key.pubkey() {
                            continue;
                        }

                        // 2. Deduplicate across multi-relay deliveries
                        {
                            let mut seen = pool_c.seen_event_ids.borrow_mut();
                            if seen.contains(&event.id) {
                                continue;
                            }
                            seen.insert(event.id.clone());
                        }

                        // 3. Verify Schnorr signature
                        if let Ok(valid) = verify_event(&event) {
                            if !valid {
                                log::warn!("Received Nostr event with invalid signature, dropping");
                                continue;
                            }
                        } else {
                            continue;
                        }

                        // 4. Decrypt symmetric ciphertext with room secret key
                        let enc_payload: EncryptedPayload = match serde_json::from_str(&event.content) {
                            Ok(ep) => ep,
                            Err(_) => continue,
                        };

                        if let Ok(signal) = decrypt_json::<SignalPayload>(&pool_c.key, &enc_payload) {
                            on_signal_c(event.pubkey, signal);
                        } else {
                            log::warn!("Failed to decrypt incoming Nostr signal payload with room key");
                        }
                    }
                }

                // On disconnect
                let count = {
                    let mut c = pool_c.connected_count.borrow_mut();
                    if *c > 0 {
                        *c -= 1;
                    }
                    *c
                };
                on_relay_connected_c(count);
                log::warn!("Disconnected from Nostr relay: {}", relay_url_c);
            });
        }

        Ok(pool)
    }

    pub fn self_pubkey(&self) -> &str {
        self.burner_key.pubkey()
    }

    /// Broadcast a SignalPayload across all active relays in the pool.
    pub fn broadcast_signal(&self, signal: &SignalPayload) {
        let enc_payload = match encrypt_json(&self.key, signal) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to encrypt signaling payload: {:?}", e);
                return;
            }
        };

        let content_json = match serde_json::to_string(&enc_payload) {
            Ok(j) => j,
            Err(e) => {
                log::error!("Failed to serialize encrypted payload: {:?}", e);
                return;
            }
        };

        let now_sec = (js_sys::Date::now() / 1000.0) as u64;
        let event = match self.burner_key.create_event(
            KIND_EPHEMERAL_SIGNAL,
            vec![vec!["d".to_string(), self.topic.clone()]],
            content_json,
            now_sec,
        ) {
            Ok(ev) => ev,
            Err(e) => {
                log::error!("Failed to create Nostr event: {:?}", e);
                return;
            }
        };

        let client_msg = ClientRelayMessage::Event(event);
        let msg_json = match client_msg.to_json() {
            Ok(j) => j,
            Err(e) => {
                log::error!("Failed to serialize ClientRelayMessage: {:?}", e);
                return;
            }
        };

        let mut senders = self.active_senders.borrow_mut();
        // Remove closed senders
        senders.retain(|tx| tx.unbounded_send(msg_json.clone()).is_ok());
    }
}
