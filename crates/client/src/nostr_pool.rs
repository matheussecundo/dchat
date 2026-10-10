use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use protocol::crypto::{decrypt_bytes, encrypt_bytes, EncryptedPayload};
use protocol::{
    decode_signal, encode_signal, relay_key_message, verify_event, verify_message, ClientRelayMessage, DecodedSignal,
    GossipDedup, NostrBurnerKey, NostrFilter, RelayClientMessage, RelayFrame, RelaySignal, SignalPayload,
    KIND_EPHEMERAL_SIGNAL, KEY_LENGTH, PROTOCOL_VERSION,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Reconnect delays: start small, double up to the cap, reset after a stable connection.
const RECONNECT_INITIAL_MS: f64 = 1000.0;
const RECONNECT_MAX_MS: f64 = 30_000.0;
const STABLE_CONNECTION_MS: f64 = 60_000.0;
const SEEN_EVENTS_CAPACITY: usize = 4096;

/// The protocol version this tab speaks. E2E builds can pretend to be another version
/// (`window.__dchatProtocolVersion`, set before the app loads).
pub fn protocol_version() -> u32 {
    #[cfg(feature = "e2e-hooks")]
    {
        let forced = web_sys::window()
            .and_then(|w| js_sys::Reflect::get(&w, &"__dchatProtocolVersion".into()).ok())
            .and_then(|v| v.as_f64());
        if let Some(version) = forced {
            return version as u32;
        }
    }
    PROTOCOL_VERSION
}

/// Relay keys whose identity certificate was checked, at most this many remembered.
const CERTIFIED_CAPACITY: usize = 1024;

/// What the pool reports to the session.
pub struct PoolEvents {
    /// A signal from the member `pubkey` (its identity), addressed to us or to everyone.
    pub on_signal: Box<dyn Fn(String, SignalPayload)>,
    /// The member behind relay key `pubkey` speaks another protocol version; its signals are dropped.
    pub on_other_version: Box<dyn Fn(String, u32)>,
    /// How many relays are connected now.
    pub on_relay_connected: Box<dyn Fn(usize)>,
}

/// Connections to the room's relays. Each relay runs its own loop that keeps the
/// subscription alive and reconnects with backoff until the pool is closed.
pub struct NostrRelayPool {
    /// Signs this session's relay events. Fresh every session, so relays can't link rooms.
    relay_key: NostrBurnerKey,
    /// The member's identity: addressing, sealing, and the certificate below.
    identity: Rc<NostrBurnerKey>,
    /// `identity`'s signature over `relay_key_message(topic, relay key)`.
    cert: String,
    /// Relay key → the identity whose certificate it carried.
    certified: RefCell<HashMap<String, String>>,
    pub key: [u8; KEY_LENGTH],
    pub topic: String,
    /// Signals carry it; members on another version are reported, never linked.
    pub version: u32,
    /// Outgoing queue of each relay that currently has an open connection.
    senders: RefCell<HashMap<usize, mpsc::UnboundedSender<String>>>,
    seen_event_ids: RefCell<GossipDedup>,
    connected_count: Cell<usize>,
    closed: Cell<bool>,
}

impl NostrRelayPool {
    /// `identity` is the member's session key (it also signs room envelopes, and survives a
    /// rekey); relay events are signed by a fresh key that `identity` certifies inside each
    /// encrypted frame. `topic` is the room's relay topic (`hash_room_topic`, or
    /// `password_room_topic`).
    pub fn new(
        topic: String,
        key: [u8; KEY_LENGTH],
        identity: Rc<NostrBurnerKey>,
        relays: Vec<String>,
        events: PoolEvents,
    ) -> Result<Rc<Self>, String> {
        let events = Rc::new(events);
        let relay_key = NostrBurnerKey::generate().map_err(|e| e.to_string())?;
        let cert = identity
            .sign_message(&relay_key_message(&topic, relay_key.pubkey()))
            .map_err(|e| e.to_string())?;
        let pool = Rc::new(Self {
            relay_key,
            identity,
            cert,
            certified: RefCell::new(HashMap::new()),
            key,
            topic,
            version: protocol_version(),
            senders: RefCell::new(HashMap::new()),
            seen_event_ids: RefCell::new(GossipDedup::new(SEEN_EVENTS_CAPACITY)),
            connected_count: Cell::new(0),
            closed: Cell::new(false),
        });
        log::info!("Initializing Nostr relay pool with {} relays", relays.len());
        for (index, url) in relays.into_iter().enumerate() {
            let pool = pool.clone();
            let events = events.clone();
            wasm_bindgen_futures::spawn_local(async move {
                pool.run_relay(index, url, events).await;
            });
        }
        Ok(pool)
    }

    /// Stop every relay loop and close their connections.
    pub fn close(&self) {
        self.closed.set(true);
        // Dropping the senders ends each forwarder, which closes its socket.
        self.senders.borrow_mut().clear();
    }

    async fn run_relay(self: Rc<Self>, index: usize, url: String, events: Rc<PoolEvents>) {
        let mut backoff = RECONNECT_INITIAL_MS;
        while !self.closed.get() {
            let started = js_sys::Date::now();
            match WebSocket::open(&url) {
                Ok(ws) => self.serve_connection(index, &url, ws, &events).await,
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
    async fn serve_connection(&self, index: usize, url: &str, ws: WebSocket, events: &PoolEvents) {
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
                (events.on_relay_connected)(self.connected_count.get());
                log::info!("Connected to Nostr relay {}", url);
            }
            if let Ok(Some(RelayClientMessage::Event { event, .. })) = RelayClientMessage::from_json(&text) {
                self.handle_event(event, events);
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
            (events.on_relay_connected)(self.connected_count.get());
        }
        log::warn!("Disconnected from Nostr relay {}", url);
    }

    fn handle_event(&self, event: protocol::NostrEvent, events: &PoolEvents) {
        // 1. Our own events echo back from some relays.
        if event.pubkey == self.relay_key.pubkey() {
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
        // 4. Decrypt with the room key.
        let Ok(encrypted) = serde_json::from_str::<EncryptedPayload>(&event.content) else {
            return;
        };
        let Ok(plaintext) = decrypt_bytes(&self.key, &encrypted) else {
            log::warn!("Failed to decrypt incoming Nostr signal payload with room key");
            return;
        };
        // 5. Same protocol version only; the relay key must be certified by the member it
        // claims; signals addressed to other members are dropped, and ours are opened with
        // our identity.
        match decode_signal(&plaintext, self.version) {
            DecodedSignal::Signal(RelayFrame { from, cert, signal: relayed }) => {
                if from == self.identity.pubkey() || !self.certifies(&event.pubkey, &from, &cert) {
                    return;
                }
                if relayed.recipient().is_some_and(|to| to != self.identity.pubkey()) {
                    return;
                }
                match relayed.open(&self.identity, &from) {
                    Some(signal) => (events.on_signal)(from, signal),
                    None => log::warn!("Dropping a sealed signal that does not open"),
                }
            }
            DecodedSignal::OtherVersion(version) => (events.on_other_version)(event.pubkey, version),
            DecodedSignal::Invalid => log::warn!("Dropping an unreadable signal"),
        }
    }

    /// Whether `identity` certified `relay_pubkey` (checked once per relay key). A relay key
    /// speaks for one member only.
    fn certifies(&self, relay_pubkey: &str, identity: &str, cert: &str) -> bool {
        if let Some(known) = self.certified.borrow().get(relay_pubkey) {
            return known == identity;
        }
        if !verify_message(identity, &relay_key_message(&self.topic, relay_pubkey), cert) {
            log::warn!("Dropping a signal whose relay key is not certified");
            return false;
        }
        let mut certified = self.certified.borrow_mut();
        if certified.len() >= CERTIFIED_CAPACITY {
            certified.clear();
        }
        certified.insert(relay_pubkey.to_string(), identity.to_string());
        true
    }

    /// Seal (when addressed), encrypt, sign and serialize `signal` as an EVENT message.
    fn signed_event_json(&self, signal: &SignalPayload) -> Option<String> {
        let relayed = RelaySignal::seal(&self.identity, signal)
            .map_err(|e| log::error!("Failed to seal signaling payload: {:?}", e))
            .ok()?;
        let frame = RelayFrame { from: self.identity.pubkey().to_string(), cert: self.cert.clone(), signal: relayed };
        let plaintext = encode_signal(&frame, self.version).ok()?;
        let encrypted = encrypt_bytes(&self.key, &plaintext)
            .map_err(|e| log::error!("Failed to encrypt signaling payload: {:?}", e))
            .ok()?;
        let content = serde_json::to_string(&encrypted).ok()?;
        let now_sec = (js_sys::Date::now() / 1000.0) as u64;
        let event = self
            .relay_key
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
