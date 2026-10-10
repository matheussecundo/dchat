//! A group room session: one `PeerLink` per member (full mesh), a signed gossip layer
//! for room messages, and the roster that drives the member list and caps.
//! Everything lives in RAM and is gone on reload.

mod admin;
mod control;
mod extras;
mod file_links;
mod files;
mod history;
mod lounge;
mod quality;
mod succession;
mod sync;

pub use files::{save_blob, save_finished, start_save};

use crate::mesh::{LinkEvent, LinkEventHandler, PeerLink, SignalOut};
use crate::names::pubkey_tag;
use crate::nostr_pool::{protocol_version, NostrRelayPool, PoolEvents};
use crate::state::{
    current_fragment, current_time_string, get_default_relays, ChatMessageUi, ConnectionStatus,
    DmUi, LinkUi, LoungeMemberUi, MemberUi, MyVoiceUi, Notice, RekeyTarget, SessionCarry,
};
use control::Control;
use file_links::FileLinks;
use files::Files;
use lounge::{Lounge, VoiceInfo};
use leptos::prelude::*;
use protocol::chat_log::ChatLog;
use protocol::presence::Absences;
use protocol::succession::Succession;
use protocol::crypto::{decrypt_json, encrypt_json};
use protocol::{
    admin_proof_message, hash_room_topic, password_room_topic, plan_ice, verify_message, EncryptedPayload, GossipDedup, IcePlan,
    Member, NostrBurnerKey, RoomBody, RoomEnvelope, RoomParams, Roster, SignalPayload, FALLBACK_STUN_URL, KEY_LENGTH,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{window, RtcConfiguration, RtcIceTransportPolicy};

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
/// After moving to a new room link, members re-join in a burst: no join notices for a while.
const MIGRATION_QUIET_MS: f64 = 5000.0;
/// The ticker missed this much: the tab was paused (a phone that switched apps, a frozen
/// background tab), so its links and relay connections may have died meanwhile.
const FROZEN_GAP_MS: f64 = 8000.0;
/// After a pause, a link that answers nothing for this long is dropped and dialed again.
const LINK_CHECK_MS: f64 = 3000.0;
/// An offer or ICE through the relays from a member whose link to us has been open this long
/// means they lost that link (it would go over the link otherwise): ours is stale.
const STALE_LINK_MS: f64 = 15_000.0;

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
    /// Members holding a voice seat, in seat order.
    pub lounge: WriteSignal<Vec<LoungeMemberUi>>,
    pub my_voice: WriteSignal<MyVoiceUi>,
    /// Pubkeys currently heard speaking.
    pub speaking: WriteSignal<HashSet<String>>,
    /// Name of a member who just joined voice while we are out of it.
    pub voice_prompt: WriteSignal<Option<String>>,
    /// i18n key of a transient notice.
    pub toast: WriteSignal<Option<&'static str>>,
    /// An admin removed this tab from the room.
    pub removed: WriteSignal<bool>,
    /// An admin moved the room: start a new session there.
    pub rekey: WriteSignal<Option<RekeyTarget>, LocalStorage>,
    /// Names of members currently typing.
    pub typing: WriteSignal<Vec<String>>,
    /// Count of messages that @-mentioned us (the UI resets it when seen).
    pub mention: WriteSignal<usize>,
    /// Private conversations by peer pubkey, and their unread counts.
    pub dms: WriteSignal<HashMap<String, Vec<DmUi>>>,
    pub dm_unread: WriteSignal<HashMap<String, usize>>,
    /// A member runs a newer protocol version: reload to update and connect with them.
    pub update_required: WriteSignal<bool>,
    /// The room hides IP addresses (TURN only) but no TURN server is available here.
    pub no_turn: WriteSignal<bool>,
    /// Remote control: offers, my rights, requests waiting for my answer.
    pub control: WriteSignal<crate::state::ControlUi>,
    /// The device choice, when the session changes it (a chosen device was missing, or 🔄
    /// moved to the next camera).
    pub devices: WriteSignal<crate::state::DeviceChoice>,
    /// Finished downloads waiting for a tap on Save (iOS), by file id. App level, so a
    /// rekey keeps them; the Save button takes them out.
    pub ready_files: StoredValue<HashMap<String, web_sys::File>, LocalStorage>,
    /// The room's chat log (`protocol::chat_log`). App level, so a rekey carries it into the
    /// new room; a fresh entry starts it empty.
    pub chat_log: StoredValue<Rc<RefCell<ChatLog>>, LocalStorage>,
    /// Earlier messages are being fetched from a member.
    pub history_loading: WriteSignal<bool>,
    /// Media loaded for viewing in the chat (`held_media`). App level, so a rekey keeps it.
    pub held_media: RwSignal<crate::held_media::HeldMedia>,
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
    /// Members this tab refuses direct links with (test hook only).
    blocked: RefCell<HashSet<String>>,
    relays_connected: Cell<usize>,
    last_presence_echo: Cell<f64>,
    closed: Cell<bool>,
    lounge: Lounge,
    files: Files,
    file_links: FileLinks,
    /// The room's chat log, shared with the next session after a rekey.
    log: Rc<RefCell<ChatLog>>,
    sync: sync::SyncState,
    /// The "earlier messages shown above" line is in the timeline.
    history_noted: Cell<bool>,
    /// The admin secret, while this session is an admin (from the link, or handed over).
    admin_key: RefCell<Option<Rc<NostrBurnerKey>>>,
    /// Admin succession: seniority, the heir we handed the secret to, our dormant copy.
    succession: RefCell<Succession>,
    heir_check_queued: Cell<bool>,
    rekeying: Cell<bool>,
    quiet_until: f64,
    last_typing_sent: Cell<f64>,
    typing: RefCell<HashMap<String, f64>>,
    dm_peers: RefCell<HashSet<String>>,
    /// Members already reported as running another protocol version.
    other_versions: RefCell<HashSet<String>>,
    control: Control,
    /// Members who dropped out of reach without leaving: away, then gone after the grace.
    absences: RefCell<Absences>,
    /// When the ticker last ran (or the tab last came back): a gap means we were paused.
    last_alive: Cell<f64>,
    /// Beacon every tick until then (joining, or back after a pause).
    fast_beacon_until: Cell<f64>,
    /// Links asked whether they are alive after a pause, and when they must have answered.
    link_checks: RefCell<HashMap<String, f64>>,
    /// DMs to members who are away, sealed and signed, by recipient: (id, frame).
    dm_outbox: RefCell<HashMap<String, Vec<(String, String)>>>,
    /// After we followed a rekey: hand it to members who were away, on the old topic.
    forwarding: RefCell<Option<admin::Forwarding>>,
    /// Test hook: the tab acts as if paused (no signals, no links, no ticks).
    #[cfg(feature = "e2e-hooks")]
    frozen: Cell<bool>,
}

impl RoomSession {
    /// Join `room_id`. `carry` comes from the previous session when an admin moved the room
    /// here from another link. `host_ice` are extra ICE servers the host offered (see `ice.rs`).
    pub fn start(
        room_id: String,
        key: [u8; KEY_LENGTH],
        name: String,
        signals: SessionSignals,
        carry: Option<SessionCarry>,
        host_ice: Option<js_sys::Array>,
    ) -> Result<Self, String> {
        let params = RoomParams::from_fragment(&current_fragment());
        let migrated = carry.is_some();
        let (identity, shared_files, first_seen) = match carry {
            Some(carry) => (carry.identity, carry.shared_files, carry.first_seen),
            None => (Rc::new(NostrBurnerKey::generate().map_err(|e| e.to_string())?), HashMap::new(), Default::default()),
        };
        let me = identity.pubkey().to_string();
        let started_at = js_sys::Date::now();
        let admin_key = params
            .admin_secret
            .as_deref()
            .and_then(|secret| NostrBurnerKey::from_secret_hex(secret).ok())
            .filter(|admin| params.admin_pubkey.as_deref() == Some(admin.pubkey()));
        let admin_proof = admin_key
            .as_ref()
            .and_then(|admin| admin.sign_message(&admin_proof_message(&room_id, &me)).ok());
        // After a move, members come back in a burst: hand the secret to an heir once they have.
        let hold_until = if migrated { started_at + MIGRATION_QUIET_MS } else { 0.0 };

        let (rtc_config, ice_plan) = build_rtc_config(&params, host_ice.as_ref());
        let topic = match params.password_salt {
            Some(_) => password_room_topic(&room_id, &key),
            None => hash_room_topic(&room_id),
        };
        signals.no_turn.set(ice_plan.relay_only && !ice_plan.has_turn);
        let session = Self {
            inner: Rc::new(Inner {
                key,
                rtc_config,
                params,
                room_id: room_id.clone(),
                identity: identity.clone(),
                me,
                join_ts: started_at as u64,
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
                blocked: RefCell::new(HashSet::new()),
                relays_connected: Cell::new(0),
                last_presence_echo: Cell::new(0.0),
                closed: Cell::new(false),
                lounge: Lounge::default(),
                files: Files::default(),
                file_links: FileLinks::default(),
                log: signals.chat_log.get_value(),
                sync: sync::SyncState::default(),
                history_noted: Cell::new(migrated),
                admin_key: RefCell::new(admin_key.map(Rc::new)),
                succession: RefCell::new(Succession::new(first_seen, hold_until as u64)),
                heir_check_queued: Cell::new(false),
                rekeying: Cell::new(false),
                quiet_until: if migrated { started_at + MIGRATION_QUIET_MS } else { 0.0 },
                last_typing_sent: Cell::new(0.0),
                typing: RefCell::new(HashMap::new()),
                dm_peers: RefCell::new(HashSet::new()),
                other_versions: RefCell::new(HashSet::new()),
                control: Control::default(),
                absences: RefCell::new(Absences::default()),
                last_alive: Cell::new(started_at),
                fast_beacon_until: Cell::new(started_at + FAST_BEACON_FOR_MS),
                link_checks: RefCell::new(HashMap::new()),
                dm_outbox: RefCell::new(HashMap::new()),
                forwarding: RefCell::new(None),
                #[cfg(feature = "e2e-hooks")]
                frozen: Cell::new(false),
            }),
        };
        if migrated {
            session.push_notice(Notice::Rekeyed);
            session.restore_files(shared_files);
        }

        session.publish(RoomBody::Hello {
            name,
            join_ts: session.inner.join_ts,
            admin_proof,
        });
        session.publish_link_state();

        let events = PoolEvents {
            on_signal: {
                let s = session.clone();
                Box::new(move |from, signal| s.on_relay_signal(from, signal))
            },
            on_other_version: {
                let s = session.clone();
                Box::new(move |from, version| s.on_other_version(&from, version))
            },
            on_relay_connected: {
                let s = session.clone();
                Box::new(move |count| {
                    // A pool kept open after a rekey (`forward_rekey`) must not touch the UI,
                    // which shows the new session.
                    if s.inner.closed.get() {
                        return;
                    }
                    s.inner.relays_connected.set(count);
                    s.inner.signals.connected_relays.set(count);
                    s.refresh_status();
                })
            },
        };
        let pool = NostrRelayPool::new(topic, key, identity, get_default_relays(), events)?;
        *session.inner.pool.borrow_mut() = Some(pool);

        session.start_ticker();
        session.start_typing_ticker();
        session.watch_lifecycle();
        session.recompute();
        #[cfg(feature = "e2e-hooks")]
        session.install_e2e_hooks();
        Ok(session)
    }

    /// Post a chat message. Alone in the room, it waits in the chat log: whoever joins gets
    /// it with the history.
    pub fn send_chat(&self, text: &str) -> Result<(), String> {
        self.publish(RoomBody::Chat { text: text.to_string() })
            .map(|_| ())
            .ok_or_else(|| "Failed to sign message".into())
    }

    /// Announce departure and tear down every link. Idempotent.
    pub fn leave(&self) {
        if self.inner.closed.get() {
            return;
        }
        self.end_control();
        self.close_lounge();
        if let Some(pool) = self.pool() {
            pool.broadcast_signal(&SignalPayload::PeerLeft);
        }
        self.publish(RoomBody::Leave);
        self.inner.closed.set(true);
        self.inner.succession.borrow_mut().clear_dormant();
        self.inner.admin_key.borrow_mut().take();
        self.close_all_file_links();
        for (_, link) in self.inner.links.borrow_mut().drain() {
            link.close();
        }
        // Queued messages (the PeerLeft above) are still sent before the sockets close. After
        // a rekey, the relays stay a while for members who were away (`forward_rekey`).
        if let Some(pool) = self.pool() {
            self.close_pool_after_forwarding(pool);
        }
        self.refresh_status();
    }

    // ---- Signaling -------------------------------------------------------------------

    /// A signal through the relays. An offer or ICE from a member whose link to us has been
    /// open a while means they lost that link (a phone that slept): ours is stale, start over.
    fn on_relay_signal(&self, from: String, signal: SignalPayload) {
        if self.frozen() {
            return;
        }
        if self.inner.closed.get() {
            if signal == SignalPayload::Presence {
                self.forward_rekey(&from);
            }
            return;
        }
        match &signal {
            SignalPayload::RekeyForward { envelope, .. } => {
                self.on_rekey_forward(envelope);
                return;
            }
            SignalPayload::Offer { .. } | SignalPayload::IceBatch { .. } => {
                if self.link(&from).is_some_and(|l| l.open_for() > STALE_LINK_MS) {
                    log::info!("{} lost its link to us: connecting again", pubkey_tag(&from));
                    self.drop_link(&from, false);
                }
            }
            _ => {}
        }
        self.on_signal(from, signal);
    }

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
            // Relays only (`on_relay_signal`).
            SignalPayload::RekeyForward { .. } => {}
        }
    }

    /// A member on another protocol version: we never link. The older side is asked to
    /// reload (which loads the newest build); the newer side is told once per member.
    fn on_other_version(&self, from: &str, version: u32) {
        if self.inner.closed.get() || !self.inner.other_versions.borrow_mut().insert(from.to_string()) {
            return;
        }
        let own = protocol_version();
        if version > own {
            log::warn!("A member runs protocol version {version}, newer than ours ({own}): reload to update");
            self.inner.signals.update_required.set(true);
        } else {
            log::warn!("A member runs older protocol version {version} (ours: {own}); it must reload to connect");
            self.toast("toast_peer_outdated");
        }
    }

    /// A member announced itself. The lower pubkey of each pair dials; the higher one
    /// answers with its own presence so the lower one learns about it.
    fn on_presence(&self, from: &str) {
        if self.inner.links.borrow().contains_key(from) {
            return;
        }
        let now = js_sys::Date::now();
        // The backoff spares a member we still reach through others while our direct link to
        // them keeps failing. One out of reach (away, gone, or we were paused) is dialed at once.
        let reachable = self.inner.present.borrow().contains(from);
        if reachable && self.inner.retry_after.borrow().get(from).is_some_and(|t| *t > now) {
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
        // No links before the session joined the relays, or after it left.
        self.pool()?;
        let handler: LinkEventHandler = {
            let s = self.clone();
            Rc::new(move |remote, id, event| s.on_link_event(remote, id, event))
        };
        let signal_out: SignalOut = {
            let s = self.clone();
            Rc::new(move |remote, signal| s.send_link_signal(remote, signal))
        };
        match PeerLink::new(&self.inner.me, remote, &self.inner.rtc_config, signal_out, handler, initiator) {
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

    /// Send a link's offer, answer or ICE to `remote`: over the link itself once it is open,
    /// otherwise (the first handshake) through the relays.
    fn send_link_signal(&self, remote: &str, signal: SignalPayload) {
        if self.inner.closed.get() {
            return;
        }
        let in_band = self.link(remote).is_some_and(|link| link.is_open());
        if in_band
            && self.send_direct(
                remote,
                RoomBody::LinkSignal {
                    to: remote.to_string(),
                    signal: signal.clone(),
                },
            )
        {
            return;
        }
        if let Some(pool) = self.pool() {
            pool.broadcast_signal(&signal);
        }
    }

    fn on_link_event(&self, remote: &str, id: u64, event: LinkEvent) {
        let link = self.inner.links.borrow().get(remote).filter(|l| l.id == id).cloned();
        let Some(link) = link else {
            return;
        };
        if self.inner.closed.get() || self.frozen() {
            return;
        }
        match event {
            LinkEvent::Open => {
                log::info!("Direct link open with {}", pubkey_tag(remote));
                link.mark_opened();
                self.inner.retry_after.borrow_mut().remove(remote);
                self.inner.link_checks.borrow_mut().remove(remote);
                self.publish_link_state();
                self.sync_to(remote);
                self.sync_link_opened(remote);
                self.recompute();
                // Back from away (either side): stopped downloads go on, waiting DMs go out.
                self.resume_downloads(remote);
                self.flush_dm_outbox(remote);
            }
            LinkEvent::Closed => self.drop_link(remote, true),
            LinkEvent::Message(text) => self.on_frame(remote, &text),
            // Our own stream per member is used, never the browser's (see `on_remote_track`).
            LinkEvent::Track(track, _stream, receiver) => self.on_remote_track(remote, track, receiver),
            // The codecs this link negotiated are known now: pick ours among them.
            LinkEvent::Negotiated => self.apply_video_params(remote),
            LinkEvent::Chunk(packet) => self.on_file_chunk(remote, None, &packet),
            LinkEvent::Input { lane, packet } => self.on_input_packet(remote, lane, &packet),
        }
    }

    fn drop_link(&self, remote: &str, failed: bool) {
        let Some(link) = self.inner.links.borrow_mut().remove(remote) else {
            return;
        };
        link.close();
        self.forget_member_media(remote, false);
        self.on_file_peer_lost(remote);
        self.on_control_peer_lost(remote);
        self.sync_peer_lost(remote);
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
        let envelope = RoomEnvelope::sign(&self.inner.identity, self.adm(), js_sys::Date::now() as u64, body).ok()?;
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
        let voices = self.inner.lounge.envelopes.borrow();
        let controls = self.inner.control.envelopes.borrow();
        for envelope in hellos.values().chain(reports.values()).chain(voices.values()).chain(controls.values()) {
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
        // Anything from them answers a link check.
        self.inner.link_checks.borrow_mut().remove(from);
        if envelope.author == self.inner.me || self.inner.dedup.borrow().contains(&envelope.id) {
            return;
        }
        // Verify before recording the id, so a forged copy cannot shadow the real one.
        if !envelope.verify(self.adm()) {
            log::warn!("Dropping room message with an invalid signature");
            return;
        }
        self.inner.dedup.borrow_mut().insert(&envelope.id);

        // Direct-only messages (file requests and the like) must come straight from
        // their author over this link, are for us alone and are never relayed.
        if let Some(to) = envelope.body.recipient() {
            if to == self.inner.me && envelope.author == from {
                self.apply(&envelope);
            }
            return;
        }
        // A DM names no recipient: if it opens for us it is ours and goes no further;
        // otherwise we relay it unread.
        if matches!(envelope.body, RoomBody::Dm { .. }) {
            if self.receive_dm(&envelope) {
                return;
            }
        } else {
            self.apply(&envelope);
        }

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
                // Kept in the log too, to name the author in history after they leave.
                self.log_merge(envelope);
                self.inner.succession.borrow_mut().seen.saw(author, js_sys::Date::now() as u64);
                let name = clean_remote_name(name, author);
                let is_admin = admin_proof
                    .as_deref()
                    .is_some_and(|proof| self.is_valid_admin_proof(author, proof));
                self.remember_name(author, &name);
                // A newer Hello may carry a proof the first one lacked: the member became an admin.
                let was_admin = self.inner.roster.borrow().get(author).map(|m| m.is_admin);
                self.inner.roster.borrow_mut().upsert(Member {
                    pubkey: author.to_string(),
                    name: name.clone(),
                    join_ts: *join_ts,
                    is_admin,
                });
                if was_admin == Some(false) && is_admin {
                    self.push_notice(Notice::NowAdmin(name));
                }
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
            RoomBody::Chat { .. } | RoomBody::FileOffer { .. } => self.on_logged(envelope),
            RoomBody::Typing
            | RoomBody::Reaction { .. }
            | RoomBody::Edit { .. }
            | RoomBody::Delete { .. }
            | RoomBody::Dm { .. } => self.on_chat_extra(envelope),
            RoomBody::VoiceState { seq, in_voice, voice_ts, mic_muted, video, video_ts } => {
                let info = VoiceInfo {
                    seq: *seq,
                    in_voice: *in_voice,
                    voice_ts: *voice_ts,
                    mic_muted: *mic_muted,
                    video: *video,
                    video_ts: *video_ts,
                };
                self.on_voice_state(envelope, info);
            }
            RoomBody::SyncSummary { horizon, windows, .. } => self.on_sync_summary(author, *horizon, windows),
            RoomBody::SyncAsk { horizon, level, starts, .. } => self.on_sync_ask(author, *horizon, *level, starts),
            RoomBody::SyncDiff { ids, level, windows, last, .. } => self.on_sync_diff(author, ids, *level, windows, *last),
            RoomBody::SyncWant { ids, .. } => self.on_sync_want(author, ids),
            RoomBody::SyncBatch { envelopes, last, .. } => self.on_sync_batch(author, envelopes, *last),
            RoomBody::AdminRekey { kicked, grants } => self.on_admin_rekey(envelope, kicked.as_deref(), grants),
            RoomBody::AdminHandover { promote, sealed } => self.on_admin_handover(envelope, *promote, sealed),
            RoomBody::FileCancel { to: None, .. } => {
                // A withdrawal is part of the card's history.
                self.log_merge(envelope);
                self.on_file_message(author, &envelope.body);
            }
            RoomBody::FileRequest { .. } | RoomBody::FileQueued { .. } | RoomBody::FileCancel { .. } => {
                self.on_file_message(author, &envelope.body);
            }
            RoomBody::FileLinkSignal { link, from_dialer, signal, .. } => {
                self.on_file_link_signal(author, *link, *from_dialer, signal);
            }
            RoomBody::LinkSignal { signal, .. } => {
                // Only offers, answers and ICE addressed to us (presence, departures and
                // forwarded rekeys stay on the relays).
                let handshake =
                    matches!(signal, SignalPayload::Offer { .. } | SignalPayload::Answer { .. } | SignalPayload::IceBatch { .. });
                if handshake && signal.recipient() == Some(self.inner.me.as_str()) {
                    self.on_signal(author.to_string(), signal.clone());
                }
            }
            RoomBody::LinkCheck { reply, .. } => {
                if !reply {
                    self.send_direct(author, RoomBody::LinkCheck { to: author.to_string(), reply: true });
                }
            }
            RoomBody::ControlStatus { .. } => self.on_control_status(envelope),
            RoomBody::ControlRequest { mouse_keyboard, controller, .. } => {
                let wants = protocol::ControlWants { mouse_keyboard: *mouse_keyboard, controller: *controller };
                self.on_control_request(author, wants);
            }
            RoomBody::ControlGrant { mouse_keyboard, pad, reason, .. } => {
                self.on_control_grant(author, *mouse_keyboard, *pad, *reason);
            }
            RoomBody::ControlRelease { .. } => self.on_control_release(author),
            RoomBody::Leave => {
                if author != self.inner.me {
                    self.remove_member(author);
                }
            }
        }
    }

    /// Sign `body` and send it over the direct link to `to` only (no local apply, no relay).
    fn send_direct(&self, to: &str, body: RoomBody) -> bool {
        let Some(link) = self.link(to) else {
            return false;
        };
        let Ok(envelope) = RoomEnvelope::sign(&self.inner.identity, self.adm(), js_sys::Date::now() as u64, body) else {
            return false;
        };
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        self.encode(&envelope).is_some_and(|frame| link.send(&frame))
    }

    /// The room's admin pubkey: every room message is signed for it (`""` without one).
    fn adm(&self) -> &str {
        self.inner.params.admin_pubkey.as_deref().unwrap_or("")
    }

    /// The room connects only through TURN, hiding members' IP addresses from each other.
    pub fn hides_ip(&self) -> bool {
        self.inner.params.hide_ip
    }

    /// Joining the room needs a password as well as the link.
    pub fn has_password(&self) -> bool {
        self.inner.params.password_salt.is_some()
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
        // Away until now: their "left" line wasn't shown, and what waited for them goes.
        if self.inner.absences.borrow_mut().forget(pubkey) {
            if let Some(name) = self.inner.names.borrow().get(pubkey).cloned() {
                self.push_notice(Notice::Left(name));
            }
            self.member_gone(pubkey);
        }
        self.inner.roster.borrow_mut().remove(pubkey);
        self.inner.hellos.borrow_mut().remove(pubkey);
        self.inner.link_reports.borrow_mut().remove(pubkey);
        let link = self.inner.links.borrow_mut().remove(pubkey);
        if let Some(link) = link {
            link.close();
        }
        self.forget_member_media(pubkey, true);
        self.on_file_peer_lost(pubkey);
        self.on_control_peer_lost(pubkey);
        self.sync_peer_lost(pubkey);
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
        // Members who dropped out of reach without leaving are away; members no longer in the
        // roster left (`Leave`, `PeerLeft`).
        let still_listed: HashSet<String> = previous.iter().filter(|pk| roster.get(pk).is_some()).cloned().collect();
        let changes = self.inner.absences.borrow_mut().update(&still_listed, &present, js_sys::Date::now() as u64);
        let away = self.inner.absences.borrow().away();
        let mut notices = Vec::new();
        let quiet = js_sys::Date::now() < self.inner.quiet_until;
        for pk in present.difference(&previous) {
            // Back from away: they never left. Back after their "left" line: joined again.
            let returned = changes.returned.contains(pk);
            if let Some(member) = roster.get(pk).filter(|m| *pk != me && !quiet && (returned || m.join_ts > self.inner.join_ts)) {
                if !changes.back.contains(pk) {
                    notices.push(Notice::Joined(member.name.clone()));
                }
            }
        }
        let names = self.inner.names.borrow();
        let mut departed: Vec<String> = previous.iter().filter(|pk| **pk != me && !still_listed.contains(*pk)).cloned().collect();
        departed.extend(changes.expired.iter().cloned());
        for pk in &departed {
            if let Some(name) = names.get(pk) {
                notices.push(Notice::Left(name.clone()));
            }
        }
        drop(names);

        let links = self.inner.links.borrow();
        let mut members: Vec<&Member> =
            roster.members().filter(|m| present.contains(&m.pubkey) || away.contains(&m.pubkey)).collect();
        members.sort_by_key(|m| (m.pubkey != me, m.join_ts, m.pubkey.clone()));
        let members_ui = members
            .into_iter()
            .map(|m| {
                let link = if m.pubkey == me {
                    LinkUi::Me
                } else if away.contains(&m.pubkey) {
                    LinkUi::Away
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
            self.push_notice(notice);
        }
        for pk in &changes.away {
            self.stop_typing(pk);
        }
        for pk in changes.back.iter().chain(&changes.returned) {
            // Reached through someone else again: dial them at once, not after the backoff.
            self.inner.retry_after.borrow_mut().remove(pk);
            self.on_file_author_back(pk);
            self.flush_dm_outbox(pk);
        }
        for pk in &departed {
            self.member_gone(pk);
        }
        self.refresh_status();
        self.recompute_lounge();
        self.schedule_heir_check();
    }

    /// `pubkey` left, or stayed away for the whole grace: what waited for them ends.
    fn member_gone(&self, pubkey: &str) {
        self.on_file_author_gone(pubkey);
        self.stop_typing(pubkey);
        self.drop_dm_outbox(pubkey);
        self.end_dm_thread(pubkey);
    }

    /// Members away now (out of reach, within their grace), as the roster holds them.
    fn away_members(&self) -> Vec<Member> {
        let absences = self.inner.absences.borrow();
        self.inner.roster.borrow().members().filter(|m| absences.is_away(&m.pubkey)).cloned().collect()
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

    /// `getStats()` of the link to `remote` (for the stats panel), if there is one.
    pub fn peer_stats(&self, remote: &str) -> Option<js_sys::Promise> {
        self.link(remote).map(|link| link.stats())
    }

    /// Members our video currently goes to (their video sender carries a track), for the
    /// stats panel's rows.
    pub fn video_viewers(&self) -> Vec<String> {
        let mut viewers: Vec<String> = self
            .inner
            .lounge
            .senders
            .borrow()
            .iter()
            .filter(|(_, l)| l.video.as_ref().is_some_and(|v| v.track().is_some()))
            .map(|(remote, _)| remote.clone())
            .collect();
        viewers.sort();
        viewers
    }

    fn has_open_link(&self) -> bool {
        self.inner.links.borrow().values().any(|l| l.is_open())
    }

    fn encode(&self, envelope: &RoomEnvelope) -> Option<String> {
        let encrypted = encrypt_json(&self.inner.key, envelope).ok()?;
        serde_json::to_string(&encrypted).ok()
    }

    fn toast(&self, key: &'static str) {
        self.inner.signals.toast.set(Some(key));
    }

    fn remember_name(&self, pubkey: &str, name: &str) {
        self.inner.names.borrow_mut().insert(pubkey.to_string(), name.to_string());
        self.inner.signals.names.update(|names| {
            names.insert(pubkey.to_string(), name.to_string());
        });
    }

    fn push_notice(&self, notice: Notice) {
        self.push_message(ChatMessageUi {
            id: uuid::Uuid::new_v4().to_string(),
            author: String::new(),
            is_self: false,
            text: String::new(),
            time: current_time_string(),
            ts: js_sys::Date::now() as u64,
            notice: Some(notice),
            ..Default::default()
        });
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
            if s.frozen() {
                return;
            }
            let now = js_sys::Date::now();
            s.wake_check(now);
            let n = tick.get() + 1;
            tick.set(n);
            if now < s.inner.fast_beacon_until.get() || n % SLOW_BEACON_EVERY == 0 {
                if let Some(pool) = s.pool() {
                    pool.broadcast_signal(&SignalPayload::Presence);
                }
            }
            if s.inner.absences.borrow().next_expiry().is_some_and(|at| at as f64 <= now) {
                s.recompute();
            }
            let silent: Vec<String> =
                s.inner.link_checks.borrow().iter().filter(|(_, until)| **until <= now).map(|(pk, _)| pk.clone()).collect();
            for remote in silent {
                log::info!("Link to {} did not answer after a pause: connecting again", pubkey_tag(&remote));
                s.inner.link_checks.borrow_mut().remove(&remote);
                s.drop_link(&remote, false);
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
            s.control_tick();
            s.sync_tick();
            s.succession_tick();
        }) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            if let Ok(h) = w.set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), TICK_MS) {
                handle.set(Some(h));
            }
        }
        cb.forget();
    }

    /// Leaving the page leaves the room. Coming back to it (the page shown again, the
    /// network back) checks whether we were paused meanwhile (`wake_check`).
    fn watch_lifecycle(&self) {
        let Some(w) = window() else {
            return;
        };
        let s = self.clone();
        let leave = Closure::wrap(Box::new(move || s.leave()) as Box<dyn FnMut()>);
        let _ = w.add_event_listener_with_callback("pagehide", leave.as_ref().unchecked_ref());
        leave.forget();
        let s = self.clone();
        let shown = Closure::wrap(Box::new(move || {
            let visible = window().and_then(|w| w.document()).is_some_and(|d| !d.hidden());
            if visible && !s.inner.closed.get() && !s.frozen() {
                s.wake_check(js_sys::Date::now());
            }
        }) as Box<dyn FnMut()>);
        if let Some(doc) = w.document() {
            let _ = doc.add_event_listener_with_callback("visibilitychange", shown.as_ref().unchecked_ref());
        }
        shown.forget();
        let s = self.clone();
        let online = Closure::wrap(Box::new(move || {
            if !s.inner.closed.get() && !s.frozen() {
                s.on_wake();
            }
        }) as Box<dyn FnMut()>);
        let _ = w.add_event_listener_with_callback("online", online.as_ref().unchecked_ref());
        online.forget();
    }

    /// The ticker (or the page shown again) runs at `now`: if it missed a lot, the tab was
    /// paused and its connections may be dead.
    fn wake_check(&self, now: f64) {
        let gap = now - self.inner.last_alive.replace(now);
        if gap > FROZEN_GAP_MS {
            log::info!("Back after {:.0} s paused", gap / 1000.0);
            self.on_wake();
        }
    }

    /// Back from a pause, or the network came back: reconnect the relays, ask every link
    /// whether it is still alive (silent ones are dialed again), and announce ourselves.
    fn on_wake(&self) {
        if self.inner.closed.get() {
            return;
        }
        let now = js_sys::Date::now();
        self.inner.last_alive.set(now);
        self.inner.fast_beacon_until.set(now + FAST_BEACON_FOR_MS);
        self.inner.retry_after.borrow_mut().clear();
        if let Some(pool) = self.pool() {
            pool.wake();
            pool.broadcast_signal(&SignalPayload::Presence);
        }
        let links: Vec<Rc<PeerLink>> = self.inner.links.borrow().values().cloned().collect();
        for link in links {
            let remote = link.remote.clone();
            let asked = link.is_open() && self.send_direct(&remote, RoomBody::LinkCheck { to: remote.clone(), reply: false });
            if asked {
                self.inner.link_checks.borrow_mut().insert(remote, now + LINK_CHECK_MS);
            } else {
                self.drop_link(&remote, false);
            }
        }
        self.resume_voice();
        self.recompute();
    }

    /// Whether the tab acts as paused (test hook only).
    fn frozen(&self) -> bool {
        #[cfg(feature = "e2e-hooks")]
        {
            self.inner.frozen.get()
        }
        #[cfg(not(feature = "e2e-hooks"))]
        {
            false
        }
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

        // Allow links with `pubkey` again (after `blockPeer`) and announce ourselves.
        let s = self.clone();
        let unblock_peer = Closure::wrap(Box::new(move |pubkey: String| {
            s.inner.blocked.borrow_mut().remove(&pubkey);
            s.inner.retry_after.borrow_mut().remove(&pubkey);
            if let Some(pool) = s.pool() {
                pool.broadcast_signal(&SignalPayload::Presence);
            }
        }) as Box<dyn Fn(String)>);
        let _ = js_sys::Reflect::set(&hooks, &"unblockPeer".into(), unblock_peer.as_ref());
        unblock_peer.forget();
        self.install_sync_hooks(&hooks);

        // Act as a phone that switched apps: links die without a word (the members drop them
        // as if they timed out), signals and ticks are ignored, nothing is sent.
        let s = self.clone();
        let freeze = Closure::wrap(Box::new(move || {
            s.inner.frozen.set(true);
            for link in s.inner.links.borrow().values() {
                link.close();
            }
        }) as Box<dyn Fn()>);
        let _ = js_sys::Reflect::set(&hooks, &"simulateFreeze".into(), freeze.as_ref());
        freeze.forget();

        // Come back from `simulateFreeze` as the page shown again would: the ticker missed
        // `paused_ms`, so the tab finds its links dead and reconnects.
        let s = self.clone();
        let resume = Closure::wrap(Box::new(move |paused_ms: f64| {
            s.inner.frozen.set(false);
            let now = js_sys::Date::now();
            s.inner.last_alive.set(now - paused_ms);
            s.wake_check(now);
        }) as Box<dyn Fn(f64)>);
        let _ = js_sys::Reflect::set(&hooks, &"simulateResume".into(), resume.as_ref());
        resume.forget();

        // Shorten how long members stay away before counting as gone.
        let s = self.clone();
        let grace = Closure::wrap(Box::new(move |ms: f64| {
            s.inner.absences.borrow_mut().set_grace(ms as u64);
            s.recompute();
        }) as Box<dyn Fn(f64)>);
        let _ = js_sys::Reflect::set(&hooks, &"awayGraceMs".into(), grace.as_ref());
        grace.forget();

        // Members this tab lists as away.
        let s = self.clone();
        let away = Closure::wrap(Box::new(move || {
            let mut away: Vec<String> = s.inner.absences.borrow().away().into_iter().collect();
            away.sort();
            JsValue::from(away.into_iter().map(JsValue::from).collect::<js_sys::Array>())
        }) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(&hooks, &"awayMembers".into(), away.as_ref());
        away.forget();

        // "active", "dormant" (heir) or "none": what this session holds of the admin secret.
        let s = self.clone();
        let admin_state = Closure::wrap(Box::new(move || {
            let state = if s.is_admin() {
                "active"
            } else if s.inner.succession.borrow().has_dormant() {
                "dormant"
            } else {
                "none"
            };
            JsValue::from_str(state)
        }) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(&hooks, &"adminKeyState".into(), admin_state.as_ref());
        admin_state.forget();

        // Pause between uploaded chunks, so upload queues and interruptions can be observed.
        let s = self.clone();
        let throttle = Closure::wrap(Box::new(move |ms: i32| {
            s.inner.files.chunk_delay_ms.set(ms);
        }) as Box<dyn Fn(i32)>);
        let _ = js_sys::Reflect::set(&hooks, &"throttleUploads".into(), throttle.as_ref());
        throttle.forget();

        // How many files this tab asked members for (Download of a viewed file asks for none).
        let s = self.clone();
        let requests = Closure::wrap(Box::new(move || s.inner.files.requests_sent.get()) as Box<dyn Fn() -> u32>);
        let _ = js_sys::Reflect::set(&hooks, &"fileRequestsSent".into(), requests.as_ref());
        requests.forget();
        self.install_file_link_hooks(&hooks);

        // Send remote-control input (JSON `InputEvent`s) to a sharer, bypassing our own rights.
        let s = self.clone();
        let raw_input = Closure::wrap(Box::new(move |sharer: String, events: String| {
            if let Ok(events) = serde_json::from_str::<Vec<protocol::InputEvent>>(&events) {
                s.send_raw_input(&sharer, events);
            }
        }) as Box<dyn Fn(String, String)>);
        let _ = js_sys::Reflect::set(&hooks, &"sendRawInput".into(), raw_input.as_ref());
        raw_input.forget();

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

/// STUN by default, plus TURN for members whose NATs block direct connections: servers the
/// host offers (`host_ice`) and/or one from the room link (`&turn=`, `&turnuser=`, `&turnpass=`).
/// ICE settings for every link (see `protocol::plan_ice`): the room's STUN and TURN, then
/// the host's; Google's STUN only when nothing else offers STUN; TURN only with `&hideip=1`.
fn build_rtc_config(params: &RoomParams, host_ice: Option<&js_sys::Array>) -> (RtcConfiguration, IcePlan) {
    let config = RtcConfiguration::new();
    let servers = js_sys::Array::new();
    let server = |urls: &[String]| {
        let entry = js_sys::Object::new();
        let urls: js_sys::Array = urls.iter().map(|u| wasm_bindgen::JsValue::from_str(u)).collect();
        let _ = js_sys::Reflect::set(&entry, &"urls".into(), &urls);
        entry
    };

    if !params.stun_urls.is_empty() {
        servers.push(&server(&params.stun_urls));
    }
    if !params.turn_urls.is_empty() {
        let urls: Vec<String> = params.turn_urls.iter().map(|u| decode(u)).collect();
        let turn = server(&urls);
        if let Some(user) = &params.turn_user {
            let _ = js_sys::Reflect::set(&turn, &"username".into(), &decode(user).into());
        }
        if let Some(pass) = &params.turn_pass {
            let _ = js_sys::Reflect::set(&turn, &"credential".into(), &decode(pass).into());
        }
        servers.push(&turn);
    }
    let mut host_urls = Vec::new();
    for entry in host_ice.into_iter().flat_map(|list| list.iter()) {
        if let Ok(urls) = js_sys::Reflect::get(&entry, &"urls".into()) {
            match urls.dyn_ref::<js_sys::Array>() {
                Some(list) => host_urls.extend(list.iter().filter_map(|u| u.as_string())),
                None => host_urls.extend(urls.as_string()),
            }
        }
        servers.push(&entry);
    }

    let plan = plan_ice(params, &host_urls);
    if plan.fallback_stun {
        servers.push(&server(&[FALLBACK_STUN_URL.to_string()]));
    }
    if plan.relay_only {
        config.set_ice_transport_policy(RtcIceTransportPolicy::Relay);
    }
    config.set_ice_servers(&servers);
    (config, plan)
}

/// Fragment values may be percent-encoded (e.g. TURN credentials with special characters).
fn decode(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| value.to_string())
}
