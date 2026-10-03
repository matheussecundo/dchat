use crate::nostr_pool::NostrRelayPool;
use crate::state::{
    current_time_string, get_default_relays, AudioSettings, CallState, CallType, ChatMessageUi,
    ConnectionStatus, FileOfferInfo, FileTransferStatus,
};
use leptos::*;
use protocol::crypto::{decrypt_json, encrypt_json};
use protocol::{
    decrypt_chunk, encrypt_chunk, DataChannelMessage, EncryptedPayload, IceCandidateData,
    SignalPayload, CHUNK_SIZE, KEY_LENGTH,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    window, HtmlAnchorElement, HtmlAudioElement, HtmlVideoElement, MediaStream,
    MediaStreamConstraints, MediaStreamTrack, MessageEvent, RtcConfiguration, RtcDataChannel,
    RtcDataChannelEvent, RtcDataChannelInit, RtcDataChannelState, RtcDataChannelType,
    RtcIceCandidate, RtcIceCandidateInit, RtcPeerConnection, RtcPeerConnectionIceEvent,
    RtcRtpSender, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
};

#[allow(dead_code)]
pub struct IncomingTransfer {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub chunks: Vec<js_sys::Uint8Array>,
    pub writable: Option<JsValue>,
    pub start_time: f64,
    pub bytes_received: u64,
}

#[allow(dead_code)]
pub struct WebRtcSession {
    pub room_id: String,
    pub key: [u8; KEY_LENGTH],
    pub peer: RtcPeerConnection,
    pub data_channel: Rc<RefCell<Option<RtcDataChannel>>>,
    pub file_channel: Rc<RefCell<Option<RtcDataChannel>>>,
    pub nostr_pool: Rc<NostrRelayPool>,
    pub local_stream: Rc<RefCell<Option<MediaStream>>>,
    pub media_senders: Rc<RefCell<Vec<RtcRtpSender>>>,
    pub remote_audio: Rc<RefCell<Option<HtmlAudioElement>>>,
    pub remote_stream: Rc<RefCell<Option<MediaStream>>>,
    pub current_call_type: Rc<RefCell<CallType>>,
    pub is_front_camera_cell: Rc<RefCell<bool>>,
    pub audio_settings: Rc<RefCell<AudioSettings>>,
    pub audio_recapture_in_flight: Rc<RefCell<bool>>,
    pub pending_audio_settings: Rc<RefCell<Option<AudioSettings>>>,
    pub call_state_signal: WriteSignal<CallState>,
    pub is_mic_muted_signal: WriteSignal<bool>,
    pub is_video_muted_signal: WriteSignal<bool>,
    pub is_speaker_muted_signal: WriteSignal<bool>,
    pub is_front_camera_signal: WriteSignal<bool>,
    pub toast_signal: WriteSignal<Option<String>>,
    pub active_send_files: Rc<RefCell<HashMap<String, web_sys::File>>>,
    pub active_incoming_transfers: Rc<RefCell<HashMap<String, IncomingTransfer>>>,
    pub cancelled_transfers: Rc<RefCell<HashSet<String>>>,
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
                file: None,
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

    pub fn start_call(&self, call_type: CallType) {
        if call_type == CallType::None {
            return;
        }

        let pc = self.peer.clone();
        let local_stream_cell = self.local_stream.clone();
        let media_senders_cell = self.media_senders.clone();
        let current_call_type_cell = self.current_call_type.clone();
        let remote_stream_cell = self.remote_stream.clone();
        let remote_audio_cell = self.remote_audio.clone();
        let call_state_signal = self.call_state_signal;
        let is_mic_muted_signal = self.is_mic_muted_signal;
        let is_video_muted_signal = self.is_video_muted_signal;
        let is_speaker_muted_signal = self.is_speaker_muted_signal;
        let audio_settings = *self.audio_settings.borrow();
        let toast = self.toast_signal;
        let dc_msg_sender = self.clone_dc_sender();

        wasm_bindgen_futures::spawn_local(async move {
            unlock_remote_audio();

            let stream_res = match call_type {
                CallType::Audio => capture_microphone(&audio_settings).await,
                CallType::Video => capture_camera(true, Some(&audio_settings)).await,
                CallType::ScreenShare => capture_screen().await,
                CallType::None => return,
            };

            let stream = match stream_res {
                Ok(s) => s,
                Err(err) => {
                    log::error!("Media capture error: {:?}", err);
                    let err_msg = match call_type {
                        CallType::Audio => "Microphone permission denied or device not found",
                        CallType::Video => "Camera permission denied or device not found",
                        CallType::ScreenShare => "Screen share permission denied or cancelled",
                        CallType::None => "Media capture failed",
                    };
                    toast.set(Some(err_msg.into()));
                    call_state_signal.set(CallState::Idle);
                    return;
                }
            };

            // For screen share, also capture microphone if possible so presenter can speak
            if call_type == CallType::ScreenShare {
                if let Ok(mic_stream) = capture_microphone(&audio_settings).await {
                    let audio_tracks = mic_stream.get_audio_tracks();
                    for i in 0..audio_tracks.length() {
                        let track: MediaStreamTrack = audio_tracks.get(i).unchecked_into();
                        stream.add_track(&track);
                    }
                }

                // If user stops sharing via browser/system controls, cleanly end the call
                let video_tracks = stream.get_video_tracks();
                if video_tracks.length() > 0 {
                    let track: MediaStreamTrack = video_tracks.get(0).unchecked_into();
                    let cleanup_peer = pc.clone();
                    let cleanup_local = local_stream_cell.clone();
                    let cleanup_senders = media_senders_cell.clone();
                    let cleanup_remote_stream = remote_stream_cell.clone();
                    let cleanup_remote_audio = remote_audio_cell.clone();
                    let cleanup_call_type = current_call_type_cell.clone();
                    let cleanup_state = call_state_signal;
                    let cleanup_mic = is_mic_muted_signal;
                    let cleanup_vid = is_video_muted_signal;
                    let cleanup_speaker = is_speaker_muted_signal;
                    let dc_sender_for_end = dc_msg_sender.clone();

                    let on_ended = Closure::wrap(Box::new(move || {
                        log::info!("Screen share track ended by user/system");
                        let _ = dc_sender_for_end(DataChannelMessage::CallEnded);
                        perform_media_cleanup(
                            &cleanup_peer,
                            &cleanup_local,
                            &cleanup_senders,
                            &cleanup_remote_stream,
                            &cleanup_remote_audio,
                            &cleanup_call_type,
                            cleanup_state,
                            cleanup_mic,
                            cleanup_vid,
                            cleanup_speaker,
                        );
                    }) as Box<dyn FnMut()>);
                    track.set_onended(Some(on_ended.as_ref().unchecked_ref()));
                    on_ended.forget();
                }
            }

            attach_local_stream_to_dom(&stream);

            let mut senders = Vec::new();
            let tracks = stream.get_tracks();
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                let sender = pc.add_track_0(&track, &stream);
                senders.push(sender);
            }
            *local_stream_cell.borrow_mut() = Some(stream);
            *media_senders_cell.borrow_mut() = senders;

            let msg = match call_type {
                CallType::Audio => DataChannelMessage::CallInvite,
                CallType::Video => DataChannelMessage::VideoCallInvite,
                CallType::ScreenShare => DataChannelMessage::ScreenShareInvite,
                CallType::None => return,
            };

            if let Err(err) = dc_msg_sender(msg) {
                toast.set(Some(format!("Cannot initiate call: {err}")));
                call_state_signal.set(CallState::Idle);
                return;
            }

            *current_call_type_cell.borrow_mut() = call_type;
            call_state_signal.set(CallState::Calling(call_type));
        });
    }

    pub fn accept_call(&self, call_type: CallType) {
        let pc = self.peer.clone();
        let local_stream_cell = self.local_stream.clone();
        let media_senders_cell = self.media_senders.clone();
        let call_type_cell = self.current_call_type.clone();
        let call_state_signal = self.call_state_signal;
        let audio_settings = *self.audio_settings.borrow();
        let toast = self.toast_signal;
        let dc_msg_sender = self.clone_dc_sender();

        wasm_bindgen_futures::spawn_local(async move {
            unlock_remote_audio();

            let stream_res = match call_type {
                CallType::Audio => capture_microphone(&audio_settings).await,
                CallType::Video => capture_camera(true, Some(&audio_settings)).await,
                CallType::ScreenShare => capture_microphone(&audio_settings).await,
                CallType::None => return,
            };

            let stream = match stream_res {
                Ok(s) => s,
                Err(err) => {
                    log::error!("Media capture error: {:?}", err);
                    let err_msg = match call_type {
                        CallType::Audio => "Microphone permission denied or device not found",
                        CallType::Video => "Camera permission denied or device not found",
                        _ => "Media permission denied",
                    };
                    toast.set(Some(err_msg.into()));
                    let _ = dc_msg_sender(DataChannelMessage::CallRejected);
                    call_state_signal.set(CallState::Idle);
                    return;
                }
            };

            attach_local_stream_to_dom(&stream);

            let mut senders = Vec::new();
            let tracks = stream.get_tracks();
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                let sender = pc.add_track_0(&track, &stream);
                senders.push(sender);
            }
            *local_stream_cell.borrow_mut() = Some(stream);
            *media_senders_cell.borrow_mut() = senders;

            let _ = dc_msg_sender(DataChannelMessage::CallAccepted);
            *call_type_cell.borrow_mut() = call_type;
            call_state_signal.set(CallState::Active(call_type));
        });
    }

    pub fn reject_call(&self) {
        let _ = self.send_dc_message(&DataChannelMessage::CallRejected);
        self.cleanup_media();
    }

    pub fn toggle_mic_mute(&self) {
        let stream_guard = self.local_stream.borrow();
        if let Some(ref stream) = *stream_guard {
            let tracks = stream.get_audio_tracks();
            let mut new_muted = false;
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                let current = track.enabled();
                track.set_enabled(!current);
                new_muted = current;
            }
            self.is_mic_muted_signal.set(new_muted);
        }
    }

    pub fn toggle_video_mute(&self) {
        let stream_guard = self.local_stream.borrow();
        if let Some(ref stream) = *stream_guard {
            let tracks = stream.get_video_tracks();
            let mut new_muted = false;
            for i in 0..tracks.length() {
                let track: MediaStreamTrack = tracks.get(i).unchecked_into();
                let current = track.enabled();
                track.set_enabled(!current);
                new_muted = current;
            }
            self.is_video_muted_signal.set(new_muted);
        }
    }

    /// Mutes incoming peer audio locally. The peer keeps sending and is not notified.
    pub fn toggle_speaker_mute(&self) {
        if let Some(audio) = get_or_create_remote_audio() {
            let new_muted = !audio.muted();
            audio.set_muted(new_muted);
            self.is_speaker_muted_signal.set(new_muted);
        }
    }

    /// Stores new mic processing settings and, during a call, swaps in a freshly captured
    /// mic track. Changes arriving mid-swap collapse into one follow-up swap (latest wins).
    pub fn set_audio_settings(&self, settings: AudioSettings) {
        *self.audio_settings.borrow_mut() = settings;

        if *self.audio_recapture_in_flight.borrow() {
            *self.pending_audio_settings.borrow_mut() = Some(settings);
            return;
        }
        if !local_stream_has_audio(&self.local_stream) {
            return;
        }
        *self.audio_recapture_in_flight.borrow_mut() = true;

        let local_stream_cell = self.local_stream.clone();
        let media_senders_cell = self.media_senders.clone();
        let in_flight = self.audio_recapture_in_flight.clone();
        let pending = self.pending_audio_settings.clone();
        let toast = self.toast_signal;

        wasm_bindgen_futures::spawn_local(async move {
            let mut next = Some(settings);
            while let Some(s) = next {
                if let Err(err) = swap_mic_track(&local_stream_cell, &media_senders_cell, &s).await {
                    log::error!("Audio settings re-capture error: {:?}", err);
                    toast.set(Some("Could not apply audio settings.".into()));
                }
                next = pending.borrow_mut().take();
            }
            *in_flight.borrow_mut() = false;
        });
    }

    pub fn flip_camera(&self) {
        let local_stream_cell = self.local_stream.clone();
        let media_senders_cell = self.media_senders.clone();
        let is_front_cell = self.is_front_camera_cell.clone();
        let is_front_signal = self.is_front_camera_signal;
        let toast = self.toast_signal;

        let current_front = *is_front_cell.borrow();
        let new_front = !current_front;
        *is_front_cell.borrow_mut() = new_front;
        is_front_signal.set(new_front);

        wasm_bindgen_futures::spawn_local(async move {
            // Video-only: the live (sent) mic track is carried over below instead of
            // opening a second, unsent microphone.
            match capture_camera(new_front, None).await {
                Ok(new_stream) => {
                    let video_tracks = new_stream.get_video_tracks();
                    if video_tracks.length() > 0 {
                        let track_ref: MediaStreamTrack = video_tracks.get(0).unchecked_into();
                        for sender in media_senders_cell.borrow().iter() {
                            if let Some(sender_track) = sender.track() {
                                if sender_track.kind() == "video" {
                                    let _ = sender.replace_track(Some(&track_ref));
                                }
                            }
                        }
                    }

                    let old_stream = local_stream_cell.borrow_mut().take();
                    if let Some(old_stream) = old_stream {
                        let old_audio = old_stream.get_audio_tracks();
                        for i in 0..old_audio.length() {
                            let t: MediaStreamTrack = old_audio.get(i).unchecked_into();
                            new_stream.add_track(&t);
                        }
                        let old_tracks = old_stream.get_video_tracks();
                        for i in 0..old_tracks.length() {
                            let t: MediaStreamTrack = old_tracks.get(i).unchecked_into();
                            t.stop();
                        }
                    }
                    attach_local_stream_to_dom(&new_stream);
                    *local_stream_cell.borrow_mut() = Some(new_stream);
                }
                Err(err) => {
                    log::error!("Camera flip error: {:?}", err);
                    toast.set(Some("Could not switch camera.".into()));
                }
            }
        });
    }

    pub fn end_call(&self) {
        let _ = self.send_dc_message(&DataChannelMessage::CallEnded);
        self.cleanup_media();
    }

    pub fn attach_active_video_streams(&self) {
        let call_type = *self.current_call_type.borrow();
        if let Some(ref stream) = *self.local_stream.borrow() {
            attach_local_stream_to_dom(stream);
            if call_type == CallType::ScreenShare {
                let has_remote_video = self
                    .remote_stream
                    .borrow()
                    .as_ref()
                    .map(|s| s.get_video_tracks().length() > 0)
                    .unwrap_or(false);
                if !has_remote_video {
                    attach_remote_stream_to_dom(stream);
                }
            }
        }
        if let Some(ref r_stream) = *self.remote_stream.borrow() {
            if r_stream.get_video_tracks().length() > 0 {
                attach_remote_stream_to_dom(r_stream);
            }
        }
    }

    pub fn cleanup_media(&self) {
        perform_media_cleanup(
            &self.peer,
            &self.local_stream,
            &self.media_senders,
            &self.remote_stream,
            &self.remote_audio,
            &self.current_call_type,
            self.call_state_signal,
            self.is_mic_muted_signal,
            self.is_video_muted_signal,
            self.is_speaker_muted_signal,
        );
    }

    pub fn send_file_offer(
        &self,
        file: web_sys::File,
        caption: Option<String>,
        messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    ) -> Result<String, String> {
        let dc_guard = self.data_channel.borrow();
        let dc = dc_guard.as_ref().ok_or("Data channel is not open yet")?;
        if dc.ready_state() != RtcDataChannelState::Open {
            return Err("Data channel is not yet connected".into());
        }

        let file_id = uuid::Uuid::new_v4().to_string();
        let name = file.name();
        let size = file.size() as u64;
        let mime_type = file.type_();
        let timestamp = js_sys::Date::now() as u64;

        self.active_send_files.borrow_mut().insert(file_id.clone(), file);

        let offer_msg = DataChannelMessage::FileOffer {
            id: file_id.clone(),
            sender: "Self".into(),
            name: name.clone(),
            size,
            mime_type: mime_type.clone(),
            caption: caption.clone(),
            timestamp,
        };

        self.send_dc_message(&offer_msg)?;

        let time_str = current_time_string();
        messages_signal.update(|msgs| {
            msgs.push(ChatMessageUi {
                id: file_id.clone(),
                sender: "You".into(),
                is_self: true,
                text: caption.unwrap_or_default(),
                time: time_str,
                file: Some(FileOfferInfo {
                    file_id: file_id.clone(),
                    name,
                    size,
                    mime_type,
                    status: FileTransferStatus::Offered,
                }),
            });
        });

        Ok(file_id)
    }

    pub fn request_file_download(
        &self,
        file_id: &str,
        messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    ) {
        let mut file_info = None;
        messages_signal.update(|msgs| {
            if let Some(msg) = msgs.iter().find(|m| m.id == file_id) {
                if let Some(ref f) = msg.file {
                    file_info = Some((f.name.clone(), f.size, f.mime_type.clone()));
                }
            }
        });
        let (name, size, mime_type) = match file_info {
            Some(info) => info,
            None => return,
        };

        let file_id_str = file_id.to_string();
        let session_dc_sender = self.clone_dc_sender();
        let active_incoming = self.active_incoming_transfers.clone();
        let toast = self.toast_signal;

        wasm_bindgen_futures::spawn_local(async move {
            let writable = try_open_save_stream(&name).await;

            active_incoming.borrow_mut().insert(
                file_id_str.clone(),
                IncomingTransfer {
                    file_id: file_id_str.clone(),
                    name: name.clone(),
                    size,
                    mime_type,
                    chunks: Vec::new(),
                    writable,
                    start_time: js_sys::Date::now(),
                    bytes_received: 0,
                },
            );

            messages_signal.update(|msgs| {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id_str) {
                    if let Some(ref mut f) = msg.file {
                        f.status = FileTransferStatus::Downloading { progress: 0, speed_kb: 0 };
                    }
                }
            });

            if let Err(err) = session_dc_sender(DataChannelMessage::FileRequest { id: file_id_str }) {
                toast.set(Some(format!("Failed to request file: {}", err)));
            }
        });
    }

    pub fn start_file_upload(
        &self,
        file_id: String,
        messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    ) {
        let file = match self.active_send_files.borrow().get(&file_id).cloned() {
            Some(f) => f,
            None => {
                log::warn!("FileRequest for unknown file_id: {}", file_id);
                return;
            }
        };

        let file_dc_cell = self.file_channel.clone();
        let cancelled = self.cancelled_transfers.clone();
        let key = self.key;
        let toast = self.toast_signal;
        let dc_sender = self.clone_dc_sender();

        messages_signal.update(|msgs| {
            if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                if let Some(ref mut f) = msg.file {
                    f.status = FileTransferStatus::Downloading { progress: 0, speed_kb: 0 };
                }
            }
        });

        wasm_bindgen_futures::spawn_local(async move {
            let total_size = file.size() as f64;
            let chunk_size = CHUNK_SIZE as f64;
            let total_chunks = if total_size == 0.0 {
                1
            } else {
                (total_size / chunk_size).ceil() as u32
            };

            let file_uuid = match uuid::Uuid::parse_str(&file_id) {
                Ok(u) => u,
                Err(_) => uuid::Uuid::new_v4(),
            };
            let file_id_bytes = *file_uuid.as_bytes();

            let start_time = js_sys::Date::now();
            let mut bytes_sent: u64 = 0;

            for chunk_idx in 0..total_chunks {
                if cancelled.borrow().contains(&file_id) {
                    log::info!("File transfer {} cancelled by sender", file_id);
                    return;
                }

                // Check backpressure on file_channel
                loop {
                    let buffered = file_dc_cell.borrow().as_ref().map(|dc| dc.buffered_amount()).unwrap_or(0);
                    if buffered < 512 * 1024 {
                        break;
                    }
                    sleep_ms(15).await;
                }

                let start = (chunk_idx as f64) * chunk_size;
                let end = (start + chunk_size).min(total_size);
                let blob = match file.slice_with_f64_and_f64(start, end) {
                    Ok(b) => b,
                    Err(err) => {
                        log::error!("Slice error: {:?}", err);
                        return;
                    }
                };

                let array_buf_promise = blob.array_buffer();
                let array_buf = match wasm_bindgen_futures::JsFuture::from(array_buf_promise).await {
                    Ok(ab) => ab.unchecked_into::<js_sys::ArrayBuffer>(),
                    Err(err) => {
                        log::error!("ArrayBuffer error: {:?}", err);
                        return;
                    }
                };

                let u8_arr = js_sys::Uint8Array::new(&array_buf);
                let slice_bytes = u8_arr.to_vec();
                bytes_sent += slice_bytes.len() as u64;

                let packet = match encrypt_chunk(&key, &file_id_bytes, chunk_idx, total_chunks, &slice_bytes) {
                    Ok(p) => p,
                    Err(err) => {
                        log::error!("Encrypt chunk error: {:?}", err);
                        return;
                    }
                };

                if let Some(ref dc) = *file_dc_cell.borrow() {
                    if let Err(err) = dc.send_with_u8_array(&packet) {
                        log::error!("Data channel send error: {:?}", err);
                        return;
                    }
                }

                let elapsed_sec = (js_sys::Date::now() - start_time) / 1000.0;
                let speed_kb = if elapsed_sec > 0.0 {
                    ((bytes_sent as f64 / 1024.0) / elapsed_sec) as u64
                } else {
                    0
                };
                let progress = (((chunk_idx + 1) as f64 / total_chunks as f64) * 100.0) as u8;

                messages_signal.update(|msgs| {
                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                        if let Some(ref mut f) = msg.file {
                            f.status = FileTransferStatus::Downloading { progress, speed_kb };
                        }
                    }
                });
            }

            let _ = dc_sender(DataChannelMessage::FileComplete { id: file_id.clone() });
            messages_signal.update(|msgs| {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                    if let Some(ref mut f) = msg.file {
                        f.status = FileTransferStatus::Completed;
                    }
                }
            });
            toast.set(Some("✅ File upload complete".into()));
        });
    }

    pub fn cancel_file_transfer(&self, file_id: &str, messages_signal: WriteSignal<Vec<ChatMessageUi>>) {
        self.cancelled_transfers.borrow_mut().insert(file_id.to_string());
        let _ = self.send_dc_message(&DataChannelMessage::FileCancel {
            id: file_id.to_string(),
            reason: "Cancelled by user".into(),
        });

        if let Some(transfer) = self.active_incoming_transfers.borrow_mut().remove(file_id) {
            if let Some(ref w) = transfer.writable {
                let w_clone = w.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = abort_save_stream(&w_clone).await;
                });
            }
        }

        self.active_send_files.borrow_mut().remove(file_id);

        messages_signal.update(|msgs| {
            if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                if let Some(ref mut f) = msg.file {
                    f.status = FileTransferStatus::Cancelled { reason: "Cancelled".into() };
                }
            }
        });
    }

    pub fn handle_incoming_chunk(
        &self,
        file_id: String,
        chunk_index: u32,
        total_chunks: u32,
        plaintext: Vec<u8>,
        messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    ) {
        if self.cancelled_transfers.borrow().contains(&file_id) {
            return;
        }

        let mut incoming_map = self.active_incoming_transfers.borrow_mut();
        if let Some(transfer) = incoming_map.get_mut(&file_id) {
            transfer.bytes_received += plaintext.len() as u64;
            let elapsed_sec = (js_sys::Date::now() - transfer.start_time) / 1000.0;
            let speed_kb = if elapsed_sec > 0.0 {
                ((transfer.bytes_received as f64 / 1024.0) / elapsed_sec) as u64
            } else {
                0
            };
            let progress = (((chunk_index + 1) as f64 / total_chunks as f64) * 100.0) as u8;

            if let Some(ref writable) = transfer.writable {
                let writable_clone = writable.clone();
                let plaintext_c = plaintext.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = write_chunk_to_stream(&writable_clone, &plaintext_c).await;
                });
            } else {
                transfer.chunks.push(js_sys::Uint8Array::from(&plaintext[..]));
            }

            let is_last_chunk = chunk_index + 1 >= total_chunks;
            let name = transfer.name.clone();
            let mime = transfer.mime_type.clone();
            let writable_opt = transfer.writable.clone();
            let chunks_clone = if is_last_chunk { transfer.chunks.clone() } else { Vec::new() };

            if is_last_chunk {
                let toast = self.toast_signal;
                let file_id_c = file_id.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Some(ref w) = writable_opt {
                        let _ = close_save_stream(w).await;
                    } else {
                        let _ = trigger_blob_download(&name, &mime, &chunks_clone);
                    }
                    messages_signal.update(|msgs| {
                        if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id_c) {
                            if let Some(ref mut f) = msg.file {
                                f.status = FileTransferStatus::Completed;
                            }
                        }
                    });
                    toast.set(Some(format!("✅ Downloaded {}", name)));
                });
                incoming_map.remove(&file_id);
            } else {
                messages_signal.update(|msgs| {
                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                        if let Some(ref mut f) = msg.file {
                            f.status = FileTransferStatus::Downloading { progress, speed_kb };
                        }
                    }
                });
            }
        }
    }

    pub fn cleanup_file_transfers(&self, messages_signal: WriteSignal<Vec<ChatMessageUi>>) {
        let mut incoming = self.active_incoming_transfers.borrow_mut();
        for (_, transfer) in incoming.drain() {
            if let Some(ref w) = transfer.writable {
                let w_c = w.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = abort_save_stream(&w_c).await;
                });
            }
        }
        self.active_send_files.borrow_mut().clear();
        messages_signal.update(|msgs| {
            for msg in msgs.iter_mut() {
                if let Some(ref mut f) = msg.file {
                    if matches!(f.status, FileTransferStatus::Downloading { .. } | FileTransferStatus::Offered) {
                        f.status = FileTransferStatus::Interrupted;
                    }
                }
            }
        });
    }

    fn clone_dc_sender(&self) -> Rc<dyn Fn(DataChannelMessage) -> Result<(), String>> {
        let dc_cell = self.data_channel.clone();
        let key = self.key;
        Rc::new(move |msg| {
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

/// Single place where mic processing is requested; a future in-app denoiser
/// (e.g. RNNoise) would hook in alongside this.
fn build_audio_constraints(settings: &AudioSettings) -> Result<JsValue, JsValue> {
    // Plain booleans are "ideal" constraints: an unsupported switch never makes
    // getUserMedia fail with OverconstrainedError.
    let audio_opts = js_sys::Object::new();
    js_sys::Reflect::set(&audio_opts, &"noiseSuppression".into(), &settings.noise_suppression.into())?;
    js_sys::Reflect::set(&audio_opts, &"echoCancellation".into(), &settings.echo_cancellation.into())?;
    js_sys::Reflect::set(&audio_opts, &"autoGainControl".into(), &settings.auto_gain_control.into())?;
    Ok(audio_opts.into())
}

/// Which mic processing switches this browser recognizes, as
/// (noise suppression, echo cancellation, auto gain control).
pub fn supported_audio_constraints() -> (bool, bool, bool) {
    let supported = window()
        .and_then(|w| w.navigator().media_devices().ok())
        .and_then(|md| {
            let f = js_sys::Reflect::get(&md, &"getSupportedConstraints".into()).ok()?;
            let f: js_sys::Function = f.dyn_into().ok()?;
            f.call0(&md).ok()
        });
    let has = |key: &str| {
        supported
            .as_ref()
            .and_then(|s| js_sys::Reflect::get(s, &key.into()).ok())
            .map(|v| v.is_truthy())
            .unwrap_or(false)
    };
    (has("noiseSuppression"), has("echoCancellation"), has("autoGainControl"))
}

pub async fn capture_microphone(settings: &AudioSettings) -> Result<MediaStream, JsValue> {
    let win = window().ok_or_else(|| JsValue::from_str("No window"))?;
    let nav = win.navigator();
    let media_devices = nav.media_devices()?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&build_audio_constraints(settings)?);
    constraints.set_video(&JsValue::from_bool(false));
    let promise = media_devices.get_user_media_with_constraints(&constraints)?;
    let js_stream = wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(js_stream.unchecked_into())
}

/// Captures the camera, plus the mic when `audio` is given.
pub async fn capture_camera(front: bool, audio: Option<&AudioSettings>) -> Result<MediaStream, JsValue> {
    let win = window().ok_or_else(|| JsValue::from_str("No window"))?;
    let nav = win.navigator();
    let media_devices = nav.media_devices()?;
    let constraints = MediaStreamConstraints::new();
    match audio {
        Some(settings) => constraints.set_audio(&build_audio_constraints(settings)?),
        None => constraints.set_audio(&JsValue::from_bool(false)),
    }

    let video_opts = js_sys::Object::new();
    let facing = if front { "user" } else { "environment" };
    js_sys::Reflect::set(&video_opts, &"facingMode".into(), &facing.into())?;
    constraints.set_video(&video_opts);

    let promise = media_devices.get_user_media_with_constraints(&constraints)?;
    let js_stream = wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(js_stream.unchecked_into())
}

pub async fn capture_screen() -> Result<MediaStream, JsValue> {
    let win = window().ok_or_else(|| JsValue::from_str("No window"))?;
    let nav = win.navigator();
    let media_devices = nav.media_devices()?;
    let promise = media_devices.get_display_media()?;
    let js_stream = wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(js_stream.unchecked_into())
}

fn try_attach_video(element_id: &str, stream: &MediaStream) -> bool {
    if let Some(win) = window() {
        if let Some(doc) = win.document() {
            if let Some(el) = doc.get_element_by_id(element_id) {
                if let Ok(video) = el.dyn_into::<HtmlVideoElement>() {
                    video.set_muted(true);
                    let should_set = match video.src_object() {
                        Some(ref cur) => cur.id() != stream.id(),
                        None => true,
                    };
                    if should_set {
                        video.set_src_object(Some(stream));
                    }
                    let _ = video.play();
                    return true;
                }
            }
        }
    }
    false
}

pub fn attach_local_stream_to_dom(stream: &MediaStream) {
    if stream.get_video_tracks().length() == 0 {
        return;
    }
    // `Clone::clone` copies the JS handle; the inherent `MediaStream::clone` would call
    // JS `clone()`, duplicating tracks that then hold the mic open with stale settings.
    let stream_clone = Clone::clone(stream);
    if try_attach_video("local-video-preview", &stream_clone) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..15 {
            sleep_ms(30).await;
            if try_attach_video("local-video-preview", &stream_clone) {
                break;
            }
        }
    });
}

pub fn attach_remote_stream_to_dom(stream: &MediaStream) {
    if stream.get_video_tracks().length() == 0 {
        return;
    }
    // Handle copy, not JS `clone()`: screen-share shows the local stream here too.
    let stream_clone = Clone::clone(stream);
    if try_attach_video("remote-video-feed", &stream_clone) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..15 {
            sleep_ms(30).await;
            if try_attach_video("remote-video-feed", &stream_clone) {
                break;
            }
        }
    });
}

fn local_stream_has_audio(local_stream: &RefCell<Option<MediaStream>>) -> bool {
    local_stream
        .borrow()
        .as_ref()
        .map(|s| s.get_audio_tracks().length() > 0)
        .unwrap_or(false)
}

/// Replaces the sent mic track with one captured using `settings`, keeping its
/// mute state. No renegotiation: only the sender's track is swapped.
async fn swap_mic_track(
    local_stream: &RefCell<Option<MediaStream>>,
    media_senders: &RefCell<Vec<RtcRtpSender>>,
    settings: &AudioSettings,
) -> Result<(), JsValue> {
    let old_track: MediaStreamTrack = match local_stream.borrow().as_ref() {
        Some(stream) if stream.get_audio_tracks().length() > 0 => {
            stream.get_audio_tracks().get(0).unchecked_into()
        }
        // The call ended while a previous swap was in flight.
        _ => return Ok(()),
    };
    let was_enabled = old_track.enabled();
    // Chrome hands a new capture the processing of an already-open mic source,
    // so the old track must be stopped first; the peer hears a short gap.
    old_track.stop();

    let new_stream = capture_microphone(settings).await?;
    let new_track: MediaStreamTrack = new_stream
        .get_audio_tracks()
        .get(0)
        .dyn_into()
        .map_err(|_| JsValue::from_str("No audio track captured"))?;
    new_track.set_enabled(was_enabled);

    let audio_senders: Vec<RtcRtpSender> = media_senders
        .borrow()
        .iter()
        .filter(|s| s.track().map(|t| t.kind() == "audio").unwrap_or(false))
        .cloned()
        .collect();
    for sender in audio_senders {
        if let Err(err) =
            wasm_bindgen_futures::JsFuture::from(sender.replace_track(Some(&new_track))).await
        {
            new_track.stop();
            return Err(err);
        }
    }

    match local_stream.borrow().as_ref() {
        Some(stream) => {
            stream.remove_track(&old_track);
            stream.add_track(&new_track);
        }
        None => new_track.stop(),
    }
    Ok(())
}

pub fn perform_media_cleanup(
    peer: &RtcPeerConnection,
    local_stream: &RefCell<Option<MediaStream>>,
    media_senders: &RefCell<Vec<RtcRtpSender>>,
    remote_stream: &RefCell<Option<MediaStream>>,
    remote_audio: &RefCell<Option<HtmlAudioElement>>,
    current_call_type: &RefCell<CallType>,
    call_state_signal: WriteSignal<CallState>,
    is_mic_muted_signal: WriteSignal<bool>,
    is_video_muted_signal: WriteSignal<bool>,
    is_speaker_muted_signal: WriteSignal<bool>,
) {
    if let Some(stream) = local_stream.borrow_mut().take() {
        let tracks = stream.get_tracks();
        for i in 0..tracks.length() {
            let track: MediaStreamTrack = tracks.get(i).unchecked_into();
            track.stop();
        }
    }

    for sender in media_senders.borrow_mut().drain(..) {
        let _ = peer.remove_track(&sender);
    }

    remote_stream.borrow_mut().take();

    if let Some(audio) = remote_audio.borrow_mut().take() {
        let _ = audio.pause();
        audio.set_src_object(None);
    }

    detach_streams_from_dom();

    *current_call_type.borrow_mut() = CallType::None;
    is_mic_muted_signal.set(false);
    is_video_muted_signal.set(false);
    is_speaker_muted_signal.set(false);
    call_state_signal.set(CallState::Idle);
}

pub fn get_or_create_remote_audio() -> Option<HtmlAudioElement> {
    if let Some(win) = window() {
        if let Some(doc) = win.document() {
            if let Some(el) = doc.get_element_by_id("remote-audio") {
                if let Ok(audio) = el.dyn_into::<HtmlAudioElement>() {
                    return Some(audio);
                }
            }
            if let Ok(el) = doc.create_element("audio") {
                let _ = el.set_attribute("id", "remote-audio");
                let _ = el.set_attribute("autoplay", "true");
                let _ = el.set_attribute("playsinline", "true");
                let _ = el.set_attribute("style", "display: none;");
                if let Some(body) = doc.body() {
                    let _ = body.append_child(&el);
                }
                if let Ok(audio) = el.dyn_into::<HtmlAudioElement>() {
                    return Some(audio);
                }
            }
        }
    }
    None
}

pub fn unlock_remote_audio() {
    if let Some(audio) = get_or_create_remote_audio() {
        let _ = audio.play();
    }
}

pub fn attach_remote_audio_to_dom(stream: &MediaStream, toast: WriteSignal<Option<String>>) {
    if let Some(audio) = get_or_create_remote_audio() {
        audio.set_src_object(Some(stream));
        audio.set_autoplay(true);
        match audio.play() {
            Ok(promise) => {
                wasm_bindgen_futures::spawn_local(async move {
                    if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                        log::warn!("Audio autoplay blocked by browser policy");
                        toast.set(Some("🔊 Tap anywhere to enable audio".into()));
                    }
                });
            }
            Err(_) => {
                toast.set(Some("🔊 Tap anywhere to enable audio".into()));
            }
        }
    }
}

fn detach_streams_from_dom() {
    if let Some(win) = window() {
        if let Some(doc) = win.document() {
            if let Some(el) = doc.get_element_by_id("local-video-preview") {
                if let Ok(video) = el.dyn_into::<HtmlVideoElement>() {
                    video.set_src_object(None);
                }
            }
            if let Some(el) = doc.get_element_by_id("remote-video-feed") {
                if let Ok(video) = el.dyn_into::<HtmlVideoElement>() {
                    video.set_src_object(None);
                }
            }
            if let Some(el) = doc.get_element_by_id("remote-audio") {
                if let Ok(audio) = el.dyn_into::<HtmlAudioElement>() {
                    let _ = audio.pause();
                    audio.set_src_object(None);
                    audio.set_muted(false);
                }
            }
        }
    }
}

async fn try_open_save_stream(suggested_name: &str) -> Option<JsValue> {
    let win = window()?;
    let picker_fn = js_sys::Reflect::get(&win, &"showSaveFilePicker".into()).ok()?;
    if picker_fn.is_undefined() || picker_fn.is_null() {
        return None;
    }
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"suggestedName".into(), &suggested_name.into());
    let picker_func: js_sys::Function = picker_fn.dyn_into().ok()?;
    let promise: js_sys::Promise = picker_func.call1(&win, &opts).ok()?.dyn_into().ok()?;
    let file_handle = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    let create_writable_fn: js_sys::Function = js_sys::Reflect::get(&file_handle, &"createWritable".into()).ok()?.dyn_into().ok()?;
    let writable_promise: js_sys::Promise = create_writable_fn.call0(&file_handle).ok()?.dyn_into().ok()?;
    let writable = wasm_bindgen_futures::JsFuture::from(writable_promise).await.ok()?;
    Some(writable)
}

async fn write_chunk_to_stream(writable: &JsValue, chunk: &[u8]) -> Result<(), JsValue> {
    let write_fn: js_sys::Function = js_sys::Reflect::get(writable, &"write".into())?.dyn_into()?;
    let u8_arr = js_sys::Uint8Array::from(chunk);
    let promise: js_sys::Promise = write_fn.call1(writable, &u8_arr)?.dyn_into()?;
    wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(())
}

async fn close_save_stream(writable: &JsValue) -> Result<(), JsValue> {
    let close_fn: js_sys::Function = js_sys::Reflect::get(writable, &"close".into())?.dyn_into()?;
    let promise: js_sys::Promise = close_fn.call0(writable)?.dyn_into()?;
    wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(())
}

async fn abort_save_stream(writable: &JsValue) -> Result<(), JsValue> {
    let abort_fn: js_sys::Function = js_sys::Reflect::get(writable, &"abort".into())?.dyn_into()?;
    let promise: js_sys::Promise = abort_fn.call0(writable)?.dyn_into()?;
    wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(())
}

fn trigger_blob_download(name: &str, mime_type: &str, chunks: &[js_sys::Uint8Array]) -> Result<(), JsValue> {
    let parts = js_sys::Array::new();
    for c in chunks {
        parts.push(c);
    }
    let prop_bag = web_sys::BlobPropertyBag::new();
    let effective_mime = if mime_type.is_empty() { "application/octet-stream" } else { mime_type };
    prop_bag.set_type(effective_mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &prop_bag)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    if let Some(win) = window() {
        if let Some(doc) = win.document() {
            if let Ok(el) = doc.create_element("a") {
                if let Ok(a) = el.dyn_into::<HtmlAnchorElement>() {
                    a.set_href(&url);
                    a.set_download(name);
                    if let Some(body) = doc.body() {
                        let _ = body.append_child(&a);
                        a.click();
                        let _ = body.remove_child(&a);
                    }
                }
            }
        }
        let url_c = url.clone();
        let closure = Closure::once(move || {
            let _ = web_sys::Url::revoke_object_url(&url_c);
        });
        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(closure.as_ref().unchecked_ref(), 10000);
        closure.forget();
    }
    Ok(())
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

fn attach_file_datachannel_callbacks(
    dc: &RtcDataChannel,
    key: [u8; KEY_LENGTH],
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) {
    let session_c = session_cell.clone();
    let on_message = Closure::wrap(Box::new(move |ev: MessageEvent| {
        let data = ev.data();
        if let Ok(ab) = data.dyn_into::<js_sys::ArrayBuffer>() {
            let uint8 = js_sys::Uint8Array::new(&ab);
            let packet = uint8.to_vec();
            if let Ok((header, plaintext)) = decrypt_chunk(&key, &packet) {
                let file_id_str = match uuid::Uuid::from_slice(&header.file_id) {
                    Ok(u) => u.to_string(),
                    Err(_) => return,
                };
                if let Some(ref sess) = *session_c.borrow() {
                    sess.handle_incoming_chunk(
                        file_id_str,
                        header.chunk_index,
                        header.total_chunks,
                        plaintext,
                        messages_signal,
                    );
                }
            } else {
                log::warn!("Failed to decrypt chunk packet");
            }
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
}

pub fn start_webrtc_session(
    room_id: String,
    key: [u8; KEY_LENGTH],
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    call_state_signal: WriteSignal<CallState>,
    is_mic_muted_signal: WriteSignal<bool>,
    is_video_muted_signal: WriteSignal<bool>,
    is_speaker_muted_signal: WriteSignal<bool>,
    is_front_camera_signal: WriteSignal<bool>,
    toast_signal: WriteSignal<Option<String>>,
    connected_relays_signal: WriteSignal<usize>,
) -> Result<Rc<RefCell<Option<WebRtcSession>>>, JsValue> {
    status_signal.set(ConnectionStatus::ConnectingRelay);

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
    let file_channel_cell = Rc::new(RefCell::new(None::<RtcDataChannel>));
    let active_send_files = Rc::new(RefCell::new(HashMap::<String, web_sys::File>::new()));
    let active_incoming_transfers = Rc::new(RefCell::new(HashMap::<String, IncomingTransfer>::new()));
    let cancelled_transfers = Rc::new(RefCell::new(HashSet::<String>::new()));
    let local_stream = Rc::new(RefCell::new(None::<MediaStream>));
    let media_senders = Rc::new(RefCell::new(Vec::<RtcRtpSender>::new()));
    let remote_audio = Rc::new(RefCell::new(None::<HtmlAudioElement>));
    let remote_stream_cell = Rc::new(RefCell::new(None::<MediaStream>));
    let current_call_type = Rc::new(RefCell::new(CallType::None));
    let is_front_camera_cell = Rc::new(RefCell::new(true));
    let session_cell = Rc::new(RefCell::new(None::<WebRtcSession>));

    // Peer negotiation tracking state
    let is_negotiating = Rc::new(RefCell::new(false));
    let has_connected = Rc::new(RefCell::new(false));
    let remote_peer_pubkey = Rc::new(RefCell::new(None::<String>));
    let pool_cell = Rc::new(RefCell::new(None::<Rc<NostrRelayPool>>));

    // Setup incoming track listener for remote audio/video
    {
        let remote_audio_cell = remote_audio.clone();
        let remote_stream_c = remote_stream_cell.clone();
        let toast = toast_signal;
        let on_track = Closure::wrap(Box::new(move |ev: RtcTrackEvent| {
            let track = ev.track();
            let streams = ev.streams();

            let r_stream: MediaStream = if streams.length() > 0 {
                streams.get(0).unchecked_into()
            } else {
                let arr = js_sys::Array::new();
                arr.push(&track);
                match MediaStream::new_with_tracks(&arr) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("Failed to create MediaStream from track: {:?}", e);
                        return;
                    }
                }
            };

            let existing_opt = remote_stream_c.borrow().clone();
            let final_stream = if let Some(existing) = existing_opt {
                let existing_tracks = existing.get_tracks();
                let mut has_track = false;
                for i in 0..existing_tracks.length() {
                    let t: MediaStreamTrack = existing_tracks.get(i).unchecked_into();
                    if t.id() == track.id() {
                        has_track = true;
                        break;
                    }
                }
                if !has_track {
                    existing.add_track(&track);
                }
                existing
            } else {
                *remote_stream_c.borrow_mut() = Some(r_stream.clone());
                r_stream
            };

            if track.kind() == "video" || final_stream.get_video_tracks().length() > 0 {
                attach_remote_stream_to_dom(&final_stream);
            }

            if track.kind() == "audio" || final_stream.get_audio_tracks().length() > 0 {
                attach_remote_audio_to_dom(&final_stream, toast);
                if let Some(audio) = get_or_create_remote_audio() {
                    *remote_audio_cell.borrow_mut() = Some(audio);
                }
            }
            log::info!("Remote media track received ({}) and attached", track.kind());
        }) as Box<dyn FnMut(RtcTrackEvent)>);
        pc.set_ontrack(Some(on_track.as_ref().unchecked_ref()));
        on_track.forget();
    }

    // Build incoming Nostr signal router callback
    let on_signal: Rc<dyn Fn(String, SignalPayload)> = {
        let pc = pc.clone();
        let dc_cell = data_channel_cell.clone();
        let file_dc_cell = file_channel_cell.clone();
        let is_negotiating = is_negotiating.clone();
        let has_connected = has_connected.clone();
        let remote_peer_pubkey = remote_peer_pubkey.clone();
        let pool_cell = pool_cell.clone();
        let session_cell = session_cell.clone();

        Rc::new(move |sender_pubkey: String, signal: SignalPayload| {
            let pool = match pool_cell.borrow().as_ref() {
                Some(p) => p.clone(),
                None => return,
            };

            match signal {
                SignalPayload::Presence => {
                    if !*has_connected.borrow() && !*is_negotiating.borrow() {
                        *remote_peer_pubkey.borrow_mut() = Some(sender_pubkey.clone());
                        let self_pk = pool.self_pubkey();
                        if self_pk < sender_pubkey.as_str() {
                            // Initiator (lower pubkey)
                            *is_negotiating.borrow_mut() = true;
                            status_signal.set(ConnectionStatus::NegotiatingWebRtc);
                            let pc = pc.clone();
                            let dc_cell = dc_cell.clone();
                            let file_dc_cell = file_dc_cell.clone();
                            let pool = pool.clone();
                            let session_cell = session_cell.clone();
                            wasm_bindgen_futures::spawn_local(async move {
                                let _ = initiate_p2p_offer(
                                    &pc,
                                    &pool,
                                    dc_cell,
                                    file_dc_cell,
                                    status_signal,
                                    messages_signal,
                                    session_cell,
                                )
                                .await;
                            });
                        } else {
                            // Responder (higher pubkey)
                            *is_negotiating.borrow_mut() = true;
                            status_signal.set(ConnectionStatus::NegotiatingWebRtc);
                            setup_responder_datachannel(
                                &pc,
                                key,
                                dc_cell.clone(),
                                file_dc_cell.clone(),
                                status_signal,
                                messages_signal,
                                session_cell.clone(),
                            );
                            pool.broadcast_signal(&SignalPayload::Presence);
                        }
                    }
                }
                SignalPayload::Offer { sdp } => {
                    *remote_peer_pubkey.borrow_mut() = Some(sender_pubkey.clone());
                    let is_initial = dc_cell.borrow().is_none();
                    if is_initial {
                        if !*is_negotiating.borrow() {
                            *is_negotiating.borrow_mut() = true;
                            status_signal.set(ConnectionStatus::NegotiatingWebRtc);
                            setup_responder_datachannel(
                                &pc,
                                key,
                                dc_cell.clone(),
                                file_dc_cell.clone(),
                                status_signal,
                                messages_signal,
                                session_cell.clone(),
                            );
                        }
                    }
                    let pc = pc.clone();
                    let pool = pool.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        handle_remote_offer(&pc, sdp, &pool, is_initial, status_signal).await;
                    });
                }
                SignalPayload::Answer { sdp } => {
                    let pc = pc.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        handle_remote_answer(&pc, sdp).await;
                    });
                }
                SignalPayload::IceCandidate { candidate, sdp_mid, sdp_m_line_index } => {
                    let pc = pc.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        add_single_ice_candidate(&pc, candidate, sdp_mid, sdp_m_line_index).await;
                    });
                }
                SignalPayload::IceBatch { candidates } => {
                    let pc = pc.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        add_batch_ice_candidates(&pc, candidates).await;
                    });
                }
                SignalPayload::PeerLeft => {
                    status_signal.set(ConnectionStatus::Disconnected);
                    if let Some(ref sess) = *session_cell.borrow() {
                        sess.cleanup_media();
                        sess.cleanup_file_transfers(messages_signal);
                    }
                }
            }
        })
    };

    let on_relay_connected: Rc<dyn Fn(usize)> = {
        let status_signal = status_signal;
        let connected_relays_signal = connected_relays_signal;
        Rc::new(move |count: usize| {
            log::info!("Nostr relays connected: {}", count);
            connected_relays_signal.set(count);
            if count > 0 {
                status_signal.update(|val| {
                    if *val == ConnectionStatus::ConnectingRelay {
                        *val = ConnectionStatus::WaitingForPeer;
                    }
                });
            }
        })
    };

    let relays = get_default_relays();
    let nostr_pool = match NostrRelayPool::new(
        room_id.clone(),
        key,
        relays,
        on_signal,
        on_relay_connected,
    ) {
        Ok(p) => p,
        Err(err) => {
            status_signal.set(ConnectionStatus::Error(format!("Nostr error: {err}")));
            return Err(JsValue::from_str(&err));
        }
    };
    *pool_cell.borrow_mut() = Some(nostr_pool.clone());

    // Store session
    *session_cell.borrow_mut() = Some(WebRtcSession {
        room_id: room_id.clone(),
        key,
        peer: pc.clone(),
        data_channel: data_channel_cell.clone(),
        file_channel: file_channel_cell.clone(),
        nostr_pool: nostr_pool.clone(),
        local_stream: local_stream.clone(),
        media_senders: media_senders.clone(),
        remote_audio: remote_audio.clone(),
        remote_stream: remote_stream_cell.clone(),
        current_call_type: current_call_type.clone(),
        is_front_camera_cell: is_front_camera_cell.clone(),
        audio_settings: Rc::new(RefCell::new(AudioSettings::default())),
        audio_recapture_in_flight: Rc::new(RefCell::new(false)),
        pending_audio_settings: Rc::new(RefCell::new(None)),
        call_state_signal,
        is_mic_muted_signal,
        is_video_muted_signal,
        is_speaker_muted_signal,
        is_front_camera_signal,
        toast_signal,
        active_send_files: active_send_files.clone(),
        active_incoming_transfers: active_incoming_transfers.clone(),
        cancelled_transfers: cancelled_transfers.clone(),
    });

    // Handle incoming ICE candidates on PC with 100ms debounce batching
    {
        let pool_c = nostr_pool.clone();
        let ice_batch = Rc::new(RefCell::new(Vec::<IceCandidateData>::new()));
        let timeout_handle = Rc::new(RefCell::new(None::<i32>));

        let on_ice = Closure::wrap(Box::new(move |ev: RtcPeerConnectionIceEvent| {
            let pool = pool_c.clone();
            let batch_ref = ice_batch.clone();
            let timer_ref = timeout_handle.clone();

            if let Some(candidate) = ev.candidate() {
                let cand_str = candidate.candidate();
                let sdp_mid = candidate.sdp_mid();
                let sdp_m_line_index = candidate.sdp_m_line_index();

                let data = IceCandidateData {
                    candidate: cand_str,
                    sdp_mid,
                    sdp_m_line_index,
                };

                batch_ref.borrow_mut().push(data);

                if batch_ref.borrow().len() >= 10 {
                    let candidates = std::mem::take(&mut *batch_ref.borrow_mut());
                    pool.broadcast_signal(&SignalPayload::IceBatch { candidates });
                } else {
                    if let Some(h) = timer_ref.borrow_mut().take() {
                        let _ = window().map(|w| w.clear_timeout_with_handle(h));
                    }

                    let cb_pool = pool.clone();
                    let cb_batch = batch_ref.clone();
                    let cb = Closure::once(move || {
                        let candidates = std::mem::take(&mut *cb_batch.borrow_mut());
                        if !candidates.is_empty() {
                            cb_pool.broadcast_signal(&SignalPayload::IceBatch { candidates });
                        }
                    });

                    if let Some(w) = window() {
                        if let Ok(h) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                            cb.as_ref().unchecked_ref(),
                            100,
                        ) {
                            *timer_ref.borrow_mut() = Some(h);
                            cb.forget();
                        }
                    }
                }
            } else {
                // Gathering complete
                let candidates = std::mem::take(&mut *batch_ref.borrow_mut());
                if !candidates.is_empty() {
                    pool.broadcast_signal(&SignalPayload::IceBatch { candidates });
                }
            }
        }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);

        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        on_ice.forget();
    }

    // Periodic presence beacon while waiting for peer
    {
        let pool = nostr_pool.clone();
        let has_conn = has_connected.clone();
        let is_neg = is_negotiating.clone();

        let beacon_cb: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
        let beacon_cb_c = beacon_cb.clone();

        let cb = Closure::wrap(Box::new(move || {
            if !*has_conn.borrow() && !*is_neg.borrow() {
                pool.broadcast_signal(&SignalPayload::Presence);
                if let Some(ref c) = *beacon_cb_c.borrow() {
                    if let Some(w) = window() {
                        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                            c.as_ref().unchecked_ref(),
                            2500,
                        );
                    }
                }
            }
        }) as Box<dyn FnMut()>);

        if let Some(w) = window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                2500,
            );
        }
        *beacon_cb.borrow_mut() = Some(cb);
    }

    Ok(session_cell)
}

async fn initiate_p2p_offer(
    pc: &RtcPeerConnection,
    pool: &NostrRelayPool,
    dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    file_dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) -> Result<(), JsValue> {
    let dc_init = RtcDataChannelInit::new();
    dc_init.set_ordered(true);
    let dc = pc.create_data_channel_with_data_channel_dict("chat", &dc_init);
    attach_datachannel_callbacks(&dc, pool.key, status_signal, messages_signal, session_cell.clone());
    *dc_cell.borrow_mut() = Some(dc);

    let file_dc_init = RtcDataChannelInit::new();
    file_dc_init.set_ordered(true);
    let file_dc = pc.create_data_channel_with_data_channel_dict("file-transfer", &file_dc_init);
    file_dc.set_binary_type(RtcDataChannelType::Arraybuffer);
    attach_file_datachannel_callbacks(&file_dc, pool.key, messages_signal, session_cell);
    *file_dc_cell.borrow_mut() = Some(file_dc);

    let offer = wasm_bindgen_futures::JsFuture::from(pc.create_offer()).await?;
    let sdp = js_sys::Reflect::get(&offer, &"sdp".into())?
        .as_string()
        .ok_or_else(|| JsValue::from_str("No SDP in offer"))?;

    let offer_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    offer_init.set_sdp(&sdp);
    wasm_bindgen_futures::JsFuture::from(pc.set_local_description(&offer_init)).await?;

    let signal = SignalPayload::Offer { sdp };
    pool.broadcast_signal(&signal);

    Ok(())
}

fn setup_responder_datachannel(
    pc: &RtcPeerConnection,
    key: [u8; KEY_LENGTH],
    dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    file_dc_cell: Rc<RefCell<Option<RtcDataChannel>>>,
    status_signal: WriteSignal<ConnectionStatus>,
    messages_signal: WriteSignal<Vec<ChatMessageUi>>,
    session_cell: Rc<RefCell<Option<WebRtcSession>>>,
) {
    let on_dc = Closure::wrap(Box::new(move |ev: RtcDataChannelEvent| {
        let dc = ev.channel();
        let label = dc.label();
        if label == "file-transfer" {
            dc.set_binary_type(RtcDataChannelType::Arraybuffer);
            attach_file_datachannel_callbacks(&dc, key, messages_signal, session_cell.clone());
            *file_dc_cell.borrow_mut() = Some(dc);
        } else {
            attach_datachannel_callbacks(&dc, key, status_signal, messages_signal, session_cell.clone());
            *dc_cell.borrow_mut() = Some(dc);
        }
    }) as Box<dyn FnMut(RtcDataChannelEvent)>);

    pc.set_ondatachannel(Some(on_dc.as_ref().unchecked_ref()));
    on_dc.forget();
}

async fn handle_remote_offer(
    pc: &RtcPeerConnection,
    sdp: String,
    pool: &NostrRelayPool,
    is_initial: bool,
    status_signal: WriteSignal<ConnectionStatus>,
) {
    let desc_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    desc_init.set_sdp(&sdp);
    if wasm_bindgen_futures::JsFuture::from(pc.set_remote_description(&desc_init))
        .await
        .is_err()
    {
        log::error!("Failed to set remote offer");
        if is_initial {
            status_signal.set(ConnectionStatus::Error("Failed to set remote offer".into()));
        }
        return;
    }

    let answer = match wasm_bindgen_futures::JsFuture::from(pc.create_answer()).await {
        Ok(a) => a,
        Err(e) => {
            log::error!("Failed to create answer: {:?}", e);
            if is_initial {
                status_signal.set(ConnectionStatus::Error("Failed to create answer".into()));
            }
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
        log::error!("Failed to set local answer");
        if is_initial {
            status_signal.set(ConnectionStatus::Error("Failed to set local answer".into()));
        }
        return;
    }

    let resp_signal = SignalPayload::Answer { sdp: answer_sdp };
    pool.broadcast_signal(&resp_signal);
}

async fn handle_remote_answer(pc: &RtcPeerConnection, sdp: String) {
    let ans_init = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    ans_init.set_sdp(&sdp);
    let _ = wasm_bindgen_futures::JsFuture::from(pc.set_remote_description(&ans_init)).await;
}

async fn add_single_ice_candidate(
    pc: &RtcPeerConnection,
    candidate: String,
    sdp_mid: Option<String>,
    sdp_m_line_index: Option<u16>,
) {
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

async fn add_batch_ice_candidates(
    pc: &RtcPeerConnection,
    candidates: Vec<IceCandidateData>,
) {
    for c in candidates {
        let init = RtcIceCandidateInit::new(&c.candidate);
        if let Some(ref mid) = c.sdp_mid {
            init.set_sdp_mid(Some(mid));
        }
        if let Some(idx) = c.sdp_m_line_index {
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
        let session_close_c = session_cell.clone();
        let on_close = Closure::wrap(Box::new(move || {
            status_signal.set(ConnectionStatus::Disconnected);
            log::info!("RTCDataChannel Closed.");
            if let Some(ref sess) = *session_close_c.borrow() {
                sess.cleanup_file_transfers(messages_signal);
            }
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
                                        file: None,
                                    });
                                });
                            }
                            DataChannelMessage::FileOffer { id, name, size, mime_type, caption, .. } => {
                                let time_str = current_time_string();
                                messages_signal.update(|msgs| {
                                    msgs.push(ChatMessageUi {
                                        id: id.clone(),
                                        sender: "Peer".into(),
                                        is_self: false,
                                        text: caption.unwrap_or_default(),
                                        time: time_str,
                                        file: Some(FileOfferInfo {
                                            file_id: id,
                                            name,
                                            size,
                                            mime_type,
                                            status: FileTransferStatus::Offered,
                                        }),
                                    });
                                });
                            }
                            DataChannelMessage::FileRequest { id } => {
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.start_file_upload(id, messages_signal);
                                }
                            }
                            DataChannelMessage::FileCancel { id, reason } => {
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.cancelled_transfers.borrow_mut().insert(id.clone());
                                    if let Some(transfer) = sess.active_incoming_transfers.borrow_mut().remove(&id) {
                                        if let Some(ref w) = transfer.writable {
                                            let w_clone = w.clone();
                                            wasm_bindgen_futures::spawn_local(async move {
                                                let _ = abort_save_stream(&w_clone).await;
                                            });
                                        }
                                    }
                                    sess.active_send_files.borrow_mut().remove(&id);
                                }
                                messages_signal.update(|msgs| {
                                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == id) {
                                        if let Some(ref mut f) = msg.file {
                                            f.status = FileTransferStatus::Cancelled { reason };
                                        }
                                    }
                                });
                            }
                            DataChannelMessage::FileComplete { id } => {
                                messages_signal.update(|msgs| {
                                    if let Some(msg) = msgs.iter_mut().find(|m| m.id == id) {
                                        if let Some(ref mut f) = msg.file {
                                            f.status = FileTransferStatus::Completed;
                                        }
                                    }
                                });
                            }
                            DataChannelMessage::CallInvite => {
                                log::info!("Incoming audio call invite received from peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    *sess.current_call_type.borrow_mut() = CallType::Audio;
                                    sess.call_state_signal.set(CallState::Incoming(CallType::Audio));
                                }
                            }
                            DataChannelMessage::VideoCallInvite => {
                                log::info!("Incoming video call invite received from peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    *sess.current_call_type.borrow_mut() = CallType::Video;
                                    sess.call_state_signal.set(CallState::Incoming(CallType::Video));
                                }
                            }
                            DataChannelMessage::ScreenShareInvite => {
                                log::info!("Incoming screen share invite received from peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    *sess.current_call_type.borrow_mut() = CallType::ScreenShare;
                                    sess.call_state_signal.set(CallState::Incoming(CallType::ScreenShare));
                                }
                            }
                            DataChannelMessage::CallAccepted => {
                                log::info!("Peer accepted call! Connecting media stream.");
                                if let Some(ref sess) = *session_c.borrow() {
                                    let current_type = *sess.current_call_type.borrow();
                                    sess.call_state_signal.set(CallState::Active(current_type));
                                    sess.attach_active_video_streams();

                                    // Caller renegotiates: create and broadcast SDP offer with media tracks
                                    let pc = sess.peer.clone();
                                    let pool = sess.nostr_pool.clone();
                                    let toast = sess.toast_signal;
                                    wasm_bindgen_futures::spawn_local(async move {
                                        match wasm_bindgen_futures::JsFuture::from(pc.create_offer()).await {
                                            Ok(offer) => {
                                                if let Ok(sdp) = js_sys::Reflect::get(&offer, &"sdp".into()) {
                                                    if let Some(sdp_str) = sdp.as_string() {
                                                        let init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                                                        init.set_sdp(&sdp_str);
                                                        if wasm_bindgen_futures::JsFuture::from(pc.set_local_description(&init)).await.is_ok() {
                                                            pool.broadcast_signal(&SignalPayload::Offer { sdp: sdp_str });
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                log::error!("Failed to create offer on call accepted: {:?}", e);
                                                toast.set(Some("Failed to negotiate media connection".into()));
                                            }
                                        }
                                    });
                                }
                            }
                            DataChannelMessage::CallRejected => {
                                log::info!("Peer rejected call");
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.cleanup_media();
                                    sess.toast_signal.set(Some("Peer declined call".into()));
                                }
                            }
                            DataChannelMessage::CallEnded => {
                                log::info!("Call ended by peer");
                                if let Some(ref sess) = *session_c.borrow() {
                                    sess.cleanup_media();
                                    sess.toast_signal.set(Some("Call ended".into()));
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
