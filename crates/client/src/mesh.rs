//! One WebRTC link per remote room member, using the "perfect negotiation" pattern
//! so either side can (re)negotiate at any time without signaling glare.

use protocol::{IceCandidateData, InputLane, SignalPayload, INPUT_EVENTS_LABEL, INPUT_STATE_LABEL};
use futures::channel::oneshot;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    window, MediaStream, MediaStreamTrack, MessageEvent, RtcConfiguration, RtcDataChannel,
    RtcDataChannelEvent, RtcDataChannelInit, RtcDataChannelState, RtcDataChannelType, RtcIceCandidate,
    RtcIceCandidateInit, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcPeerConnectionState,
    RtcRtpSender, RtcSdpType, RtcSessionDescriptionInit, RtcSignalingState, RtcTrackEvent,
};

const CHAT_LABEL: &str = "chat";
const FILE_LABEL: &str = "file-transfer";
pub const FILE_BUFFER_LOW_THRESHOLD: u32 = 512 * 1024;
/// History sync waiting for room on the chat channel wakes when its queue drops below this.
const CHAT_BUFFER_LOW_THRESHOLD: u32 = 64 * 1024;
const ICE_BATCH_DELAY_MS: i32 = 100;
/// Pointer and controller states are dropped rather than queued behind this much data.
const INPUT_STATE_MAX_BUFFERED: u32 = 8 * 1024;
const ICE_BATCH_MAX: usize = 10;
/// A link stuck in "disconnected" this long counts as lost (a killed tab only reaches
/// "failed" after the ~30 s ICE consent timeout); a real member reconnects via presence.
const DISCONNECT_GRACE_MS: i32 = 10_000;

pub enum LinkEvent {
    /// The chat channel opened: the member is directly reachable.
    Open,
    /// The link failed or closed; it will not recover and should be dropped.
    Closed,
    /// A text frame arrived on the chat channel.
    Message(String),
    /// The member started sending a media track (with its stream, when announced, and the
    /// `RTCRtpReceiver` that receives it).
    Track(MediaStreamTrack, Option<MediaStream>, JsValue),
    /// An offer/answer exchange completed (an answer was applied): the negotiated codecs are
    /// known now, so per-sender parameters can pick one. Never fired on a rollback.
    Negotiated,
    /// A binary packet arrived on the file-transfer channel.
    Chunk(Vec<u8>),
    /// A remote-control input packet arrived on one of the input channels.
    Input { lane: InputLane, packet: Vec<u8> },
}

/// Receives `(remote pubkey, link id, event)`. The id tells a replaced link's late
/// events apart from the current link's.
pub type LinkEventHandler = Rc<dyn Fn(&str, u64, LinkEvent)>;

/// Delivers this link's signaling (offer, answer, ICE) to `remote`: over the link itself
/// once it is open, otherwise through the Nostr relays.
pub type SignalOut = Rc<dyn Fn(&str, SignalPayload)>;

type DrainNotify = Rc<RefCell<Vec<oneshot::Sender<()>>>>;

pub struct PeerLink {
    pub remote: String,
    pub id: u64,
    pub created_at: f64,
    /// When the chat channel opened (0 until then), set by the session on `LinkEvent::Open`.
    opened_at: Cell<f64>,
    pc: RtcPeerConnection,
    /// The polite side yields when both offer at once (higher pubkey is polite).
    polite: bool,
    making_offer: Rc<Cell<bool>>,
    ignore_offer: Cell<bool>,
    chat: Rc<RefCell<Option<RtcDataChannel>>>,
    /// Wakes history sync waiting for room on the chat channel.
    chat_drain_notify: DrainNotify,
    /// Binary channel for encrypted file chunks, separate so transfers never delay chat.
    files: Rc<RefCell<Option<RtcDataChannel>>>,
    file_drain_notify: DrainNotify,
    /// Remote-control input: keys and clicks (reliable) and pointer/controller states
    /// (unordered, never retransmitted), so neither waits behind chat or files.
    input_events: Rc<RefCell<Option<RtcDataChannel>>>,
    input_state: Rc<RefCell<Option<RtcDataChannel>>>,
    /// Candidates that arrived before the remote description (relays can reorder).
    pending_ice: RefCell<Vec<IceCandidateData>>,
    closed: Rc<Cell<bool>>,
    signal_out: SignalOut,
    on_event: LinkEventHandler,
}

thread_local! {
    static NEXT_LINK_ID: Cell<u64> = const { Cell::new(1) };
}

impl PeerLink {
    /// Create a link to `remote`. Exactly one side is the `initiator` (the lower pubkey):
    /// it opens the chat channel, which triggers the first offer.
    pub fn new(
        self_pubkey: &str,
        remote: &str,
        config: &RtcConfiguration,
        signal_out: SignalOut,
        on_event: LinkEventHandler,
        initiator: bool,
    ) -> Result<Rc<Self>, JsValue> {
        let pc = RtcPeerConnection::new_with_configuration(config)?;
        let id = NEXT_LINK_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        let link = Rc::new(Self {
            remote: remote.to_string(),
            id,
            created_at: js_sys::Date::now(),
            opened_at: Cell::new(0.0),
            pc,
            polite: self_pubkey > remote,
            making_offer: Rc::new(Cell::new(false)),
            ignore_offer: Cell::new(false),
            chat: Rc::new(RefCell::new(None)),
            chat_drain_notify: Rc::new(RefCell::new(Vec::new())),
            files: Rc::new(RefCell::new(None)),
            file_drain_notify: Rc::new(RefCell::new(Vec::new())),
            input_events: Rc::new(RefCell::new(None)),
            input_state: Rc::new(RefCell::new(None)),
            pending_ice: RefCell::new(Vec::new()),
            closed: Rc::new(Cell::new(false)),
            signal_out,
            on_event: on_event.clone(),
        });

        let notify_closed = link.closed_notifier(on_event.clone());
        {
            let signal_out = link.signal_out.clone();
            let remote = link.remote.clone();
            batch_ice(&link.pc, Rc::new(move |candidates| signal_out(&remote, SignalPayload::IceBatch { to: remote.clone(), candidates })));
        }
        link.install_negotiation();
        watch_state(&link.pc, notify_closed.clone());
        link.install_track_handler(on_event.clone());

        if initiator {
            let init = RtcDataChannelInit::new();
            init.set_ordered(true);
            let dc = link.pc.create_data_channel_with_data_channel_dict(CHAT_LABEL, &init);
            attach_chat_callbacks(&dc, &link.remote, id, on_event.clone(), notify_closed, link.chat_drain_notify.clone());
            *link.chat.borrow_mut() = Some(dc);
            let file_dc = link.pc.create_data_channel_with_data_channel_dict(FILE_LABEL, &init);
            file_dc.set_buffered_amount_low_threshold(FILE_BUFFER_LOW_THRESHOLD);
            attach_file_callbacks(&file_dc, &link.remote, id, on_event.clone(), link.file_drain_notify.clone());
            *link.files.borrow_mut() = Some(file_dc);
            let events_dc = link.pc.create_data_channel_with_data_channel_dict(INPUT_EVENTS_LABEL, &init);
            attach_input_callbacks(&events_dc, &link.remote, id, InputLane::Events, on_event.clone());
            *link.input_events.borrow_mut() = Some(events_dc);
            let lossy = RtcDataChannelInit::new();
            lossy.set_ordered(false);
            lossy.set_max_retransmits(0);
            let state_dc = link.pc.create_data_channel_with_data_channel_dict(INPUT_STATE_LABEL, &lossy);
            attach_input_callbacks(&state_dc, &link.remote, id, InputLane::State, on_event);
            *link.input_state.borrow_mut() = Some(state_dc);
        } else {
            let chat = link.chat.clone();
            let chat_drain_notify = link.chat_drain_notify.clone();
            let files = link.files.clone();
            let file_drain_notify = link.file_drain_notify.clone();
            let input_events = link.input_events.clone();
            let input_state = link.input_state.clone();
            let remote = link.remote.clone();
            let on_dc = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
                let dc = ev.channel();
                match dc.label().as_str() {
                    CHAT_LABEL => {
                        attach_chat_callbacks(&dc, &remote, id, on_event.clone(), notify_closed.clone(), chat_drain_notify.clone());
                        *chat.borrow_mut() = Some(dc);
                    }
                    FILE_LABEL => {
                        dc.set_buffered_amount_low_threshold(FILE_BUFFER_LOW_THRESHOLD);
                        attach_file_callbacks(&dc, &remote, id, on_event.clone(), file_drain_notify.clone());
                        *files.borrow_mut() = Some(dc);
                    }
                    INPUT_EVENTS_LABEL => {
                        attach_input_callbacks(&dc, &remote, id, InputLane::Events, on_event.clone());
                        *input_events.borrow_mut() = Some(dc);
                    }
                    INPUT_STATE_LABEL => {
                        attach_input_callbacks(&dc, &remote, id, InputLane::State, on_event.clone());
                        *input_state.borrow_mut() = Some(dc);
                    }
                    _ => {}
                }
            }) as Box<dyn FnMut(RtcDataChannelEvent)>);
            link.pc.set_ondatachannel(Some(on_dc.as_ref().unchecked_ref()));
            on_dc.forget();
        }

        Ok(link)
    }

    pub fn is_open(&self) -> bool {
        self.chat
            .borrow()
            .as_ref()
            .is_some_and(|dc| dc.ready_state() == RtcDataChannelState::Open)
    }

    pub fn mark_opened(&self) {
        self.opened_at.set(js_sys::Date::now());
    }

    /// How long the chat channel has been open (0 if it hasn't).
    pub fn open_for(&self) -> f64 {
        match self.opened_at.get() {
            at if at > 0.0 && self.is_open() => js_sys::Date::now() - at,
            _ => 0.0,
        }
    }

    /// Send a text frame on the chat channel; returns whether it was handed to the browser.
    pub fn send(&self, text: &str) -> bool {
        match self.chat.borrow().as_ref() {
            Some(dc) if dc.ready_state() == RtcDataChannelState::Open => dc.send_with_str(text).is_ok(),
            _ => false,
        }
    }

    /// Wait until the chat channel has less than `high_water` queued, so bulk traffic (history
    /// sync) leaves room for live messages; `false` once the channel is gone.
    pub async fn wait_for_chat_room(&self, high_water: u32) -> bool {
        use futures::FutureExt;
        loop {
            let buffered = match self.chat.borrow().as_ref() {
                Some(dc) if !self.closed.get() && dc.ready_state() == RtcDataChannelState::Open => dc.buffered_amount(),
                _ => return false,
            };
            if buffered < high_water {
                return true;
            }
            let (tx, rx) = oneshot::channel();
            self.chat_drain_notify.borrow_mut().push(tx);
            futures::select! {
                _ = rx.fuse() => {},
                _ = crate::media::sleep_ms(100).fuse() => {},
            }
        }
    }

    /// Send an encrypted file chunk; returns whether it was handed to the browser.
    pub fn send_bytes(&self, bytes: &[u8]) -> bool {
        match self.files.borrow().as_ref() {
            Some(dc) if dc.ready_state() == RtcDataChannelState::Open => dc.send_with_u8_array(bytes).is_ok(),
            _ => false,
        }
    }

    /// Bytes queued on the file channel (for backpressure); `None` when it is not open.
    pub fn file_buffered_amount(&self) -> Option<u32> {
        if self.closed.get() {
            return None;
        }
        self.files
            .borrow()
            .as_ref()
            .filter(|dc| dc.ready_state() == RtcDataChannelState::Open)
            .map(|dc| dc.buffered_amount())
    }

    /// Resolves when the file channel's queue drops below `FILE_BUFFER_LOW_THRESHOLD`.
    fn file_drained(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.file_drain_notify.borrow_mut().push(tx);
        rx
    }

    /// Send a sealed remote-control packet. State packets are dropped instead of queued
    /// when the channel is backed up: a newer state will follow.
    pub fn send_input(&self, lane: InputLane, packet: &[u8]) -> bool {
        let channel = match lane {
            InputLane::Events => &self.input_events,
            InputLane::State => &self.input_state,
        };
        match channel.borrow().as_ref() {
            Some(dc) if dc.ready_state() == RtcDataChannelState::Open => {
                if lane == InputLane::State && dc.buffered_amount() > INPUT_STATE_MAX_BUFFERED {
                    return false;
                }
                dc.send_with_u8_array(packet).is_ok()
            }
            _ => false,
        }
    }

    /// Start sending `track` (as part of `stream`) to this member; triggers renegotiation.
    pub fn add_track(&self, track: &MediaStreamTrack, stream: &MediaStream) -> RtcRtpSender {
        self.pc.add_track_0(track, stream)
    }

    /// `getStats()` of the whole connection.
    pub fn stats(&self) -> js_sys::Promise {
        self.pc.get_stats()
    }

    pub fn close(&self) {
        self.closed.set(true);
        for tx in self.file_drain_notify.borrow_mut().drain(..).chain(self.chat_drain_notify.borrow_mut().drain(..)) {
            let _ = tx.send(());
        }
        for channel in [&self.chat, &self.files, &self.input_events, &self.input_state] {
            if let Some(dc) = channel.borrow().as_ref() {
                dc.close();
            }
        }
        self.pc.close();
    }

    /// Apply a remote offer or answer (perfect negotiation).
    pub async fn handle_description(&self, is_offer: bool, sdp: String) {
        if self.closed.get() {
            return;
        }
        let stable = self.pc.signaling_state() == RtcSignalingState::Stable;
        let collision = is_offer && (self.making_offer.get() || !stable);
        self.ignore_offer.set(!self.polite && collision);
        if self.ignore_offer.get() {
            log::info!("Ignoring colliding offer from {}", self.remote);
            return;
        }
        if !is_offer && self.pc.signaling_state() != RtcSignalingState::HaveLocalOffer {
            return;
        }
        if collision {
            let rollback = RtcSessionDescriptionInit::new(RtcSdpType::Rollback);
            if let Err(err) = JsFuture::from(self.pc.set_local_description(&rollback)).await {
                log::warn!("Rollback failed for {}: {:?}", self.remote, err);
            }
        }

        let desc = RtcSessionDescriptionInit::new(if is_offer { RtcSdpType::Offer } else { RtcSdpType::Answer });
        desc.set_sdp(&sdp);
        if let Err(err) = JsFuture::from(self.pc.set_remote_description(&desc)).await {
            log::warn!("Failed to apply remote description from {}: {:?}", self.remote, err);
            return;
        }
        if !is_offer {
            self.notify_negotiated();
        }
        self.flush_pending_ice().await;

        if is_offer {
            if let Err(err) = set_local_description_implicit(&self.pc).await {
                log::warn!("Failed to create answer for {}: {:?}", self.remote, err);
                return;
            }
            self.notify_negotiated();
            if let Some(local) = self.pc.local_description() {
                (self.signal_out)(
                    &self.remote,
                    SignalPayload::Answer {
                        to: self.remote.clone(),
                        sdp: local.sdp(),
                    },
                );
            }
        }
    }

    fn notify_negotiated(&self) {
        if !self.closed.get() {
            (self.on_event)(&self.remote, self.id, LinkEvent::Negotiated);
        }
    }

    pub async fn add_candidates(&self, candidates: Vec<IceCandidateData>) {
        if self.pc.remote_description().is_none() {
            self.pending_ice.borrow_mut().extend(candidates);
            return;
        }
        for candidate in candidates {
            self.add_candidate(candidate).await;
        }
    }

    async fn flush_pending_ice(&self) {
        let pending = std::mem::take(&mut *self.pending_ice.borrow_mut());
        for candidate in pending {
            self.add_candidate(candidate).await;
        }
    }

    async fn add_candidate(&self, c: IceCandidateData) {
        if !add_ice_candidate(&self.pc, c).await && !self.ignore_offer.get() {
            log::warn!("Failed to add ICE candidate from {}", self.remote);
        }
    }

    /// Fires `Closed` at most once, however many browser events report the failure.
    fn closed_notifier(&self, on_event: LinkEventHandler) -> Rc<dyn Fn()> {
        let remote = self.remote.clone();
        let id = self.id;
        let notified = Rc::new(Cell::new(false));
        Rc::new(move || {
            if !notified.replace(true) {
                on_event(&remote, id, LinkEvent::Closed);
            }
        })
    }

    fn install_track_handler(&self, on_event: LinkEventHandler) {
        let remote = self.remote.clone();
        let id = self.id;
        let on_track = Closure::wrap(Box::new(move |ev: RtcTrackEvent| {
            let streams = ev.streams();
            let stream = (streams.length() > 0).then(|| streams.get(0).unchecked_into::<MediaStream>());
            let receiver = js_sys::Reflect::get(&ev, &"receiver".into()).unwrap_or(JsValue::UNDEFINED);
            on_event(&remote, id, LinkEvent::Track(ev.track(), stream, receiver));
        }) as Box<dyn FnMut(RtcTrackEvent)>);
        self.pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
        on_track.forget();
    }

    fn install_negotiation(&self) {
        let pc = self.pc.clone();
        let making_offer = self.making_offer.clone();
        let closed = self.closed.clone();
        let signal_out = self.signal_out.clone();
        let remote = self.remote.clone();
        let on_needed = Closure::wrap(Box::new(move || {
            if closed.get() {
                return;
            }
            let pc = pc.clone();
            let making_offer = making_offer.clone();
            let signal_out = signal_out.clone();
            let remote = remote.clone();
            wasm_bindgen_futures::spawn_local(async move {
                making_offer.set(true);
                match set_local_description_implicit(&pc).await {
                    Ok(()) => {
                        if let Some(local) = pc.local_description() {
                            signal_out(
                                &remote,
                                SignalPayload::Offer {
                                    to: remote.clone(),
                                    sdp: local.sdp(),
                                },
                            );
                        }
                    }
                    Err(err) => log::warn!("Failed to create offer: {:?}", err),
                }
                making_offer.set(false);
            });
        }) as Box<dyn FnMut()>);
        self.pc.set_onnegotiationneeded(Some(on_needed.as_ref().unchecked_ref()));
        on_needed.forget();
    }
}

fn attach_chat_callbacks(
    dc: &RtcDataChannel,
    remote: &str,
    id: u64,
    on_event: LinkEventHandler,
    notify_closed: Rc<dyn Fn()>,
    drain_notify: DrainNotify,
) {
    dc.set_buffered_amount_low_threshold(CHAT_BUFFER_LOW_THRESHOLD);
    notify_on_drain(dc, drain_notify);
    {
        let on_event = on_event.clone();
        let remote = remote.to_string();
        let on_open = Closure::wrap(Box::new(move || {
            on_event(&remote, id, LinkEvent::Open);
        }) as Box<dyn FnMut()>);
        dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
    }
    {
        let on_close = Closure::wrap(Box::new(move || notify_closed()) as Box<dyn FnMut()>);
        dc.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    }
    {
        let remote = remote.to_string();
        let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
            if let Some(text) = ev.data().as_string() {
                on_event(&remote, id, LinkEvent::Message(text));
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();
    }
}

fn attach_file_callbacks(
    dc: &RtcDataChannel,
    remote: &str,
    id: u64,
    on_event: LinkEventHandler,
    drain_notify: DrainNotify,
) {
    dc.set_binary_type(RtcDataChannelType::Arraybuffer);
    let remote = remote.to_string();
    let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
        if let Ok(buffer) = ev.data().dyn_into::<js_sys::ArrayBuffer>() {
            on_event(&remote, id, LinkEvent::Chunk(js_sys::Uint8Array::new(&buffer).to_vec()));
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    notify_on_drain(dc, drain_notify);
}

/// Wake everyone waiting in `drain_notify` whenever `dc`'s queue drops below its threshold.
fn notify_on_drain(dc: &RtcDataChannel, drain_notify: DrainNotify) {
    let on_low = Closure::wrap(Box::new(move || {
        for tx in drain_notify.borrow_mut().drain(..) {
            let _ = tx.send(());
        }
    }) as Box<dyn FnMut()>);
    dc.set_onbufferedamountlow(Some(on_low.as_ref().unchecked_ref()));
    on_low.forget();
}

fn attach_input_callbacks(dc: &RtcDataChannel, remote: &str, id: u64, lane: InputLane, on_event: LinkEventHandler) {
    dc.set_binary_type(RtcDataChannelType::Arraybuffer);
    let remote = remote.to_string();
    let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
        if let Ok(buffer) = ev.data().dyn_into::<js_sys::ArrayBuffer>() {
            on_event(&remote, id, LinkEvent::Input { lane, packet: js_sys::Uint8Array::new(&buffer).to_vec() });
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
}

/// `pc.setLocalDescription()` with no argument: creates the right offer or answer for
/// the current signaling state (web-sys only binds the explicit form).
async fn set_local_description_implicit(pc: &RtcPeerConnection) -> Result<(), JsValue> {
    let method: js_sys::Function = js_sys::Reflect::get(pc, &"setLocalDescription".into())?.dyn_into()?;
    let promise: js_sys::Promise = method.call0(pc)?.dyn_into()?;
    JsFuture::from(promise).await?;
    Ok(())
}

/// Batch local ICE candidates (100 ms or 10 candidates) to stay under relay rate limits;
/// `out` sends each batch.
fn batch_ice(pc: &RtcPeerConnection, out: Rc<dyn Fn(Vec<IceCandidateData>)>) {
    let batch = Rc::new(RefCell::new(Vec::<IceCandidateData>::new()));
    let timer = Rc::new(Cell::new(None::<i32>));

    let flush: Rc<dyn Fn()> = {
        let batch = batch.clone();
        Rc::new(move || {
            let candidates = std::mem::take(&mut *batch.borrow_mut());
            if !candidates.is_empty() {
                out(candidates);
            }
        })
    };

    let on_ice = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
        let Some(candidate) = ev.candidate() else {
            // Gathering complete.
            flush();
            return;
        };
        batch.borrow_mut().push(IceCandidateData {
            candidate: candidate.candidate(),
            sdp_mid: candidate.sdp_mid(),
            sdp_m_line_index: candidate.sdp_m_line_index(),
        });
        if let (Some(handle), Some(w)) = (timer.take(), window()) {
            w.clear_timeout_with_handle(handle);
        }
        if batch.borrow().len() >= ICE_BATCH_MAX {
            flush();
            return;
        }
        let flush = flush.clone();
        let cb = Closure::once(move || flush());
        if let Some(w) = window() {
            if let Ok(handle) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                ICE_BATCH_DELAY_MS,
            ) {
                timer.set(Some(handle));
            }
        }
        cb.forget();
    }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
    pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
    on_ice.forget();
}

/// Call `notify_closed` when the connection fails or closes, or stays "disconnected" for
/// `DISCONNECT_GRACE_MS`.
fn watch_state(pc: &RtcPeerConnection, notify_closed: Rc<dyn Fn()>) {
    let pc_c = pc.clone();
    let on_state = Closure::wrap(Box::new(move || match pc_c.connection_state() {
        RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed => notify_closed(),
        RtcPeerConnectionState::Disconnected => {
            let pc = pc_c.clone();
            let notify_closed = notify_closed.clone();
            let check = Closure::once(move || {
                if matches!(
                    pc.connection_state(),
                    RtcPeerConnectionState::Disconnected | RtcPeerConnectionState::Failed
                ) {
                    notify_closed();
                }
            });
            if let Some(w) = window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    check.as_ref().unchecked_ref(),
                    DISCONNECT_GRACE_MS,
                );
            }
            check.forget();
        }
        _ => {}
    }) as Box<dyn FnMut()>);
    pc.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
    on_state.forget();
}

/// Returns whether the browser took the candidate.
async fn add_ice_candidate(pc: &RtcPeerConnection, c: IceCandidateData) -> bool {
    let init = RtcIceCandidateInit::new(&c.candidate);
    init.set_sdp_mid(c.sdp_mid.as_deref());
    init.set_sdp_m_line_index(c.sdp_m_line_index);
    let Ok(candidate) = RtcIceCandidate::new(&init) else {
        return false;
    };
    JsFuture::from(pc.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&candidate))).await.is_ok()
}

// ---- Extra file links --------------------------------------------------------------------

pub enum FileLinkEvent {
    Open,
    /// Failed or closed by the other side; chunks still queued on it are lost.
    Closed,
    Chunk(Vec<u8>),
}

/// Receives `(remote pubkey, link number, dialed by us, event)`.
pub type FileLinkEventHandler = Rc<dyn Fn(&str, u32, bool, FileLinkEvent)>;

/// Delivers a file link's offer, answer or ICE to its member, over their main link.
pub type FileLinkSignalOut = Rc<dyn Fn(SignalPayload)>;

/// An extra connection to a member that only carries file chunks: one ordered channel, no
/// media, negotiated once over the open main link. Several of them carry one upload in
/// parallel, each with its own congestion control (see `protocol::transfer`). The side that
/// dials a file link sends on it; the other side only receives.
pub struct FileLink {
    pub remote: String,
    pub id: u32,
    pub dialed: bool,
    /// Open but given no new chunks: it did not raise the rate. Another upload may take it back.
    pub set_aside: Cell<bool>,
    pc: RtcPeerConnection,
    dc: RtcDataChannel,
    drain_notify: DrainNotify,
    pending_ice: RefCell<Vec<IceCandidateData>>,
    described: Cell<bool>,
    closed: Rc<Cell<bool>>,
    signal_out: FileLinkSignalOut,
}

impl FileLink {
    /// Open file link `id` to `remote`. The dialer sends its offer right away; the other
    /// side creates its end when that offer arrives.
    pub fn new(
        remote: &str,
        id: u32,
        dialed: bool,
        config: &RtcConfiguration,
        signal_out: FileLinkSignalOut,
        on_event: FileLinkEventHandler,
    ) -> Result<Rc<Self>, JsValue> {
        let pc = RtcPeerConnection::new_with_configuration(config)?;
        // Negotiated with the same id on both ends: no `datachannel` event to wait for.
        let init = RtcDataChannelInit::new();
        init.set_ordered(true);
        init.set_negotiated(true);
        init.set_id(0);
        let dc = pc.create_data_channel_with_data_channel_dict(FILE_LABEL, &init);
        dc.set_buffered_amount_low_threshold(FILE_BUFFER_LOW_THRESHOLD);
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);
        let link = Rc::new(Self {
            remote: remote.to_string(),
            id,
            dialed,
            set_aside: Cell::new(false),
            pc,
            dc,
            drain_notify: Rc::new(RefCell::new(Vec::new())),
            pending_ice: RefCell::new(Vec::new()),
            described: Cell::new(false),
            closed: Rc::new(Cell::new(false)),
            signal_out,
        });

        let notify_closed: Rc<dyn Fn()> = {
            let remote = link.remote.clone();
            let on_event = on_event.clone();
            let notified = Cell::new(false);
            Rc::new(move || {
                if !notified.replace(true) {
                    on_event(&remote, id, dialed, FileLinkEvent::Closed);
                }
            })
        };
        {
            let signal_out = link.signal_out.clone();
            let remote = link.remote.clone();
            batch_ice(&link.pc, Rc::new(move |candidates| signal_out(SignalPayload::IceBatch { to: remote.clone(), candidates })));
        }
        watch_state(&link.pc, notify_closed.clone());
        {
            let remote = link.remote.clone();
            let on_event = on_event.clone();
            let on_open = Closure::wrap(Box::new(move || on_event(&remote, id, dialed, FileLinkEvent::Open)) as Box<dyn FnMut()>);
            link.dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            on_open.forget();
        }
        {
            let notify_closed = notify_closed.clone();
            let on_close = Closure::wrap(Box::new(move || notify_closed()) as Box<dyn FnMut()>);
            link.dc.set_onclose(Some(on_close.as_ref().unchecked_ref()));
            on_close.forget();
        }
        {
            let remote = link.remote.clone();
            let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
                if let Ok(buffer) = ev.data().dyn_into::<js_sys::ArrayBuffer>() {
                    on_event(&remote, id, dialed, FileLinkEvent::Chunk(js_sys::Uint8Array::new(&buffer).to_vec()));
                }
            }) as Box<dyn FnMut(MessageEvent)>);
            link.dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            on_message.forget();
        }
        notify_on_drain(&link.dc, link.drain_notify.clone());

        if dialed {
            let pc = link.pc.clone();
            let signal_out = link.signal_out.clone();
            let remote = link.remote.clone();
            let closed = link.closed.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match set_local_description_implicit(&pc).await {
                    Ok(()) => {
                        if let Some(local) = pc.local_description().filter(|_| !closed.get()) {
                            signal_out(SignalPayload::Offer { to: remote.clone(), sdp: local.sdp() });
                        }
                    }
                    Err(err) => {
                        log::warn!("Failed to create a file link offer: {:?}", err);
                        notify_closed();
                    }
                }
            });
        }
        Ok(link)
    }

    pub fn is_open(&self) -> bool {
        !self.closed.get() && self.dc.ready_state() == RtcDataChannelState::Open
    }

    /// Apply the other side's offer (we did not dial) or answer (we did). Each file link is
    /// negotiated exactly once; anything else is ignored.
    pub async fn handle_description(&self, is_offer: bool, sdp: String) {
        if self.closed.get() || is_offer == self.dialed || self.described.replace(true) {
            return;
        }
        let desc = RtcSessionDescriptionInit::new(if is_offer { RtcSdpType::Offer } else { RtcSdpType::Answer });
        desc.set_sdp(&sdp);
        if let Err(err) = JsFuture::from(self.pc.set_remote_description(&desc)).await {
            log::warn!("Failed to apply a file link description: {:?}", err);
            return;
        }
        let pending = std::mem::take(&mut *self.pending_ice.borrow_mut());
        for candidate in pending {
            add_ice_candidate(&self.pc, candidate).await;
        }
        if !is_offer {
            return;
        }
        if let Err(err) = set_local_description_implicit(&self.pc).await {
            log::warn!("Failed to answer a file link: {:?}", err);
            return;
        }
        if let Some(local) = self.pc.local_description().filter(|_| !self.closed.get()) {
            (self.signal_out)(SignalPayload::Answer { to: self.remote.clone(), sdp: local.sdp() });
        }
    }

    pub async fn add_candidates(&self, candidates: Vec<IceCandidateData>) {
        if self.pc.remote_description().is_none() {
            self.pending_ice.borrow_mut().extend(candidates);
            return;
        }
        for candidate in candidates {
            add_ice_candidate(&self.pc, candidate).await;
        }
    }

    /// Close both ends (the other side sees the channel close). No event fires here.
    pub fn close(&self) {
        self.closed.set(true);
        for tx in self.drain_notify.borrow_mut().drain(..) {
            let _ = tx.send(());
        }
        self.dc.close();
        self.pc.close();
    }
}

/// Where a file chunk can go: the main link's file channel or one of our file links.
#[derive(Clone)]
pub enum ChunkRoute {
    Main(Rc<PeerLink>),
    Extra(Rc<FileLink>),
}

impl ChunkRoute {
    /// Tells routes apart: `None` for the main link, else the file link's number.
    pub fn key(&self) -> Option<u32> {
        match self {
            Self::Main(_) => None,
            Self::Extra(link) => Some(link.id),
        }
    }

    /// Bytes queued; `None` when it can't take chunks.
    pub fn buffered(&self) -> Option<u32> {
        match self {
            Self::Main(link) => link.file_buffered_amount(),
            Self::Extra(link) => link.is_open().then(|| link.dc.buffered_amount()),
        }
    }

    /// Returns whether the browser took the chunk.
    pub fn send(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Main(link) => link.send_bytes(bytes),
            Self::Extra(link) => link.is_open() && link.dc.send_with_u8_array(bytes).is_ok(),
        }
    }

    fn drained(&self) -> oneshot::Receiver<()> {
        match self {
            Self::Main(link) => link.file_drained(),
            Self::Extra(link) => {
                let (tx, rx) = oneshot::channel();
                link.drain_notify.borrow_mut().push(tx);
                rx
            }
        }
    }
}

/// Wait until one of `routes` has less than `high_water` queued and return the emptiest
/// (ties go round-robin from `turn`, so chunks spread even when every queue is empty).
/// `None` when none of them is open.
pub async fn wait_for_room(routes: &[ChunkRoute], high_water: u32, turn: usize) -> Option<usize> {
    use futures::FutureExt;
    loop {
        let mut best: Option<(u32, usize)> = None;
        let mut any_open = false;
        for k in 0..routes.len() {
            let i = (turn + k) % routes.len();
            if let Some(buffered) = routes[i].buffered() {
                any_open = true;
                if buffered < high_water && best.is_none_or(|(least, _)| buffered < least) {
                    best = Some((buffered, i));
                }
            }
        }
        if let Some((_, i)) = best {
            return Some(i);
        }
        if !any_open {
            return None;
        }
        let drained = futures::future::select_all(routes.iter().map(ChunkRoute::drained));
        futures::select! {
            _ = drained.fuse() => {},
            _ = crate::media::sleep_ms(100).fuse() => {},
        }
    }
}
