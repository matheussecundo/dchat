//! One WebRTC link per remote room member, using the "perfect negotiation" pattern
//! so either side can (re)negotiate at any time without signaling glare.

use protocol::{IceCandidateData, SignalPayload};
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
const ICE_BATCH_DELAY_MS: i32 = 100;
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
    /// The member started sending a media track (with its stream, when announced).
    Track(MediaStreamTrack, Option<MediaStream>),
    /// A binary packet arrived on the file-transfer channel.
    Chunk(Vec<u8>),
}

/// Receives `(remote pubkey, link id, event)`. The id tells a replaced link's late
/// events apart from the current link's.
pub type LinkEventHandler = Rc<dyn Fn(&str, u64, LinkEvent)>;

/// Delivers this link's signaling (offer, answer, ICE) to `remote`: over the link itself
/// once it is open, otherwise through the Nostr relays.
pub type SignalOut = Rc<dyn Fn(&str, SignalPayload)>;

pub struct PeerLink {
    pub remote: String,
    pub id: u64,
    pub created_at: f64,
    pc: RtcPeerConnection,
    /// The polite side yields when both offer at once (higher pubkey is polite).
    polite: bool,
    making_offer: Rc<Cell<bool>>,
    ignore_offer: Cell<bool>,
    chat: Rc<RefCell<Option<RtcDataChannel>>>,
    /// Binary channel for encrypted file chunks, separate so transfers never delay chat.
    files: Rc<RefCell<Option<RtcDataChannel>>>,
    /// Candidates that arrived before the remote description (relays can reorder).
    pending_ice: RefCell<Vec<IceCandidateData>>,
    closed: Rc<Cell<bool>>,
    signal_out: SignalOut,
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
            pc,
            polite: self_pubkey > remote,
            making_offer: Rc::new(Cell::new(false)),
            ignore_offer: Cell::new(false),
            chat: Rc::new(RefCell::new(None)),
            files: Rc::new(RefCell::new(None)),
            pending_ice: RefCell::new(Vec::new()),
            closed: Rc::new(Cell::new(false)),
            signal_out,
        });

        let notify_closed = link.closed_notifier(on_event.clone());
        link.install_ice_batching();
        link.install_negotiation();
        link.install_state_watch(notify_closed.clone());
        link.install_track_handler(on_event.clone());

        if initiator {
            let init = RtcDataChannelInit::new();
            init.set_ordered(true);
            let dc = link.pc.create_data_channel_with_data_channel_dict(CHAT_LABEL, &init);
            attach_chat_callbacks(&dc, &link.remote, id, on_event.clone(), notify_closed);
            *link.chat.borrow_mut() = Some(dc);
            let file_dc = link.pc.create_data_channel_with_data_channel_dict(FILE_LABEL, &init);
            attach_file_callbacks(&file_dc, &link.remote, id, on_event);
            *link.files.borrow_mut() = Some(file_dc);
        } else {
            let chat = link.chat.clone();
            let files = link.files.clone();
            let remote = link.remote.clone();
            let on_dc = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
                let dc = ev.channel();
                match dc.label().as_str() {
                    CHAT_LABEL => {
                        attach_chat_callbacks(&dc, &remote, id, on_event.clone(), notify_closed.clone());
                        *chat.borrow_mut() = Some(dc);
                    }
                    FILE_LABEL => {
                        attach_file_callbacks(&dc, &remote, id, on_event.clone());
                        *files.borrow_mut() = Some(dc);
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

    /// Send a text frame on the chat channel; returns whether it was handed to the browser.
    pub fn send(&self, text: &str) -> bool {
        match self.chat.borrow().as_ref() {
            Some(dc) if dc.ready_state() == RtcDataChannelState::Open => dc.send_with_str(text).is_ok(),
            _ => false,
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
        self.files
            .borrow()
            .as_ref()
            .filter(|dc| dc.ready_state() == RtcDataChannelState::Open)
            .map(|dc| dc.buffered_amount())
    }

    /// Start sending `track` (as part of `stream`) to this member; triggers renegotiation.
    pub fn add_track(&self, track: &MediaStreamTrack, stream: &MediaStream) -> RtcRtpSender {
        self.pc.add_track_0(track, stream)
    }

    pub fn close(&self) {
        self.closed.set(true);
        for channel in [&self.chat, &self.files] {
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
        self.flush_pending_ice().await;

        if is_offer {
            if let Err(err) = set_local_description_implicit(&self.pc).await {
                log::warn!("Failed to create answer for {}: {:?}", self.remote, err);
                return;
            }
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
        let init = RtcIceCandidateInit::new(&c.candidate);
        init.set_sdp_mid(c.sdp_mid.as_deref());
        init.set_sdp_m_line_index(c.sdp_m_line_index);
        let Ok(candidate) = RtcIceCandidate::new(&init) else {
            return;
        };
        let result =
            JsFuture::from(self.pc.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&candidate))).await;
        if result.is_err() && !self.ignore_offer.get() {
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
            on_event(&remote, id, LinkEvent::Track(ev.track(), stream));
        }) as Box<dyn FnMut(RtcTrackEvent)>);
        self.pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
        on_track.forget();
    }

    fn install_state_watch(&self, notify_closed: Rc<dyn Fn()>) {
        let pc = self.pc.clone();
        let on_state = Closure::wrap(Box::new(move || match pc.connection_state() {
            RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed => notify_closed(),
            RtcPeerConnectionState::Disconnected => {
                let pc = pc.clone();
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
        self.pc.set_onconnectionstatechange(Some(on_state.as_ref().unchecked_ref()));
        on_state.forget();
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

    /// Batch local ICE candidates (100 ms or 10 candidates) to stay under relay rate limits.
    fn install_ice_batching(&self) {
        let signal_out = self.signal_out.clone();
        let remote = self.remote.clone();
        let batch = Rc::new(RefCell::new(Vec::<IceCandidateData>::new()));
        let timer = Rc::new(Cell::new(None::<i32>));

        let flush: Rc<dyn Fn()> = {
            let batch = batch.clone();
            Rc::new(move || {
                let candidates = std::mem::take(&mut *batch.borrow_mut());
                if !candidates.is_empty() {
                    signal_out(
                        &remote,
                        SignalPayload::IceBatch {
                            to: remote.clone(),
                            candidates,
                        },
                    );
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
        self.pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        on_ice.forget();
    }
}

fn attach_chat_callbacks(
    dc: &RtcDataChannel,
    remote: &str,
    id: u64,
    on_event: LinkEventHandler,
    notify_closed: Rc<dyn Fn()>,
) {
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

fn attach_file_callbacks(dc: &RtcDataChannel, remote: &str, id: u64, on_event: LinkEventHandler) {
    dc.set_binary_type(RtcDataChannelType::Arraybuffer);
    let remote = remote.to_string();
    let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
        if let Ok(buffer) = ev.data().dyn_into::<js_sys::ArrayBuffer>() {
            on_event(&remote, id, LinkEvent::Chunk(js_sys::Uint8Array::new(&buffer).to_vec()));
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
