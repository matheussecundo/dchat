//! A group room session: one `PeerLink` per member (full mesh), a signed gossip layer
//! for room messages, and the roster that drives the member list and caps.
//! Everything lives in RAM and is gone on reload.

use crate::mesh::{LinkEvent, LinkEventHandler, PeerLink};
use crate::names::pubkey_tag;
use crate::nostr_pool::NostrRelayPool;
use crate::state::{
    current_fragment, current_time_string, get_default_relays, ChatMessageUi, ConnectionStatus,
    LinkUi, MemberUi, Notice,
};
use leptos::*;
use protocol::crypto::{decrypt_json, encrypt_json};
use protocol::{
    admin_proof_message, verify_message, EncryptedPayload, GossipDedup, Member, NostrBurnerKey,
    RoomBody, RoomEnvelope, RoomParams, Roster, SignalPayload, KEY_LENGTH,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{window, RtcConfiguration};

const DEFAULT_STUN: &str = "stun:stun.l.google.com:19302";
const TICK_MS: i32 = 2500;
/// Beacon every tick while joining, then every `SLOW_BEACON_EVERY` ticks.
const FAST_BEACON_FOR_MS: f64 = 20_000.0;
const SLOW_BEACON_EVERY: u32 = 6;
/// A link that has not opened by then is dropped (e.g. NAT traversal failed).
const LINK_TIMEOUT_MS: f64 = 20_000.0;
/// After a failed link, wait this long before trying the same member again.
const RETRY_BACKOFF_MS: f64 = 30_000.0;
const PRESENCE_ECHO_MIN_MS: f64 = 1000.0;
const DEDUP_CAPACITY: usize = 4096;
const MAX_REMOTE_NAME_CHARS: usize = 32;

/// Reactive outputs of the session for the UI.
#[derive(Clone, Copy)]
pub struct SessionSignals {
    pub status: WriteSignal<ConnectionStatus>,
    pub messages: WriteSignal<Vec<ChatMessageUi>>,
    pub members: WriteSignal<Vec<MemberUi>>,
    /// Display names by pubkey, never pruned, so old messages keep their author's name.
    pub names: WriteSignal<HashMap<String, String>>,
    pub room_full: WriteSignal<bool>,
    pub connected_relays: WriteSignal<usize>,
}

#[derive(Clone)]
pub struct RoomSession {
    inner: Rc<Inner>,
}

struct Inner {
    key: [u8; KEY_LENGTH],
    params: RoomParams,
    room_id: String,
    identity: Rc<NostrBurnerKey>,
    me: String,
    join_ts: u64,
    started_at: f64,
    rtc_config: RtcConfiguration,
    signals: SessionSignals,
    pool: RefCell<Option<Rc<NostrRelayPool>>>,
    links: RefCell<HashMap<String, Rc<PeerLink>>>,
    retry_after: RefCell<HashMap<String, f64>>,
    roster: RefCell<Roster>,
    dedup: RefCell<GossipDedup>,
    /// Latest signed Hello and LinkState of every member, replayed to each new neighbor
    /// so it learns the whole room even before it links to everyone.
    hellos: RefCell<HashMap<String, RoomEnvelope>>,
    link_reports: RefCell<HashMap<String, RoomEnvelope>>,
    link_seq: Cell<u64>,
    /// Display names by pubkey, kept after a member leaves for the "left" notice.
    names: RefCell<HashMap<String, String>>,
    present: RefCell<HashSet<String>>,
    late_join_noted: Cell<bool>,
    /// Members this tab refuses direct links with (test hook only).
    blocked: RefCell<HashSet<String>>,
    relays_connected: Cell<usize>,
    last_presence_echo: Cell<f64>,
    closed: Cell<bool>,
}

impl RoomSession {
    pub fn start(
        room_id: String,
        key: [u8; KEY_LENGTH],
        name: String,
        signals: SessionSignals,
    ) -> Result<Self, String> {
        let params = RoomParams::from_fragment(&current_fragment());
        let identity = Rc::new(NostrBurnerKey::generate().map_err(|e| e.to_string())?);
        let me = identity.pubkey().to_string();
        let started_at = js_sys::Date::now();
        let admin_proof = params
            .admin_secret
            .as_deref()
            .and_then(|secret| NostrBurnerKey::from_secret_hex(secret).ok())
            .filter(|admin| params.admin_pubkey.as_deref() == Some(admin.pubkey()))
            .and_then(|admin| admin.sign_message(&admin_proof_message(&room_id, &me)).ok());

        let session = Self {
            inner: Rc::new(Inner {
                key,
                rtc_config: build_rtc_config(&params),
                params,
                room_id: room_id.clone(),
                identity: identity.clone(),
                me,
                join_ts: started_at as u64,
                started_at,
                signals,
                pool: RefCell::new(None),
                links: RefCell::new(HashMap::new()),
                retry_after: RefCell::new(HashMap::new()),
                roster: RefCell::new(Roster::default()),
                dedup: RefCell::new(GossipDedup::new(DEDUP_CAPACITY)),
                hellos: RefCell::new(HashMap::new()),
                link_reports: RefCell::new(HashMap::new()),
                link_seq: Cell::new(0),
                names: RefCell::new(HashMap::new()),
                present: RefCell::new(HashSet::new()),
                late_join_noted: Cell::new(false),
                blocked: RefCell::new(HashSet::new()),
                relays_connected: Cell::new(0),
                last_presence_echo: Cell::new(0.0),
                closed: Cell::new(false),
            }),
        };

        session.publish(RoomBody::Hello {
            name,
            join_ts: session.inner.join_ts,
            admin_proof,
        });
        session.publish_link_state();

        let on_signal: Rc<dyn Fn(String, SignalPayload)> = {
            let s = session.clone();
            Rc::new(move |from, signal| s.on_signal(from, signal))
        };
        let on_relay_connected: Rc<dyn Fn(usize)> = {
            let s = session.clone();
            Rc::new(move |count| {
                s.inner.relays_connected.set(count);
                s.inner.signals.connected_relays.set(count);
                s.refresh_status();
            })
        };
        let pool = NostrRelayPool::new(
            room_id,
            key,
            identity,
            get_default_relays(),
            on_signal,
            on_relay_connected,
        );
        *session.inner.pool.borrow_mut() = Some(pool);

        session.start_ticker();
        session.leave_on_pagehide();
        session.recompute();
        #[cfg(feature = "e2e-hooks")]
        session.install_e2e_hooks();
        Ok(session)
    }

    pub fn send_chat(&self, text: &str) -> Result<(), String> {
        if !self.has_open_link() {
            return Err("No member is connected yet".into());
        }
        self.publish(RoomBody::Chat { text: text.to_string() })
            .map(|_| ())
            .ok_or_else(|| "Failed to sign message".into())
    }

    /// Announce departure and tear down every link. Idempotent.
    pub fn leave(&self) {
        if self.inner.closed.get() {
            return;
        }
        if let Some(pool) = self.pool() {
            pool.broadcast_signal(&SignalPayload::PeerLeft);
        }
        self.publish(RoomBody::Leave);
        self.inner.closed.set(true);
        for (_, link) in self.inner.links.borrow_mut().drain() {
            link.close();
        }
        self.refresh_status();
    }

    // ---- Signaling -------------------------------------------------------------------

    fn on_signal(&self, from: String, signal: SignalPayload) {
        if self.inner.closed.get() || self.inner.blocked.borrow().contains(&from) {
            return;
        }
        match signal {
            SignalPayload::Presence => self.on_presence(&from),
            SignalPayload::Offer { sdp, .. } => {
                let link = self.link(&from).or_else(|| self.create_link(&from, false));
                if let Some(link) = link {
                    wasm_bindgen_futures::spawn_local(async move {
                        link.handle_description(true, sdp).await;
                    });
                }
            }
            SignalPayload::Answer { sdp, .. } => {
                if let Some(link) = self.link(&from) {
                    wasm_bindgen_futures::spawn_local(async move {
                        link.handle_description(false, sdp).await;
                    });
                }
            }
            SignalPayload::IceBatch { candidates, .. } => {
                // Candidates may overtake the offer through another relay: the answering
                // side (higher pubkey) creates its link early and queues them.
                let link = self.link(&from).or_else(|| {
                    (self.inner.me.as_str() > from.as_str())
                        .then(|| self.create_link(&from, false))
                        .flatten()
                });
                if let Some(link) = link {
                    wasm_bindgen_futures::spawn_local(async move {
                        link.add_candidates(candidates).await;
                    });
                }
            }
            SignalPayload::PeerLeft => self.remove_member(&from),
        }
    }

    /// A member announced itself. The lower pubkey of each pair dials; the higher one
    /// answers with its own presence so the lower one learns about it.
    fn on_presence(&self, from: &str) {
        if self.inner.links.borrow().contains_key(from) {
            return;
        }
        let now = js_sys::Date::now();
        if self.inner.retry_after.borrow().get(from).is_some_and(|t| *t > now) {
            return;
        }
        if self.inner.me.as_str() < from {
            self.create_link(from, true);
        } else if now - self.inner.last_presence_echo.get() > PRESENCE_ECHO_MIN_MS {
            self.inner.last_presence_echo.set(now);
            if let Some(pool) = self.pool() {
                pool.broadcast_signal(&SignalPayload::Presence);
            }
        }
    }

    fn create_link(&self, remote: &str, initiator: bool) -> Option<Rc<PeerLink>> {
        if remote == self.inner.me || self.inner.blocked.borrow().contains(remote) {
            return None;
        }
        let pool = self.pool()?;
        let handler: LinkEventHandler = {
            let s = self.clone();
            Rc::new(move |remote, id, event| s.on_link_event(remote, id, event))
        };
        match PeerLink::new(&self.inner.me, remote, &self.inner.rtc_config, pool, handler, initiator) {
            Ok(link) => {
                self.inner.links.borrow_mut().insert(remote.to_string(), link.clone());
                self.recompute();
                Some(link)
            }
            Err(err) => {
                log::error!("Failed to create peer connection: {:?}", err);
                None
            }
        }
    }

    fn on_link_event(&self, remote: &str, id: u64, event: LinkEvent) {
        let is_current = self.inner.links.borrow().get(remote).map(|l| l.id) == Some(id);
        if !is_current || self.inner.closed.get() {
            return;
        }
        match event {
            LinkEvent::Open => {
                log::info!("Direct link open with {}", pubkey_tag(remote));
                self.inner.retry_after.borrow_mut().remove(remote);
                self.publish_link_state();
                self.sync_to(remote);
                self.recompute();
            }
            LinkEvent::Closed => self.drop_link(remote, true),
            LinkEvent::Message(text) => self.on_frame(remote, &text),
        }
    }

    fn drop_link(&self, remote: &str, failed: bool) {
        let Some(link) = self.inner.links.borrow_mut().remove(remote) else {
            return;
        };
        link.close();
        if failed {
            self.inner
                .retry_after
                .borrow_mut()
                .insert(remote.to_string(), js_sys::Date::now() + RETRY_BACKOFF_MS);
        }
        self.publish_link_state();
        self.recompute();
    }

    // ---- Room messages ---------------------------------------------------------------

    /// Sign `body` as this member, apply it locally and send it to every direct neighbor
    /// (they relay it to members without a direct link to us).
    fn publish(&self, body: RoomBody) -> Option<RoomEnvelope> {
        let envelope = RoomEnvelope::sign(&self.inner.identity, js_sys::Date::now() as u64, body).ok()?;
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        self.apply(&envelope);
        if let Some(frame) = self.encode(&envelope) {
            for link in self.inner.links.borrow().values() {
                link.send(&frame);
            }
        }
        Some(envelope)
    }

    fn publish_link_state(&self) {
        let mut direct: Vec<String> = self
            .inner
            .links
            .borrow()
            .values()
            .filter(|l| l.is_open())
            .map(|l| l.remote.clone())
            .collect();
        direct.sort();
        let unchanged = self
            .inner
            .roster
            .borrow()
            .links_of(&self.inner.me)
            .is_some_and(|known| known.iter().eq(direct.iter()));
        if unchanged {
            return;
        }
        let seq = self.inner.link_seq.get() + 1;
        self.inner.link_seq.set(seq);
        self.publish(RoomBody::LinkState { seq, direct });
    }

    /// Hand a new neighbor every member's latest Hello and LinkState.
    fn sync_to(&self, remote: &str) {
        let Some(link) = self.link(remote) else {
            return;
        };
        let hellos = self.inner.hellos.borrow();
        let reports = self.inner.link_reports.borrow();
        for envelope in hellos.values().chain(reports.values()) {
            if let Some(frame) = self.encode(envelope) {
                link.send(&frame);
            }
        }
    }

    fn on_frame(&self, from: &str, frame: &str) {
        let Ok(encrypted) = serde_json::from_str::<EncryptedPayload>(frame) else {
            return;
        };
        let Ok(envelope) = decrypt_json::<RoomEnvelope>(&self.inner.key, &encrypted) else {
            log::warn!("Dropping undecryptable room frame");
            return;
        };
        if envelope.author == self.inner.me || self.inner.dedup.borrow().contains(&envelope.id) {
            return;
        }
        // Verify before recording the id, so a forged copy cannot shadow the real one.
        if !envelope.verify() {
            log::warn!("Dropping room message with an invalid signature");
            return;
        }
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        self.apply(&envelope);

        let targets = self
            .inner
            .roster
            .borrow()
            .forward_targets(&self.inner.me, &envelope.author, from);
        let links = self.inner.links.borrow();
        for target in targets {
            if let Some(link) = links.get(&target) {
                link.send(frame);
            }
        }
    }

    fn apply(&self, envelope: &RoomEnvelope) {
        let author = envelope.author.as_str();
        match &envelope.body {
            RoomBody::Hello { name, join_ts, admin_proof } => {
                let is_newer = self
                    .inner
                    .hellos
                    .borrow()
                    .get(author)
                    .map_or(true, |known| known.ts <= envelope.ts);
                if !is_newer {
                    return;
                }
                self.inner.hellos.borrow_mut().insert(author.to_string(), envelope.clone());
                let name = clean_remote_name(name, author);
                let is_admin = admin_proof
                    .as_deref()
                    .is_some_and(|proof| self.is_valid_admin_proof(author, proof));
                self.inner.names.borrow_mut().insert(author.to_string(), name.clone());
                self.inner.signals.names.update(|names| {
                    names.insert(author.to_string(), name.clone());
                });
                self.inner.roster.borrow_mut().upsert(Member {
                    pubkey: author.to_string(),
                    name,
                    join_ts: *join_ts,
                    is_admin,
                });
                self.recompute();
            }
            RoomBody::LinkState { seq, direct } => {
                let changed = self
                    .inner
                    .roster
                    .borrow_mut()
                    .set_links(author, *seq, direct.iter().cloned());
                if changed {
                    self.inner
                        .link_reports
                        .borrow_mut()
                        .insert(author.to_string(), envelope.clone());
                    self.recompute();
                }
            }
            RoomBody::Chat { text } => {
                self.push_message(ChatMessageUi {
                    id: envelope.id.clone(),
                    author: author.to_string(),
                    is_self: author == self.inner.me,
                    text: text.clone(),
                    time: current_time_string(),
                    notice: None,
                });
            }
            RoomBody::Leave => {
                if author != self.inner.me {
                    self.remove_member(author);
                }
            }
        }
    }

    fn is_valid_admin_proof(&self, author: &str, proof: &str) -> bool {
        self.inner.params.admin_pubkey.as_deref().is_some_and(|admin| {
            verify_message(admin, &admin_proof_message(&self.inner.room_id, author), proof)
        })
    }

    fn remove_member(&self, pubkey: &str) {
        if pubkey == self.inner.me {
            return;
        }
        self.inner.roster.borrow_mut().remove(pubkey);
        self.inner.hellos.borrow_mut().remove(pubkey);
        self.inner.link_reports.borrow_mut().remove(pubkey);
        let link = self.inner.links.borrow_mut().remove(pubkey);
        if let Some(link) = link {
            link.close();
        }
        self.publish_link_state();
        self.recompute();
    }

    // ---- Derived state ---------------------------------------------------------------

    /// Recompute who is in the room, enforce the member cap, emit join/leave notices and
    /// refresh the member list and status.
    fn recompute(&self) {
        if self.inner.closed.get() {
            return;
        }
        let me = self.inner.me.clone();
        let roster = self.inner.roster.borrow();
        let mut present = roster.reachable_from(&me);
        present.insert(me.clone());
        present.retain(|pk| roster.get(pk).is_some());

        let evicted = roster.evicted(&present, self.inner.params.member_cap);
        if evicted.contains(&me) {
            drop(roster);
            log::info!("Room is full: leaving");
            self.inner.signals.room_full.set(true);
            self.leave();
            return;
        }
        for pk in &evicted {
            present.remove(pk);
        }

        let previous = self.inner.present.replace(present.clone());
        let mut notices = Vec::new();
        for pk in present.difference(&previous) {
            if let Some(member) = roster.get(pk).filter(|_| *pk != me) {
                if member.join_ts > self.inner.join_ts {
                    notices.push(Notice::Joined(member.name.clone()));
                } else if !self.inner.late_join_noted.replace(true) {
                    notices.push(Notice::LateJoin);
                }
            }
        }
        let names = self.inner.names.borrow();
        for pk in previous.difference(&present) {
            if let Some(name) = names.get(pk).filter(|_| *pk != me) {
                notices.push(Notice::Left(name.clone()));
            }
        }
        drop(names);

        let links = self.inner.links.borrow();
        let mut members: Vec<&Member> = roster.members().filter(|m| present.contains(&m.pubkey)).collect();
        members.sort_by_key(|m| (m.pubkey != me, m.join_ts, m.pubkey.clone()));
        let members_ui = members
            .into_iter()
            .map(|m| {
                let link = if m.pubkey == me {
                    LinkUi::Me
                } else if links.get(&m.pubkey).is_some_and(|l| l.is_open()) {
                    LinkUi::Direct
                } else if let Some(relay) = roster.relay_for(&me, &m.pubkey) {
                    LinkUi::Via(roster.get(&relay).map(|r| r.name.clone()).unwrap_or_else(|| pubkey_tag(&relay)))
                } else {
                    LinkUi::Connecting
                };
                MemberUi {
                    pubkey: m.pubkey.clone(),
                    name: m.name.clone(),
                    tag: pubkey_tag(&m.pubkey),
                    is_admin: m.is_admin,
                    link,
                }
            })
            .collect();
        drop(links);
        drop(roster);

        self.inner.signals.members.set(members_ui);
        for notice in notices {
            self.push_message(ChatMessageUi {
                id: uuid::Uuid::new_v4().to_string(),
                author: String::new(),
                is_self: false,
                text: String::new(),
                time: current_time_string(),
                notice: Some(notice),
            });
        }
        self.refresh_status();
    }

    fn refresh_status(&self) {
        let links = self.inner.links.borrow();
        let status = if self.inner.closed.get() {
            ConnectionStatus::Disconnected
        } else if links.values().any(|l| l.is_open()) {
            ConnectionStatus::Connected
        } else if !links.is_empty() {
            ConnectionStatus::NegotiatingWebRtc
        } else if self.inner.relays_connected.get() > 0 {
            ConnectionStatus::WaitingForPeer
        } else {
            ConnectionStatus::ConnectingRelay
        };
        self.inner.signals.status.set(status);
    }

    // ---- Helpers ---------------------------------------------------------------------

    fn pool(&self) -> Option<Rc<NostrRelayPool>> {
        self.inner.pool.borrow().clone()
    }

    fn link(&self, remote: &str) -> Option<Rc<PeerLink>> {
        self.inner.links.borrow().get(remote).cloned()
    }

    fn has_open_link(&self) -> bool {
        self.inner.links.borrow().values().any(|l| l.is_open())
    }

    fn encode(&self, envelope: &RoomEnvelope) -> Option<String> {
        let encrypted = encrypt_json(&self.inner.key, envelope).ok()?;
        serde_json::to_string(&encrypted).ok()
    }

    fn push_message(&self, message: ChatMessageUi) {
        self.inner.signals.messages.update(|msgs| msgs.push(message));
    }

    /// Presence beacon (fast while joining, slow afterwards, so late or missed members
    /// still find us) and the watchdog for links that never open.
    fn start_ticker(&self) {
        let s = self.clone();
        let tick = Cell::new(0u32);
        let handle: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
        let handle_c = handle.clone();
        let cb = Closure::wrap(Box::new(move || {
            if s.inner.closed.get() {
                if let (Some(h), Some(w)) = (handle_c.take(), window()) {
                    w.clear_interval_with_handle(h);
                }
                return;
            }
            let now = js_sys::Date::now();
            let n = tick.get() + 1;
            tick.set(n);
            if now - s.inner.started_at < FAST_BEACON_FOR_MS || n % SLOW_BEACON_EVERY == 0 {
                if let Some(pool) = s.pool() {
                    pool.broadcast_signal(&SignalPayload::Presence);
                }
            }
            let stale: Vec<String> = s
                .inner
                .links
                .borrow()
                .values()
                .filter(|l| !l.is_open() && now - l.created_at > LINK_TIMEOUT_MS)
                .map(|l| l.remote.clone())
                .collect();
            for remote in stale {
                log::info!("Link to {} did not open in time", pubkey_tag(&remote));
                s.drop_link(&remote, true);
            }
        }) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            if let Ok(h) = w.set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), TICK_MS) {
                handle.set(Some(h));
            }
        }
        cb.forget();
    }

    fn leave_on_pagehide(&self) {
        let s = self.clone();
        let cb = Closure::wrap(Box::new(move || s.leave()) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            let _ = w.add_event_listener_with_callback("pagehide", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    }

    /// `window.__dchat` probes for Playwright: compiled only with the `e2e-hooks` feature.
    #[cfg(feature = "e2e-hooks")]
    fn install_e2e_hooks(&self) {
        use wasm_bindgen::JsValue;
        let Some(win) = window() else {
            return;
        };
        let hooks = js_sys::Object::new();

        let me = self.inner.me.clone();
        let self_pubkey = Closure::wrap(Box::new(move || JsValue::from_str(&me)) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(&hooks, &"selfPubkey".into(), self_pubkey.as_ref());
        self_pubkey.forget();

        // Refuse any direct link with `pubkey`, as if NAT traversal between us failed.
        let s = self.clone();
        let block_peer = Closure::wrap(Box::new(move |pubkey: String| {
            s.inner.blocked.borrow_mut().insert(pubkey.clone());
            s.drop_link(&pubkey, false);
        }) as Box<dyn Fn(String)>);
        let _ = js_sys::Reflect::set(&hooks, &"blockPeer".into(), block_peer.as_ref());
        block_peer.forget();

        let _ = js_sys::Reflect::set(&win, &"__dchat".into(), &hooks);
    }
}

/// Names come from other members: cap their length and strip control characters.
fn clean_remote_name(raw: &str, author: &str) -> String {
    let cleaned: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REMOTE_NAME_CHARS)
        .collect();
    if cleaned.is_empty() {
        pubkey_tag(author)
    } else {
        cleaned
    }
}

/// STUN by default; a room-supplied TURN server (`&turn=`, `&turnuser=`, `&turnpass=`)
/// is added for members whose NATs block direct connections.
fn build_rtc_config(params: &RoomParams) -> RtcConfiguration {
    let config = RtcConfiguration::new();
    let servers = js_sys::Array::new();

    let stun = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&stun, &"urls".into(), &DEFAULT_STUN.into());
    servers.push(&stun);

    if !params.turn_urls.is_empty() {
        let turn = js_sys::Object::new();
        let urls: js_sys::Array = params.turn_urls.iter().map(|u| wasm_bindgen::JsValue::from_str(&decode(u))).collect();
        let _ = js_sys::Reflect::set(&turn, &"urls".into(), &urls);
        if let Some(user) = &params.turn_user {
            let _ = js_sys::Reflect::set(&turn, &"username".into(), &decode(user).into());
        }
        if let Some(pass) = &params.turn_pass {
            let _ = js_sys::Reflect::set(&turn, &"credential".into(), &decode(pass).into());
        }
        servers.push(&turn);
    }

    config.set_ice_servers(&servers);
    config
}

/// Fragment values may be percent-encoded (e.g. TURN credentials with special characters).
fn decode(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| value.to_string())
}
