use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use protocol::crypto::{decrypt_json, encrypt_json, EncryptedPayload};
use protocol::{
    hash_room_topic, verify_event, ClientRelayMessage, GossipDedup, NostrBurnerKey, NostrFilter,
    RelayClientMessage, SignalPayload, KIND_EPHEMERAL_SIGNAL, KEY_LENGTH,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Reconnect delays: start small, double up to the cap, reset after a stable connection.
const RECONNECT_INITIAL_MS: f64 = 1000.0;
const RECONNECT_MAX_MS: f64 = 30_000.0;
const STABLE_CONNECTION_MS: f64 = 60_000.0;
const SEEN_EVENTS_CAPACITY: usize = 4096;

/// Connections to the room's relays. Each relay runs its own loop that keeps the
/// subscription alive and reconnects with backoff until the pool is closed.
pub struct NostrRelayPool {
    pub burner_key: Rc<NostrBurnerKey>,
    pub key: [u8; KEY_LENGTH],
    pub topic: String,
    /// Outgoing queue of each relay that currently has an open connection.
    senders: RefCell<HashMap<usize, mpsc::UnboundedSender<String>>>,
    seen_event_ids: RefCell<GossipDedup>,
    connected_count: Cell<usize>,
    closed: Cell<bool>,
}

impl NostrRelayPool {
    /// `burner_key` is the session identity: it signs relay events here and room
    /// envelopes in the session, so peers can tie both to the same member.
    pub fn new(
        room_id: String,
        key: [u8; KEY_LENGTH],
        burner_key: Rc<NostrBurnerKey>,
        relays: Vec<String>,
        on_signal: Rc<dyn Fn(String, SignalPayload)>,
        on_relay_connected: Rc<dyn Fn(usize)>,
    ) -> Rc<Self> {
        let pool = Rc::new(Self {
            burner_key,
            key,
            topic: hash_room_topic(&room_id),
            senders: RefCell::new(HashMap::new()),
            seen_event_ids: RefCell::new(GossipDedup::new(SEEN_EVENTS_CAPACITY)),
            connected_count: Cell::new(0),
            closed: Cell::new(false),
        });
        log::info!("Initializing Nostr relay pool with {} relays", relays.len());
        for (index, url) in relays.into_iter().enumerate() {
            let pool = pool.clone();
            let on_signal = on_signal.clone();
            let on_relay_connected = on_relay_connected.clone();
            wasm_bindgen_futures::spawn_local(async move {
                pool.run_relay(index, url, on_signal, on_relay_connected).await;
            });
        }
        pool
    }

    /// Stop every relay loop and close their connections.
    pub fn close(&self) {
        self.closed.set(true);
        // Dropping the senders ends each forwarder, which closes its socket.
        self.senders.borrow_mut().clear();
    }

    async fn run_relay(
        self: Rc<Self>,
        index: usize,
        url: String,
        on_signal: Rc<dyn Fn(String, SignalPayload)>,
        on_relay_connected: Rc<dyn Fn(usize)>,
    ) {
        let mut backoff = RECONNECT_INITIAL_MS;
        while !self.closed.get() {
            let started = js_sys::Date::now();
            match WebSocket::open(&url) {
                Ok(ws) => self.serve_connection(index, &url, ws, &on_signal, &on_relay_connected).await,
                Err(err) => log::warn!("Cannot open Nostr relay {}: {:?}", url, err),
            }
            if self.closed.get() {
                return;
            }
            if js_sys::Date::now() - started > STABLE_CONNECTION_MS {
                backoff = RECONNECT_INITIAL_MS;
            }
            // Jitter keeps a room's members from reconnecting in lockstep after a relay restart.
            let delay = backoff * (0.75 + 0.5 * js_sys::Math::random());
            log::info!("Reconnecting to Nostr relay {} in {:.1}s", url, delay / 1000.0);
            crate::media::sleep_ms(delay as i32).await;
            backoff = (backoff * 2.0).min(RECONNECT_MAX_MS);
        }
    }

    /// Subscribe, announce presence and route incoming events until the connection ends.
    async fn serve_connection(
        &self,
        index: usize,
        url: &str,
        ws: WebSocket,
        on_signal: &Rc<dyn Fn(String, SignalPayload)>,
        on_relay_connected: &Rc<dyn Fn(usize)>,
    ) {
        let (mut ws_sink, mut ws_stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded::<String>();
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(text) = rx.next().await {
                if ws_sink.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            let _ = ws_sink.close().await;
        });

        // Queued until the socket opens.
        let req = ClientRelayMessage::Req {
            sub_id: format!("sub-{}", &self.topic[0..12]),
            filters: vec![NostrFilter {
                kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
                d_tags: Some(vec![self.topic.clone()]),
                ..Default::default()
            }],
        };
        if let Ok(json) = req.to_json() {
            let _ = tx.unbounded_send(json);
        }
        if let Some(presence) = self.signed_event_json(&SignalPayload::Presence) {
            let _ = tx.unbounded_send(presence);
        }
        if self.closed.get() {
            return;
        }
        self.senders.borrow_mut().insert(index, tx.clone());

        // Counted as connected once the relay answers (EOSE to our subscription).
        let mut counted = false;
        while let Some(msg_res) = ws_stream.next().await {
            let text = match msg_res {
                Ok(Message::Text(t)) => t,
                Ok(_) => continue,
                Err(_) => break,
            };
            if !counted {
                counted = true;
                self.connected_count.set(self.connected_count.get() + 1);
                on_relay_connected(self.connected_count.get());
                log::info!("Connected to Nostr relay {}", url);
            }
            if let Ok(Some(RelayClientMessage::Event { event, .. })) = RelayClientMessage::from_json(&text) {
                self.handle_event(event, on_signal);
            }
        }

        // Only remove our own sender (a newer connection may have replaced it).
        let mut senders = self.senders.borrow_mut();
        if senders.get(&index).is_some_and(|current| current.same_receiver(&tx)) {
            senders.remove(&index);
        }
        drop(senders);
        if counted {
            self.connected_count.set(self.connected_count.get().saturating_sub(1));
            on_relay_connected(self.connected_count.get());
        }
        log::warn!("Disconnected from Nostr relay {}", url);
    }

    fn handle_event(&self, event: protocol::NostrEvent, on_signal: &Rc<dyn Fn(String, SignalPayload)>) {
        // 1. Our own events echo back from some relays.
        if event.pubkey == self.burner_key.pubkey() {
            return;
        }
        // 2. The same event arrives from every relay in the pool.
        if !self.seen_event_ids.borrow_mut().insert(&event.id) {
            return;
        }
        // 3. Signature.
        if !verify_event(&event).unwrap_or(false) {
            log::warn!("Received Nostr event with invalid signature, dropping");
            return;
        }
        // 4. Decrypt with the room key; signals addressed to other members are dropped.
        let Ok(encrypted) = serde_json::from_str::<EncryptedPayload>(&event.content) else {
            return;
        };
        match decrypt_json::<SignalPayload>(&self.key, &encrypted) {
            Ok(signal) => {
                if signal.recipient().is_some_and(|to| to != self.burner_key.pubkey()) {
                    return;
                }
                on_signal(event.pubkey, signal);
            }
            Err(_) => log::warn!("Failed to decrypt incoming Nostr signal payload with room key"),
        }
    }

    /// Encrypt, sign and serialize `signal` as an EVENT message.
    fn signed_event_json(&self, signal: &SignalPayload) -> Option<String> {
        let encrypted = encrypt_json(&self.key, signal)
            .map_err(|e| log::error!("Failed to encrypt signaling payload: {:?}", e))
            .ok()?;
        let content = serde_json::to_string(&encrypted).ok()?;
        let now_sec = (js_sys::Date::now() / 1000.0) as u64;
        let event = self
            .burner_key
            .create_event(KIND_EPHEMERAL_SIGNAL, vec![vec!["d".to_string(), self.topic.clone()]], content, now_sec)
            .map_err(|e| log::error!("Failed to create Nostr event: {:?}", e))
            .ok()?;
        ClientRelayMessage::Event(event).to_json().ok()
    }

    /// Broadcast a SignalPayload across all connected relays in the pool.
    pub fn broadcast_signal(&self, signal: &SignalPayload) {
        if self.closed.get() {
            return;
        }
        let Some(msg_json) = self.signed_event_json(signal) else {
            return;
        };
        self.senders
            .borrow_mut()
            .retain(|_, tx| tx.unbounded_send(msg_json.clone()).is_ok());
    }
}
