use crate::state::{current_time_string, CallState, ChatMessageUi, ConnectionStatus};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use leptos::*;
use protocol::crypto::{decrypt_json, encrypt_json};
use protocol::{
    ClientMessage, DataChannelMessage, EncryptedPayload, ServerMessage, SignalPayload, KEY_LENGTH,
};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    window, HtmlAudioElement, MediaStream, MediaStreamConstraints, MediaStreamTrack,
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit,
    RtcDataChannelState, RtcIceCandidate, RtcIceCandidateInit, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcRtpSender, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
};

#[allow(dead_code)]
pub struct WebRtcSession {
    pub room_id: String,
    pub key: [u8; KEY_LENGTH],
    pub peer: RtcPeerConnection,
    pub data_channel: Rc<RefCell<Option<RtcDataChannel>>>,
    pub ws_sender: mpsc::UnboundedSender<ClientMessage>,
    pub local_audio_stream: Rc<RefCell<Option<MediaStream>>>,
    pub audio_senders: Rc<RefCell<Vec<RtcRtpSender>>>,
    pub remote_audio: Rc<RefCell<Option<HtmlAudioElement>>>,
    pub call_state: WriteSignal<CallState>,
    pub is_mic_muted: WriteSignal<bool>,
    pub toast_signal: WriteSignal<Option<String>>,
}

impl WebRtcSession {
    pub fn send_chat_message(
        &self,
        text: &str,
        messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    ) -> Result<(), String> {
        let dc_guard = self.data_channel.borrow();
        let dc = dc_guard.as_ref().ok_or("Data channel is not open yet")?;

        if dc.ready_state() != RtcDataChannelState::Open {
            return Err("Data channel is not yet connected".into());
        }

        let msg_id = uuid::Uuid::new_v4().to_string();
        let timestamp = js_sys::Date::now() as u64;
        let chat_msg = DataChannelMessage::Chat {
            id: msg_id.clone(),
            sender: "Self".into(),
            text: text.to_string(),
            timestamp,
        };

        self.send_dc_message(&chat_msg)?;

        let time_str = current_time_string();
        messages_signal.update(|msgs| {
            msgs.push(ChatMessageUi {
                id: msg_id,
                sender: "You".into(),
                is_self: true,
                text: text.to_string(),
                time: time_str,
            });
        });

        Ok(())
    }

    pub fn send_dc_message(&self, msg: &DataChannelMessage) -> Result<(), String> {
        let dc_guard = self.data_channel.borrow();
        let dc = dc_guard.as_ref().ok_or("Data channel is not open")?;

        let encrypted = encrypt_json(&self.key, msg)
            .map_err(|e| format!("Encryption error: {e}"))?;

        let json_str = serde_json::to_string(&encrypted)
            .map_err(|e| format!("Serialization error: {e}"))?;

        dc.send_with_str(&json_str)
            .map_err(|e| format!("Data channel send error: {:?}", e))?;

        Ok(())
    }

    pub fn start_audio_call(&self) {
        if let Err(err) = self.send_dc_message(&DataChannelMessage::CallInvite) {
            self.toast_signal.set(Some(format!("Cannot initiate call: {err}")));
            return;
        }
        self.call_state.set(CallState::Calling);
    }

    pub fn accept_audio_call(&self) {
        let pc = self.peer.clone();
        let key = self.key;
        let room_id = self.room_id.clone();
        let mut ws_tx = self.ws_sender.clone();
        let local_stream_cell = self.local_audio_stream.clone();
        let audio_senders_cell = self.audio_senders.clone();
        let call_state = self.call_state;
        let toast = self.toast_signal;

        let dc_msg_sender = self.clone_dc_sender();

        wasm_bindgen_futures::spawn_local(async move {
            match capture_microphone().await {
                Ok(stream) => {
                    let tracks = stream.get_audio_tracks();
                    let mut senders = Vec::new();
                    for i in 0..tracks.length() {
                        let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                        let sender = pc.add_track_0(&track, &stream);
                        senders.push(sender);
                    }
                    *local_stream_cell.borrow_mut() = Some(stream);
                    *audio_senders_cell.borrow_mut() = senders;

                    let _ = dc_msg_sender(DataChannelMessage::CallAccepted);
                    call_state.set(CallState::Active);

                    // Renegotiate: create and send offer with audio track
                    if let Ok(offer) = wasm_bindgen_futures::JsFuture::from(pc.create_offer()).await {
                        if let Ok(sdp) = js_sys::Reflect::get(&offer, &"sdp".into()) {
                            if let Some(sdp_str) = sdp.as_string() {
                                let init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                                init.set_sdp(&sdp_str);
                                let _ = wasm_bindgen_futures::JsFuture::from(pc.set_local_description(&init)).await;
                                if let Ok(enc) = encrypt_json(&key, &SignalPayload::Offer { sdp: sdp_str }) {
                                    let _ = ws_tx.start_send(ClientMessage::Signal {
                                        room_id,
                                        payload: enc,
                                    });
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log::error!("Microphone access error: {:?}", err);
                    toast.set(Some("Microphone access denied. Check browser permissions.".into()));
                    let _ = dc_msg_sender(DataChannelMessage::CallRejected);
                    call_state.set(CallState::Idle);
                }
            }
        });
    }

    pub fn reject_audio_call(&self) {
        let _ = self.send_dc_message(&DataChannelMessage::CallRejected);
        self.call_state.set(CallState::Idle);
    }

    pub fn toggle_mic_mute(&self) {
        let stream_guard = self.local_audio_stream.borrow();
        if let Some(ref stream) = *stream_guard {
            let tracks = stream.get_audio_tracks();
            let mut new_muted = false;
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                let current = track.enabled();
                track.set_enabled(!current);
                new_muted = current; // if it was enabled, it is now muted
            }
            self.is_mic_muted.set(new_muted);
        }
    }

    pub fn end_audio_call(&self) {
        let _ = self.send_dc_message(&DataChannelMessage::CallEnded);
        self.cleanup_audio();
    }

    pub fn cleanup_audio(&self) {
        if let Some(stream) = self.local_audio_stream.borrow_mut().take() {
            let tracks = stream.get_tracks();
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                track.stop();
            }
        }

        for sender in self.audio_senders.borrow_mut().drain(..) {
            let _ = self.peer.remove_track(&sender);
        }

        if let Some(audio) = self.remote_audio.borrow_mut().take() {
            let _ = audio.pause();
            audio.set_src("");
        }

        self.is_mic_muted.set(false);
        self.call_state.set(CallState::Idle);
    }

    fn clone_dc_sender(&self) -> Box<dyn Fn(DataChannelMessage) -> Result<(), String>> {
        let dc_cell = self.data_channel.clone();
        let key = self.key;
        Box::new(move |msg| {
            let dc_guard = dc_cell.borrow();
            let dc = dc_guard.as_ref().ok_or("Data channel is not open")?;
            let encrypted = encrypt_json(&key, &msg)
                .map_err(|e| format!("Encryption error: {e}"))?;
            let json_str = serde_json::to_string(&encrypted)
                .map_err(|e| format!("Serialization error: {e}"))?;
            dc.send_with_str(&json_str)
                .map_err(|e| format!("Send error: {:?}", e))?;
            Ok(())
        })
    }
}

pub async fn capture_microphone() -> Result<MediaStream, JsValue> {
    let win = window().ok_or_else(|| JsValue::from_str("No window"))?;
    let nav = win.navigator();
    let media_devices = nav.media_devices()?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&JsValue::from_bool(true));
    constraints.set_video(&JsValue::from_bool(false));
    let promise = media_devices.get_user_media_with_constraints(&constraints)?;
    let js_stream = wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(js_stream.unchecked_into())
}

pub fn start_webrtc_session(
    room_id: String,
    key: [u8; KEY_LENGTH],
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    call_state: WriteSignal<CallState>,
    is_mic_muted: WriteSignal<bool>,
    toast_signal: WriteSignal<Option<String>>,
) -> Result<Rc<RefCell<Option<WebRtcSession>>>, JsValue> {
    status_signal.set(ConnectionStatus::ConnectingRelay);

    let win = window().ok_or_else(|| JsValue::from_str("No window"))?;
    let loc = win.location();
    let host = loc.host()?;
    let proto = loc.protocol()?;
    let ws_proto = if proto == "https:" { "wss:" } else { "ws:" };
    let ws_url = format!("{}//{}/ws", ws_proto, host);

    // Initialize RTCPeerConnection
    let rtc_config = RtcConfiguration::new();
    let ice_servers = js_sys::Array::new();
    let stun_server = js_sys::Object::new();
    js_sys::Reflect::set(
        &stun_server,
        &"urls".into(),
        &"stun:stun.l.google.com:19302".into(),
    )?;
    ice_servers.push(&stun_server);
    rtc_config.set_ice_servers(&ice_servers);

    let pc = RtcPeerConnection::new_with_configuration(&rtc_config)?;
    let data_channel_cell = Rc::new(RefCell::new(None::<RtcDataChannel>));
    let local_audio_stream = Rc::new(RefCell::new(None::<MediaStream>));
    let audio_senders = Rc::new(RefCell::new(Vec::<RtcRtpSender>::new()));
    let remote_audio = Rc::new(RefCell::new(None::<HtmlAudioElement>));
    let session_cell = Rc::new(RefCell::new(None::<WebRtcSession>));

    let (ws_tx, mut ws_rx) = mpsc::unbounded::<ClientMessage>();

    // Setup incoming track listener for remote audio
    {
        let remote_audio_cell = remote_audio.clone();
        let on_track = Closure::wrap(Box::new(move |ev: RtcTrackEvent| {
            let streams = ev.streams();
            if streams.length() > 0 {
                let remote_stream: MediaStream = streams.get(0).unchecked_into();
                if let Ok(audio) = HtmlAudioElement::new() {
                    audio.set_src_object(Some(&remote_stream));
                    audio.set_autoplay(true);
                    let _ = audio.play();
                    *remote_audio_cell.borrow_mut() = Some(audio);
                    log::info!("Remote audio track received and attached to audio element");
                }
            }
        }) as Box<dyn FnMut(RtcTrackEvent)>);
        pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
        on_track.forget();
    }

    // Store session
    *session_cell.borrow_mut() = Some(WebRtcSession {
        room_id: room_id.clone(),
        key,
        peer: pc.clone(),
        data_channel: data_channel_cell.clone(),
        ws_sender: ws_tx.clone(),
        local_audio_stream: local_audio_stream.clone(),
        audio_senders: audio_senders.clone(),
        remote_audio: remote_audio.clone(),
        call_state,
        is_mic_muted,
        toast_signal,
    });

    // Handle incoming ICE candidates on PC
    {
        let room_id_c = room_id.clone();
        let key_c = key;
        let mut ws_tx_ice = ws_tx.clone();

        let on_ice = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
            if let Some(candidate) = ev.candidate() {
                let cand_str = candidate.candidate();
                let sdp_mid = candidate.sdp_mid();
                let sdp_m_line_index = candidate.sdp_m_line_index();

                let payload = SignalPayload::IceCandidate {
                    candidate: cand_str,
                    sdp_mid,
                    sdp_m_line_index,
                };

                if let Ok(enc) = encrypt_json(&key_c, &payload) {
                    let _ = ws_tx_ice.start_send(ClientMessage::Signal {
                        room_id: room_id_c.clone(),
                        payload: enc,
                    });
                }
            }
        }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);

        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        on_ice.forget();
    }

    // Connect WebSocket
    wasm_bindgen_futures::spawn_local({
        let pc = pc.clone();
        let room_id = room_id.clone();
        let key = key;
        let data_channel_cell = data_channel_cell.clone();
        let ws_tx = ws_tx.clone();
        let session_cell_c = session_cell.clone();

        async move {
            let ws = match WebSocket::open(&ws_url) {
                Ok(w) => w,
                Err(err) => {
                    log::error!("WebSocket connection failed: {:?}", err);
                    status_signal.set(ConnectionStatus::Error(
                        "Cannot connect to signaling relay".into(),
                    ));
                    return;
                }
            };

            let (mut ws_sink, mut ws_stream) = ws.split();

            // Task to send outgoing messages to WebSocket
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(msg) = ws_rx.next().await {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        if ws_sink.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                }
            });

            // Send Join message
            let _ = ws_tx.unbounded_send(ClientMessage::Join {
                room_id: room_id.clone(),
            });

            // Listen for server messages
            while let Some(msg_res) = ws_stream.next().await {
                let text = match msg_res {
                    Ok(Message::Text(t)) => t,
                    _ => continue,
                };

                let srv_msg: ServerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                match srv_msg {
                    ServerMessage::Joined { peer_count, is_initiator, .. } => {
                        if is_initiator {
                            if peer_count < 2 {
                                status_signal.set(ConnectionStatus::WaitingForPeer);
                            }
                        } else {
                            // Responder joins
                            status_signal.set(ConnectionStatus::NegotiatingWebRtc);
                            setup_responder_datachannel(
                                &pc,
                                key,
                                data_channel_cell.clone(),
                                status_signal,
                                messages_signal,
                                session_cell_c.clone(),
                            );
                        }
                    }
                    ServerMessage::PeerJoined => {
                        // Initiator sees peer joined -> create DataChannel and Offer
                        status_signal.set(ConnectionStatus::NegotiatingWebRtc);
                        let _ = initiate_p2p_offer(
                            &pc,
                            &room_id,
                            key,
                            data_channel_cell.clone(),
                            ws_tx.clone(),
                            status_signal,
                            messages_signal,
                            session_cell_c.clone(),
                        )
                        .await;
                    }
                    ServerMessage::Signal { payload } => {
                        // Decrypt incoming signal
                        if let Ok(signal) = decrypt_json::<SignalPayload>(&key, &payload) {
                            handle_remote_signal(
                                &pc,
                                &room_id,
                                key,
                                signal,
                                ws_tx.clone(),
                                status_signal,
                            )
                            .await;
                        } else {
                            log::warn!("Failed to decrypt signal payload with URL key");
                        }
                    }
                    ServerMessage::PeerLeft => {
                        status_signal.set(ConnectionStatus::Disconnected);
                        if let Some(ref sess) = *session_cell_c.borrow() {
                            sess.cleanup_audio();
                        }
                    }
                    ServerMessage::Error { message } => {
                        status_signal.set(ConnectionStatus::Error(message));
                    }
                    _ => {}
                }
            }
        }
    });

    Ok(session_cell)
}

async fn initiate_p2p_offer(
    pc: &RtcPeerConnection,
    room_id: &str,
    key: [u8; KEY_LENGTH],
    dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    mut ws_tx: mpsc::UnboundedSender<ClientMessage>,
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) -> Result<(), JsValue> {
    let dc_init = RtcDataChannelInit::new();
    dc_init.set_ordered(true);
    let dc = pc.create_data_channel_with_data_channel_dict("chat", &dc_init);

    attach_datachannel_callbacks(&dc, key, status_signal, messages_signal, session_cell);
    *dc_cell.borrow_mut() = Some(dc);

    let offer = wasm_bindgen_futures::JsFuture::from(pc.create_offer()).await?;
    let sdp = js_sys::Reflect::get(&offer, &"sdp".into())?
        .as_string()
        .ok_or_else(|| JsValue::from_str("No SDP in offer"))?;

    let offer_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    offer_init.set_sdp(&sdp);
    wasm_bindgen_futures::JsFuture::from(pc.set_local_description(&offer_init)).await?;

    let signal = SignalPayload::Offer { sdp };
    if let Ok(enc) = encrypt_json(&key, &signal) {
        let _ = ws_tx.start_send(ClientMessage::Signal {
            room_id: room_id.to_string(),
            payload: enc,
        });
    }

    Ok(())
}

fn setup_responder_datachannel(
    pc: &RtcPeerConnection,
    key: [u8; KEY_LENGTH],
    dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) {
    let on_dc = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
        let dc = ev.channel();
        attach_datachannel_callbacks(&dc, key, status_signal, messages_signal, session_cell.clone());
        *dc_cell.borrow_mut() = Some(dc);
    }) as Box<dyn FnMut(RtcDataChannelEvent)>);

    pc.set_ondatachannel(Some(on_dc.as_ref().unchecked_ref()));
    on_dc.forget();
}

async fn handle_remote_signal(
    pc: &RtcPeerConnection,
    room_id: &str,
    key: [u8; KEY_LENGTH],
    signal: SignalPayload,
    mut ws_tx: mpsc::UnboundedSender<ClientMessage>,
    status_signal: WriteSignal<ConnectionStatus>,
) {
    match signal {
        SignalPayload::Offer { sdp } => {
            let desc_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
            desc_init.set_sdp(&sdp);
            if wasm_bindgen_futures::JsFuture::from(pc.set_remote_description(&desc_init))
                .await
                .is_err()
            {
                status_signal.set(ConnectionStatus::Error("Failed to set remote offer".into()));
                return;
            }

            let answer = match wasm_bindgen_futures::JsFuture::from(pc.create_answer()).await {
                Ok(a) => a,
                Err(_) => {
                    status_signal.set(ConnectionStatus::Error("Failed to create answer".into()));
                    return;
                }
            };

            let answer_sdp = match js_sys::Reflect::get(&answer, &"sdp".into()) {
                Ok(val) => val.as_string().unwrap_or_default(),
                Err(_) => return,
            };

            let ans_init = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
            ans_init.set_sdp(&answer_sdp);
            if wasm_bindgen_futures::JsFuture::from(pc.set_local_description(&ans_init))
                .await
                .is_err()
            {
                status_signal.set(ConnectionStatus::Error("Failed to set local answer".into()));
                return;
            }

            let resp_signal = SignalPayload::Answer { sdp: answer_sdp };
            if let Ok(enc) = encrypt_json(&key, &resp_signal) {
                let _ = ws_tx.start_send(ClientMessage::Signal {
                    room_id: room_id.to_string(),
                    payload: enc,
                });
            }
        }
        SignalPayload::Answer { sdp } => {
            let ans_init = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
            ans_init.set_sdp(&sdp);
            let _ = wasm_bindgen_futures::JsFuture::from(pc.set_remote_description(&ans_init)).await;
        }
        SignalPayload::IceCandidate {
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            let init = RtcIceCandidateInit::new(&candidate);
            if let Some(ref mid) = sdp_mid {
                init.set_sdp_mid(Some(mid));
            }
            if let Some(idx) = sdp_m_line_index {
                init.set_sdp_m_line_index(Some(idx));
            }
            if let Ok(ice_cand) = RtcIceCandidate::new(&init) {
                let _ = wasm_bindgen_futures::JsFuture::from(
                    pc.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&ice_cand)),
                )
                .await;
            }
        }
    }
}

fn attach_datachannel_callbacks(
    dc: &RtcDataChannel,
    key: [u8; KEY_LENGTH],
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) {
    // onopen
    {
        let on_open = Closure::wrap(Box::new(move || {
            status_signal.set(ConnectionStatus::Connected);
            log::info!("RTCDataChannel Open - P2P connection active!");
        }) as Box<dyn FnMut()>);
        dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
    }

    // onclose
    {
        let on_close = Closure::wrap(Box::new(move || {
            status_signal.set(ConnectionStatus::Disconnected);
            log::info!("RTCDataChannel Closed.");
        }) as Box<dyn FnMut()>);
        dc.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    }

    // onerror
    {
        let on_error = Closure::wrap(Box::new(move |e: JsValue| {
            log::error!("RTCDataChannel error: {:?}", e);
        }) as Box<dyn FnMut(JsValue)>);
        dc.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
    }

    // onmessage
    {
        let session_c = session_cell.clone();
        let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
            if let Some(text) = ev.data().as_string() {
                if let Ok(encrypted) = serde_json::from_str::<EncryptedPayload>(&text) {
                    if let Ok(msg) = decrypt_json::<DataChannelMessage>(&key, &encrypted) {
                        match msg {
                            DataChannelMessage::Chat { id, text, .. } => {
                                let time_str = current_time_string();
                                messages_signal.update(|msgs| {
                                    msgs.push(ChatMessageUi {
                                        id,
                                        sender: "Peer".into(),
                                        is_self: false,
                                        text,
                                        time: time_str,
                                    });
                                });
                            }
                            DataChannelMessage::CallInvite => {
                                log::info!("Incoming audio call invite received from peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.call_state.set(CallState::Incoming);
                                }
                            }
                            DataChannelMessage::CallAccepted => {
                                log::info!("Peer accepted audio call! Capturing caller mic and activating stream.");
                                if let Some(ref sess) = *session_c.borrow() {
                                    let pc = sess.peer.clone();
                                    let local_stream_cell = sess.local_audio_stream.clone();
                                    let audio_senders_cell = sess.audio_senders.clone();
                                    let call_state = sess.call_state;
                                    let toast = sess.toast_signal;

                                    wasm_bindgen_futures::spawn_local(async move {
                                        match capture_microphone().await {
                                            Ok(stream) => {
                                                let tracks = stream.get_audio_tracks();
                                                let mut senders = Vec::new();
                                                for i in 0..tracks.length() {
                                                    let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                                                    let sender = pc.add_track_0(&track, &stream);
                                                    senders.push(sender);
                                                }
                                                *local_stream_cell.borrow_mut() = Some(stream);
                                                *audio_senders_cell.borrow_mut() = senders;
                                                call_state.set(CallState::Active);
                                            }
                                            Err(err) => {
                                                log::error!("Caller microphone error: {:?}", err);
                                                toast.set(Some("Microphone access failed.".into()));
                                                call_state.set(CallState::Idle);
                                            }
                                        }
                                    });
                                }
                            }
                            DataChannelMessage::CallRejected => {
                                log::info!("Peer rejected audio call");
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.call_state.set(CallState::Idle);
                                    sess.toast_signal.set(Some("Peer declined audio call".into()));
                                }
                            }
                            DataChannelMessage::CallEnded => {
                                log::info!("Audio call ended by peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.cleanup_audio();
                                    sess.toast_signal.set(Some("Audio call ended".into()));
                                }
                            }
                            DataChannelMessage::Ack { .. } => {}
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();
    }
}
