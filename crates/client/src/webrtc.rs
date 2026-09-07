use crate::state::{current_time_string, ChatMessageUi, ConnectionStatus};
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
    window, MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent,
    RtcDataChannelInit, RtcDataChannelState, RtcIceCandidate, RtcIceCandidateInit,
    RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit,
};

#[allow(dead_code)]
pub struct WebRtcSession {
    pub room_id: String,
    pub key: [u8; KEY_LENGTH],
    pub peer: RtcPeerConnection,
    pub data_channel: Rc<RefCell<Option<RtcDataChannel>>>,
    pub ws_sender: mpsc::UnboundedSender<ClientMessage>,
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

        let encrypted = encrypt_json(&self.key, &chat_msg)
            .map_err(|e| format!("Encryption error: {e}"))?;

        let json_str = serde_json::to_string(&encrypted)
            .map_err(|e| format!("Serialization error: {e}"))?;

        dc.send_with_str(&json_str)
            .map_err(|e| format!("Data channel send error: {:?}", e))?;

        // Add to local message list
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
}

pub fn start_webrtc_session(
    room_id: String,
    key: [u8; KEY_LENGTH],
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
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
    let session_cell = Rc::new(RefCell::new(None::<WebRtcSession>));

    let (ws_tx, mut ws_rx) = mpsc::unbounded::<ClientMessage>();

    // Store session
    *session_cell.borrow_mut() = Some(WebRtcSession {
        room_id: room_id.clone(),
        key,
        peer: pc.clone(),
        data_channel: data_channel_cell.clone(),
        ws_sender: ws_tx.clone(),
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
) -> Result<(), JsValue> {
    let dc_init = RtcDataChannelInit::new();
    dc_init.set_ordered(true);
    let dc = pc.create_data_channel_with_data_channel_dict("chat", &dc_init);

    attach_datachannel_callbacks(&dc, key, status_signal, messages_signal);
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
) {
    let on_dc = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
        let dc = ev.channel();
        attach_datachannel_callbacks(&dc, key, status_signal, messages_signal);
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
