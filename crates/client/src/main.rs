mod i18n;
mod nostr_pool;
mod qr;
mod state;
mod webrtc;

use i18n::{
    calling_peer_text, detect_browser_language, incoming_call_desc, incoming_call_title,
    large_file_warning_desc, t, update_document_direction, Language,
};
use leptos::*;
use qr::generate_qr_svg;
use state::{
    format_file_size, get_full_share_url, get_or_init_credentials, AudioSettings, CallState,
    CallType, ChatMessageUi, ConnectionStatus, FileTransferStatus,
};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{window, HtmlInputElement};
use webrtc::{start_webrtc_session, supported_audio_constraints, WebRtcSession};

#[component]
fn App() -> impl IntoView {
    let initial_lang = detect_browser_language();
    let (lang, set_lang) = create_signal(initial_lang);

    create_effect(move |_| {
        update_document_direction(lang.get());
    });

    let (status, set_status) = create_signal(ConnectionStatus::Idle);
    let (messages, set_messages) = create_signal(Vec::<ChatMessageUi>::new());
    let (input_text, set_input_text) = create_signal(String::new());
    let (show_qr, set_show_qr) = create_signal(false);
    let (connected_relays, set_connected_relays) = create_signal(0usize);
    let (show_relays, set_show_relays) = create_signal(false);
    let (copied, set_copied) = create_signal(false);
    let (room_id_sig, set_room_id_sig) = create_signal(String::new());

    // File sharing state
    let (staged_file, set_staged_file) = create_signal(Option::<web_sys::File>::None);
    let (large_file_warning, set_large_file_warning) = create_signal(Option::<(String, String)>::None);

    // Call state signals (Audio, Video, Screen Share)
    let (call_state, set_call_state) = create_signal(CallState::Idle);
    let (is_mic_muted, set_is_mic_muted) = create_signal(false);
    let (is_video_muted, set_is_video_muted) = create_signal(false);
    let (is_speaker_muted, set_is_speaker_muted) = create_signal(false);
    let (_is_front_camera, set_is_front_camera) = create_signal(true);
    let (audio_settings, set_audio_settings) = create_signal(AudioSettings::default());
    let (show_audio_settings, set_show_audio_settings) = create_signal(false);
    let (toast, set_toast) = create_signal(Option::<String>::None);

    let session_ref = store_value(Rc::new(RefCell::new(None::<WebRtcSession>)));

    // Initialize session on mount
    create_effect(move |_| {
        if let Some((room_id, key, _)) = get_or_init_credentials() {
            set_room_id_sig.set(room_id.clone());
            if let Ok(sess) = start_webrtc_session(
                room_id,
                key,
                set_status,
                set_messages,
                set_call_state,
                set_is_mic_muted,
                set_is_video_muted,
                set_is_speaker_muted,
                set_is_front_camera,
                set_toast,
                set_connected_relays,
            ) {
                session_ref.set_value(sess);
            }
        }
    });

    let send_message_or_file = move || {
        let text = input_text.get().trim().to_string();
        let staged = staged_file.get();

        if text.is_empty() && staged.is_none() {
            return;
        }

        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                if let Some(file) = staged {
                    let caption = if text.is_empty() { None } else { Some(text) };
                    if let Err(err) = sess.send_file_offer(file, caption, set_messages) {
                        log::warn!("Failed to send file offer: {err}");
                    } else {
                        set_staged_file.set(None);
                        set_input_text.set(String::new());
                    }
                } else {
                    if let Err(err) = sess.send_chat_message(&text, set_messages) {
                        log::warn!("Failed to send message: {err}");
                    } else {
                        set_input_text.set(String::new());
                    }
                }
            }
        });
    };

    let handle_download_file = move |file_id: String| {
        let mut file_info = None;
        messages.with(|msgs| {
            if let Some(msg) = msgs.iter().find(|m| m.id == file_id) {
                if let Some(ref f) = msg.file {
                    file_info = Some((f.name.clone(), f.size));
                }
            }
        });

        if let Some((name, size)) = file_info {
            let has_picker = window()
                .and_then(|w| js_sys::Reflect::get(&w, &"showSaveFilePicker".into()).ok())
                .map(|val| !val.is_undefined() && !val.is_null())
                .unwrap_or(false);

            if size > 250 * 1024 * 1024 && !has_picker {
                set_large_file_warning.set(Some((file_id.clone(), name)));
                return;
            }
        }

        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.request_file_download(&file_id, set_messages);
            }
        });
    };

    let handle_confirm_large_download = move || {
        if let Some((file_id, _)) = large_file_warning.get() {
            session_ref.with_value(|sess_cell| {
                if let Some(ref sess) = *sess_cell.borrow() {
                    sess.request_file_download(&file_id, set_messages);
                }
            });
            set_large_file_warning.set(None);
        }
    };

    let handle_cancel_file = move |file_id: String| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.cancel_file_transfer(&file_id, set_messages);
            }
        });
    };

    let copy_share_link = move |_| {
        let url = get_full_share_url();
        if let Some(win) = window() {
            let clipboard = win.navigator().clipboard();
            let _ = clipboard.write_text(&url);
            set_copied.set(true);

            set_timeout(
                move || {
                    set_copied.set(false);
                },
                std::time::Duration::from_secs(2),
            );
        }
    };

    let destroy_session = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.nostr_pool.broadcast_signal(&protocol::SignalPayload::PeerLeft);
                sess.cleanup_media();
                sess.cleanup_file_transfers(set_messages);
            }
        });
        if let Some(win) = window() {
            let _ = win.location().set_href("/");
        }
    };

    // Call Action Handlers
    let handle_start_audio = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.start_call(CallType::Audio);
            }
        });
    };

    let handle_start_video = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.start_call(CallType::Video);
            }
        });
    };

    let handle_start_screen = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.start_call(CallType::ScreenShare);
            }
        });
    };

    let handle_accept_call = move |c_type: CallType| {
        session_ref.with_value(move |sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.accept_call(c_type);
            }
        });
    };

    let handle_reject_call = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.reject_call();
            }
        });
    };

    let handle_toggle_mute = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.toggle_mic_mute();
            }
        });
    };

    let handle_toggle_speaker = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.toggle_speaker_mute();
            }
        });
    };

    let apply_audio_settings = move |new_settings: AudioSettings| {
        set_audio_settings.set(new_settings);
        session_ref.with_value(move |sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.set_audio_settings(new_settings);
            }
        });
    };

    let open_audio_settings = move |_| {
        // The modal lives outside the video stage, so it would be hidden in fullscreen.
        if let Some(doc) = window().and_then(|w| w.document()) {
            if doc.fullscreen_element().is_some() {
                doc.exit_fullscreen();
            }
        }
        set_show_audio_settings.set(true);
    };

    let handle_toggle_video = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.toggle_video_mute();
            }
        });
    };

    let handle_flip_camera = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.flip_camera();
            }
        });
    };

    let handle_end_call = move |_| {
        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                sess.end_call();
            }
        });
    };

    let handle_fullscreen = move |_| {
        if let Some(win) = window() {
            if let Some(doc) = win.document() {
                if let Some(el) = doc.get_element_by_id("video-stage-container") {
                    let _ = el.request_fullscreen();
                }
            }
        }
    };

    create_effect(move |_| {
        let state = call_state.get();
        if matches!(state, CallState::Active(CallType::Video) | CallState::Active(CallType::ScreenShare)) {
            session_ref.with_value(move |sess_cell| {
                if let Some(ref sess) = *sess_cell.borrow() {
                    sess.attach_active_video_streams();
                }
            });
        }
    });

    let handle_app_click = move |_| {
        if let Some(win) = window() {
            if let Some(doc) = win.document() {
                if let Some(el) = doc.get_element_by_id("remote-audio") {
                    if let Ok(audio) = el.dyn_into::<web_sys::HtmlAudioElement>() {
                        if audio.src_object().is_some() && audio.paused() {
                            let _ = audio.play();
                        }
                    }
                }
            }
        }
    };

    let is_connected = move || status.get() == ConnectionStatus::Connected;

    view! {
        <div id="app" on:click=handle_app_click>
            <header>
            <div class="brand">
                <span>"🔒 dchat"</span>
                <span class="brand-badge">"RAM-Only"</span>
            </div>
            <div class="header-actions">
                <select
                    class="lang-select"
                    prop:value=move || lang.get().code()
                    on:change=move |ev| {
                        let val = event_target_value(&ev);
                        if let Some(l) = Language::from_code(&val) {
                            set_lang.set(l);
                        }
                    }
                    title="Change language / Cambiar idioma"
                >
                    {Language::ALL
                        .iter()
                        .map(|l| {
                            view! {
                                <option value=l.code() selected=move || lang.get() == *l>
                                    {format!("🌐 {}", l.native_name())}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
                <button
                    class="btn btn-secondary"
                    on:click=move |_| set_show_qr.set(true)
                >
                    {move || t(lang.get(), "scan_qr")}
                </button>
                <button
                    class="btn btn-danger"
                    on:click=destroy_session
                    title=move || t(lang.get(), "wipe_session_title")
                >
                    {move || t(lang.get(), "wipe_session")}
                </button>
            </div>
        </header>

        <div class="connection-bar">
            <div class="status-indicator">
                <span class=move || format!("status-dot {}", status.get().color_class())></span>
                <span style="font-weight: 600;">{move || status.get().label_i18n(lang.get())}</span>
                <span style="color: var(--text-muted); font-size: 0.8rem;">
                    {move || format!("{}{}", t(lang.get(), "room_prefix"), room_id_sig.get())}
                </span>
                <button
                    class="relay-badge"
                    on:click=move |_| set_show_relays.set(true)
                    style="background: rgba(168, 85, 247, 0.15); color: #c084fc; border: 1px solid rgba(168, 85, 247, 0.3); border-radius: 9999px; padding: 2px 8px; font-size: 0.75rem; font-weight: 500; cursor: pointer; display: inline-flex; align-items: center; gap: 4px; margin-left: 6px;"
                    title=move || t(lang.get(), "nostr_relays_title")
                >
                    <span>{move || t(lang.get(), "nostr_badge")}</span>
                    <span>{move || {
                        let c = connected_relays.get();
                        if c > 0 {
                            format!("({} active)", c)
                        } else {
                            "(connecting...)".to_string()
                        }
                    }}</span>
                </button>
            </div>
            <div style="display: flex; align-items: center; gap: 6px; flex-wrap: wrap;">
                {move || {
                    if is_connected() && call_state.get() == CallState::Idle {
                        view! {
                            <button
                                class="btn btn-call"
                                on:click=handle_start_audio
                                title=move || t(lang.get(), "btn_audio_title")
                            >
                                {move || t(lang.get(), "btn_audio")}
                            </button>
                            <button
                                class="btn btn-primary"
                                style="background-color: var(--accent-cyan);"
                                on:click=handle_start_video
                                title=move || t(lang.get(), "btn_video_title")
                            >
                                {move || t(lang.get(), "btn_video")}
                            </button>
                            <button
                                class="btn btn-secondary"
                                on:click=handle_start_screen
                                title=move || t(lang.get(), "btn_screen_title")
                            >
                                {move || t(lang.get(), "btn_screen")}
                            </button>
                        }.into_view()
                    } else {
                        view! { <span></span> }.into_view()
                    }
                }}
                {move || {
                    if matches!(call_state.get(), CallState::Active(_)) {
                        view! { <span></span> }.into_view()
                    } else {
                        view! {
                            <button
                                id="audio-settings-btn"
                                class="btn btn-secondary"
                                on:click=open_audio_settings
                                title=move || t(lang.get(), "btn_audio_settings_title")
                            >
                                "⚙️"
                            </button>
                        }.into_view()
                    }
                }}
                <button
                    class="btn btn-secondary"
                    on:click=copy_share_link
                >
                    {move || if copied.get() { t(lang.get(), "btn_copied") } else { t(lang.get(), "btn_copy_link") }}
                </button>
            </div>
        </div>

        // Calling Banner
        {move || match call_state.get() {
            CallState::Calling(c_type) => view! {
                <div class="call-bar">
                    <div class="call-info">
                        <span class="status-dot status-connecting"></span>
                        <span>{move || calling_peer_text(lang.get(), c_type.label_i18n(lang.get()))}</span>
                    </div>
                    <div class="call-actions">
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                        >
                            {move || t(lang.get(), "btn_cancel")}
                        </button>
                    </div>
                </div>
            }.into_view(),
            _ => view! { <div></div> }.into_view(),
        }}

        // Active Audio Call Bar
        {move || match call_state.get() {
            CallState::Active(CallType::Audio) => view! {
                <div class="call-bar">
                    <div class="call-info">
                        <span>{move || t(lang.get(), "audio_active")}</span>
                        <span style="font-weight: normal; font-size: 0.75rem; color: var(--text-muted);">
                            {move || t(lang.get(), "dtls_srtp_badge")}
                        </span>
                    </div>
                    <div class="call-actions">
                        <button
                            class=move || if is_mic_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_mute
                        >
                            {move || if is_mic_muted.get() { t(lang.get(), "btn_unmute_mic") } else { t(lang.get(), "btn_mute_mic") }}
                        </button>
                        <button
                            id="speaker-mute-btn"
                            class=move || if is_speaker_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_speaker
                            title=move || t(lang.get(), "title_mute_speaker")
                        >
                            {move || if is_speaker_muted.get() { t(lang.get(), "btn_unmute_speaker") } else { t(lang.get(), "btn_mute_speaker") }}
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=open_audio_settings
                            title=move || t(lang.get(), "btn_audio_settings_title")
                        >
                            "⚙️"
                        </button>
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                        >
                            {move || t(lang.get(), "btn_end_call")}
                        </button>
                    </div>
                </div>
            }.into_view(),
            _ => view! { <div></div> }.into_view(),
        }}

        // Active Video / Screen Share Stage
        {move || match call_state.get() {
            CallState::Active(CallType::Video) | CallState::Active(CallType::ScreenShare) => view! {
                <div id="video-stage-container" class="video-stage">
                    <video id="remote-video-feed" class="remote-video" autoplay playsinline muted></video>
                    <video id="local-video-preview" class="local-preview" autoplay playsinline muted></video>
                    <div class="video-overlay-controls">
                        <button
                            class=move || if is_mic_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_mute
                            title=move || t(lang.get(), "title_mute_mic")
                        >
                            {move || if is_mic_muted.get() { "🔇" } else { "🎙️" }}
                        </button>
                        <button
                            id="speaker-mute-btn"
                            class=move || if is_speaker_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_speaker
                            title=move || t(lang.get(), "title_mute_speaker")
                        >
                            {move || if is_speaker_muted.get() { "🔈" } else { "🔊" }}
                        </button>
                        <button
                            class=move || if is_video_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_video
                            title=move || t(lang.get(), "title_camera")
                        >
                            {move || if is_video_muted.get() { "🙈" } else { "📹" }}
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=handle_flip_camera
                            title=move || t(lang.get(), "title_flip_camera")
                        >
                            "📷"
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=handle_fullscreen
                            title=move || t(lang.get(), "title_fullscreen")
                        >
                            "⛶"
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=open_audio_settings
                            title=move || t(lang.get(), "btn_audio_settings_title")
                        >
                            "⚙️"
                        </button>
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                            title=move || t(lang.get(), "btn_end_call")
                        >
                            {move || t(lang.get(), "btn_end_call_short")}
                        </button>
                    </div>
                </div>
            }.into_view(),
            _ => view! { <div></div> }.into_view(),
        }}

        <main class="chat-container">
            {move || {
                let msgs = messages.get();
                if msgs.is_empty() {
                    view! {
                        <div class="empty-state">
                            <h3>{move || t(lang.get(), "empty_title")}</h3>
                            <p>{move || t(lang.get(), "empty_desc")}</p>
                            <div class="security-checklist">
                                <li>{move || t(lang.get(), "check_e2ee")}</li>
                                <li>{move || t(lang.get(), "check_audio")}</li>
                                <li>{move || t(lang.get(), "check_video")}</li>
                                <li>{move || t(lang.get(), "check_files")}</li>
                                <li>{move || t(lang.get(), "check_zk")}</li>
                                <li>{move || t(lang.get(), "check_zero_storage")}</li>
                                <li>{move || t(lang.get(), "check_destruction")}</li>
                            </div>
                        </div>
                    }.into_view()
                } else {
                    view! {
                        <For
                            each=move || messages.get()
                            key=|msg| msg.id.clone()
                            children=move |msg| {
                                let row_class = if msg.is_self { "message-row self" } else { "message-row peer" };
                                let msg_time = msg.time.clone();
                                let msg_sender = msg.sender.clone();
                                let is_self = msg.is_self;

                                view! {
                                    <div class=row_class>
                                        {if let Some(f) = msg.file {
                                            let file_id = f.file_id.clone();
                                            let file_name = f.name.clone();
                                            let file_size_str = format_file_size(f.size);
                                            let status = f.status.clone();
                                            let caption = msg.text.clone();

                                            view! {
                                                <div class="file-card">
                                                    <div class="file-card-header">
                                                        <span class="file-icon">"📦"</span>
                                                        <div class="file-info">
                                                             <span class="file-name">{file_name}</span>
                                                            <span class="file-size">{file_size_str}</span>
                                                        </div>
                                                    </div>
                                                    {if !caption.is_empty() {
                                                        view! { <div class="file-caption">{caption}</div> }.into_view()
                                                    } else {
                                                        view! { <div></div> }.into_view()
                                                    }}
                                                    <div class="file-card-actions">
                                                        {match status {
                                                            FileTransferStatus::Offered => {
                                                                if is_self {
                                                                    let fid = file_id.clone();
                                                                    view! {
                                                                        <div class="file-status-row">
                                                                            <span class="file-status-text">{move || t(lang.get(), "file_waiting_peer")}</span>
                                                                            <button
                                                                                class="btn btn-sm btn-danger file-cancel-btn"
                                                                                on:click=move |_| handle_cancel_file(fid.clone())
                                                                            >
                                                                                {move || t(lang.get(), "btn_cancel")}
                                                                            </button>
                                                                        </div>
                                                                    }.into_view()
                                                                } else {
                                                                    let fid_dl = file_id.clone();
                                                                    let fid_dec = file_id.clone();
                                                                    view! {
                                                                        <div class="file-status-row">
                                                                            <button
                                                                                class="btn btn-sm btn-primary file-download-btn"
                                                                                on:click=move |_| handle_download_file(fid_dl.clone())
                                                                            >
                                                                                {move || t(lang.get(), "file_download")}
                                                                            </button>
                                                                            <button
                                                                                class="btn btn-sm btn-secondary file-cancel-btn"
                                                                                on:click=move |_| handle_cancel_file(fid_dec.clone())
                                                                            >
                                                                                {move || t(lang.get(), "file_decline")}
                                                                            </button>
                                                                        </div>
                                                                    }.into_view()
                                                                }
                                                            }
                                                            FileTransferStatus::Downloading { progress, speed_kb } => {
                                                                let fid = file_id.clone();
                                                                let speed_str = if speed_kb > 1024 {
                                                                    format!("{:.1} MB/s", speed_kb as f64 / 1024.0)
                                                                } else {
                                                                    format!("{} KB/s", speed_kb)
                                                                };
                                                                view! {
                                                                    <div class="file-progress-container">
                                                                        <div class="file-progress-bar">
                                                                            <div class="file-progress-fill" style=format!("width: {}%;", progress)></div>
                                                                        </div>
                                                                        <div class="file-progress-meta">
                                                                            <span class="file-progress-label">
                                                                                {move || {
                                                                                    let label = if is_self { t(lang.get(), "file_uploading") } else { t(lang.get(), "file_downloading") };
                                                                                    format!("{}: {}% ({})", label, progress, speed_str)
                                                                                }}
                                                                            </span>
                                                                            <button
                                                                                class="btn btn-sm btn-danger file-cancel-btn"
                                                                                on:click=move |_| handle_cancel_file(fid.clone())
                                                                            >
                                                                                {move || t(lang.get(), "btn_cancel")}
                                                                            </button>
                                                                        </div>
                                                                    </div>
                                                                }.into_view()
                                                            }
                                                            FileTransferStatus::Completed => {
                                                                view! {
                                                                    <div class="file-status-row">
                                                                        <span class="file-status-text completed">
                                                                            {move || if is_self { t(lang.get(), "file_sent_success") } else { t(lang.get(), "file_download_complete") }}
                                                                        </span>
                                                                    </div>
                                                                }.into_view()
                                                            }
                                                            FileTransferStatus::Cancelled { reason } => {
                                                                view! {
                                                                    <div class="file-status-row">
                                                                        <span class="file-status-text cancelled">
                                                                            {move || format!("{}{}", t(lang.get(), "file_cancelled_prefix"), reason)}
                                                                        </span>
                                                                    </div>
                                                                }.into_view()
                                                            }
                                                            FileTransferStatus::Interrupted => {
                                                                view! {
                                                                    <div class="file-status-row">
                                                                        <span class="file-status-text cancelled">
                                                                            {move || t(lang.get(), "file_interrupted")}
                                                                        </span>
                                                                    </div>
                                                                }.into_view()
                                                            }
                                                        }}
                                                    </div>
                                                </div>
                                            }.into_view()
                                        } else {
                                            view! {
                                                <div class="message-bubble">{msg.text}</div>
                                            }.into_view()
                                        }}
                                        <div class="message-meta">
                                            <span>{msg_sender}</span>
                                            <span>" • "</span>
                                            <span>{msg_time}</span>
                                        </div>
                                    </div>
                                }
                            }
                        />
                    }.into_view()
                }
            }}
        </main>

        {move || staged_file.get().map(|file| {
            let size_str = format_file_size(file.size() as u64);
            let name = file.name();
            view! {
                <div class="attachment-chip">
                    <span class="attachment-icon">"📎"</span>
                    <span class="attachment-name">{name}</span>
                    <span class="attachment-size">{format!("({})", size_str)}</span>
                    <button
                        class="btn-remove-attachment"
                        on:click=move |_| set_staged_file.set(None)
                        title=move || t(lang.get(), "file_remove_title")
                    >
                        "✕"
                    </button>
                </div>
            }
        })}

        <input
            type="file"
            id="file-input-hidden"
            style="display: none;"
            on:change=move |ev| {
                let target: HtmlInputElement = event_target(&ev);
                if let Some(files) = target.files() {
                    if let Some(file) = files.get(0) {
                        set_staged_file.set(Some(file));
                    }
                }
                target.set_value("");
            }
        />

        <footer class="input-bar">
            <button
                class="btn btn-secondary attach-btn"
                disabled=move || !is_connected()
                on:click=move |_| {
                    if let Some(win) = window() {
                        if let Some(doc) = win.document() {
                            if let Some(el) = doc.get_element_by_id("file-input-hidden") {
                                if let Ok(input) = el.dyn_into::<HtmlInputElement>() {
                                    input.click();
                                }
                            }
                        }
                    }
                }
                title=move || t(lang.get(), "file_attach_title")
            >
                "📎"
            </button>
            <input
                type="text"
                placeholder=move || if is_connected() {
                    if staged_file.get().is_some() {
                        t(lang.get(), "placeholder_caption")
                    } else {
                        t(lang.get(), "placeholder_connected")
                    }
                } else {
                    t(lang.get(), "placeholder_waiting")
                }
                prop:value=move || input_text.get()
                on:input=move |ev| {
                    let target: HtmlInputElement = event_target(&ev);
                    set_input_text.set(target.value());
                }
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        send_message_or_file();
                    }
                }
            />
            <button
                class="btn btn-primary send-btn"
                disabled=move || !is_connected() || (input_text.get().trim().is_empty() && staged_file.get().is_none())
                on:click=move |_| send_message_or_file()
            >
                {move || t(lang.get(), "btn_send")}
            </button>
        </footer>

        // Incoming Call Modal
        {move || match call_state.get() {
            CallState::Incoming(c_type) => view! {
                <div class="modal-backdrop">
                    <div class="incoming-call-box">
                        <h3>{move || incoming_call_title(lang.get(), c_type.label_i18n(lang.get()))}</h3>
                        <p>{move || incoming_call_desc(lang.get(), c_type.label_i18n(lang.get()))}</p>
                        <div style="display: flex; gap: 12px; width: 100%; justify-content: center;">
                            <button
                                class="btn btn-call"
                                style="padding: 10px 20px; font-size: 1rem;"
                                on:click=move |_| handle_accept_call(c_type)
                            >
                                {move || t(lang.get(), "incoming_accept")}
                            </button>
                            <button
                                class="btn btn-danger"
                                style="padding: 10px 20px; font-size: 1rem;"
                                on:click=handle_reject_call
                            >
                                {move || t(lang.get(), "incoming_decline")}
                            </button>
                        </div>
                    </div>
                </div>
            }.into_view(),
            _ => view! { <div></div> }.into_view(),
        }}

        // QR Code Modal for Phone Pairing
        {move || {
            if show_qr.get() {
                let qr_svg = generate_qr_svg(&get_full_share_url()).unwrap_or_default();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_qr.set(false)>
                        <div class="modal-content" on:click=|ev| ev.stop_propagation()>
                            <h3>{move || t(lang.get(), "qr_title")}</h3>
                            <p>{move || t(lang.get(), "qr_desc")}</p>
                            <div class="qr-wrapper" inner_html=qr_svg></div>
                            <button
                                class="btn btn-secondary"
                                style="width: 100%;"
                                on:click=move |_| set_show_qr.set(false)
                            >
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }.into_view()
            } else {
                view! { <div></div> }.into_view()
            }
        }}

        // Nostr Relays Modal
        {move || {
            if show_relays.get() {
                let relays_list = crate::state::get_default_relays();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_relays.set(false)>
                        <div class="modal-content" style="max-width: 480px; text-align: left;" on:click=|ev| ev.stop_propagation()>
                            <div style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 12px;">
                                <h3 style="margin: 0; display: flex; align-items: center; gap: 8px;">
                                    {move || t(lang.get(), "nostr_relays_title")}
                                </h3>
                                <button class="btn btn-secondary" style="padding: 4px 10px;" on:click=move |_| set_show_relays.set(false)>
                                    "✕"
                                </button>
                            </div>
                            <p style="font-size: 0.85rem; color: var(--text-muted); margin-bottom: 16px; line-height: 1.4;">
                                {move || t(lang.get(), "nostr_relays_desc")}
                            </p>
                            <div style="background: var(--bg-surface-2, rgba(255,255,255,0.05)); border-radius: 8px; padding: 12px; margin-bottom: 16px;">
                                <div style="font-size: 0.8rem; font-weight: 600; margin-bottom: 8px; color: var(--text-muted);">
                                    {move || t(lang.get(), "nostr_active_relays")}
                                </div>
                                <ul style="list-style: none; padding: 0; margin: 0; font-family: monospace; font-size: 0.8rem; display: flex; flex-direction: column; gap: 6px;">
                                    {relays_list.into_iter().map(|r| {
                                        view! {
                                            <li style="display: flex; align-items: center; gap: 6px; word-break: break-all;">
                                                <span style="color: #4ade80;">"●"</span>
                                                <span>{r}</span>
                                            </li>
                                        }
                                    }).collect_view()}
                                </ul>
                            </div>
                            <button
                                class="btn btn-primary"
                                style="width: 100%;"
                                on:click=move |_| set_show_relays.set(false)
                            >
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }.into_view()
            } else {
                view! { <div></div> }.into_view()
            }
        }}

        // Audio Settings Modal
        {move || {
            if show_audio_settings.get() {
                let (ns_supported, ec_supported, agc_supported) = supported_audio_constraints();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_audio_settings.set(false)>
                        <div class="modal-content" style="max-width: 420px; text-align: left; align-items: stretch;" on:click=|ev| ev.stop_propagation()>
                            <div style="display: flex; justify-content: space-between; align-items: center;">
                                <h3 style="margin: 0;">{move || t(lang.get(), "audio_settings_title")}</h3>
                                <button class="btn btn-secondary" style="padding: 4px 10px;" on:click=move |_| set_show_audio_settings.set(false)>
                                    "✕"
                                </button>
                            </div>
                            <div class="audio-options">
                                {audio_option_row(
                                    lang,
                                    "audio-ns",
                                    "opt_noise_suppression",
                                    ns_supported,
                                    Signal::derive(move || audio_settings.get().noise_suppression),
                                    move |on| apply_audio_settings(AudioSettings { noise_suppression: on, ..audio_settings.get_untracked() }),
                                )}
                                {audio_option_row(
                                    lang,
                                    "audio-ec",
                                    "opt_echo_cancellation",
                                    ec_supported,
                                    Signal::derive(move || audio_settings.get().echo_cancellation),
                                    move |on| apply_audio_settings(AudioSettings { echo_cancellation: on, ..audio_settings.get_untracked() }),
                                )}
                                {audio_option_row(
                                    lang,
                                    "audio-agc",
                                    "opt_auto_gain_control",
                                    agc_supported,
                                    Signal::derive(move || audio_settings.get().auto_gain_control),
                                    move |on| apply_audio_settings(AudioSettings { auto_gain_control: on, ..audio_settings.get_untracked() }),
                                )}
                            </div>
                            <button
                                class="btn btn-primary"
                                style="width: 100%;"
                                on:click=move |_| set_show_audio_settings.set(false)
                            >
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }.into_view()
            } else {
                view! { <div></div> }.into_view()
            }
        }}

        // Large File Warning Modal
        {move || large_file_warning.get().map(|(_, name)| {
            let fname = name.clone();
            view! {
                <div class="modal-backdrop">
                    <div class="modal-content">
                        <h3>{move || t(lang.get(), "large_file_title")}</h3>
                        <p>
                            {move || large_file_warning_desc(lang.get(), &fname)}
                        </p>
                        <p style="color: var(--text-muted); font-size: 0.85rem; margin-top: 8px;">
                            {move || t(lang.get(), "large_file_subdesc")}
                        </p>
                        <div style="display: flex; gap: 12px; justify-content: flex-end; margin-top: 20px;">
                            <button
                                class="btn btn-secondary"
                                on:click=move |_| set_large_file_warning.set(None)
                            >
                                {move || t(lang.get(), "btn_cancel")}
                            </button>
                            <button
                                class="btn btn-primary"
                                on:click=move |_| handle_confirm_large_download()
                            >
                                {move || t(lang.get(), "btn_proceed_anyway")}
                            </button>
                        </div>
                    </div>
                </div>
            }
        })}

        // Floating toast message
        {move || {
            if let Some(msg) = toast.get() {
                set_timeout(
                    move || {
                        set_toast.set(None);
                    },
                    std::time::Duration::from_secs(3),
                );
                view! {
                    <div class="toast">
                        {msg}
                    </div>
                }.into_view()
            } else {
                view! { <div></div> }.into_view()
            }
        }}
        </div>
    }
}

/// One mic-processing checkbox; disabled with a hint when the browser lacks the switch.
fn audio_option_row(
    lang: ReadSignal<Language>,
    id: &'static str,
    label_key: &'static str,
    supported: bool,
    checked: Signal<bool>,
    on_toggle: impl Fn(bool) + 'static,
) -> impl IntoView {
    view! {
        <label class=if supported { "audio-option" } else { "audio-option disabled" } for=id>
            <input
                type="checkbox"
                id=id
                prop:checked=move || checked.get()
                prop:disabled=!supported
                on:change=move |ev| on_toggle(event_target_checked(&ev))
            />
            <span>{move || t(lang.get(), label_key)}</span>
            {(!supported).then(|| view! {
                <span class="audio-option-hint">{move || t(lang.get(), "opt_not_supported")}</span>
            })}
        </label>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(|| view! { <App/> });
}
