mod qr;
mod state;
mod webrtc;

use leptos::*;
use qr::generate_qr_svg;
use state::{get_full_share_url, get_or_init_credentials, CallState, CallType, ChatMessageUi, ConnectionStatus};
use std::cell::RefCell;
use std::rc::Rc;
use web_sys::{window, HtmlInputElement};
use webrtc::{start_webrtc_session, WebRtcSession};

#[component]
fn App() -> impl IntoView {
    let (status, set_status) = create_signal(ConnectionStatus::Idle);
    let (messages, set_messages) = create_signal(Vec::<ChatMessageUi>::new());
    let (input_text, set_input_text) = create_signal(String::new());
    let (show_qr, set_show_qr) = create_signal(false);
    let (copied, set_copied) = create_signal(false);
    let (room_id_sig, set_room_id_sig) = create_signal(String::new());

    // Call state signals (Audio, Video, Screen Share)
    let (call_state, set_call_state) = create_signal(CallState::Idle);
    let (is_mic_muted, set_is_mic_muted) = create_signal(false);
    let (is_video_muted, set_is_video_muted) = create_signal(false);
    let (_is_front_camera, set_is_front_camera) = create_signal(true);
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
                set_is_front_camera,
                set_toast,
            ) {
                session_ref.set_value(sess);
            }
        }
    });

    let send_message = move || {
        let text = input_text.get().trim().to_string();
        if text.is_empty() {
            return;
        }

        session_ref.with_value(|sess_cell| {
            if let Some(ref sess) = *sess_cell.borrow() {
                if let Err(err) = sess.send_chat_message(&text, set_messages) {
                    log::warn!("Failed to send message: {err}");
                } else {
                    set_input_text.set(String::new());
                }
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
                sess.cleanup_media();
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

    let is_connected = move || status.get() == ConnectionStatus::Connected;

    view! {
        <header>
            <div class="brand">
                <span>"🔒 dchat"</span>
                <span class="brand-badge">"RAM-Only"</span>
            </div>
            <div class="header-actions">
                <button
                    class="btn btn-secondary"
                    on:click=move |_| set_show_qr.set(true)
                >
                    "📱 Scan QR"
                </button>
                <button
                    class="btn btn-danger"
                    on:click=destroy_session
                    title="Erases linear memory and closes connections"
                >
                    "💥 Wipe Session"
                </button>
            </div>
        </header>

        <div class="connection-bar">
            <div class="status-indicator">
                <span class=move || format!("status-dot {}", status.get().color_class())></span>
                <span style="font-weight: 600;">{move || status.get().label()}</span>
                <span style="color: var(--text-muted); font-size: 0.8rem;">
                    {move || format!("• Room: {}", room_id_sig.get())}
                </span>
            </div>
            <div style="display: flex; align-items: center; gap: 6px; flex-wrap: wrap;">
                {move || {
                    if is_connected() && call_state.get() == CallState::Idle {
                        view! {
                            <button
                                class="btn btn-call"
                                on:click=handle_start_audio
                                title="Start Encrypted Audio Call"
                            >
                                "📞 Audio"
                            </button>
                            <button
                                class="btn btn-primary"
                                style="background-color: var(--accent-cyan);"
                                on:click=handle_start_video
                                title="Start Encrypted Video Call"
                            >
                                "📹 Video"
                            </button>
                            <button
                                class="btn btn-secondary"
                                on:click=handle_start_screen
                                title="Share Screen with Peer"
                            >
                                "🖥️ Screen"
                            </button>
                        }.into_view()
                    } else {
                        view! { <span></span> }.into_view()
                    }
                }}
                <button
                    class="btn btn-secondary"
                    on:click=copy_share_link
                >
                    {move || if copied.get() { "✅ Copied!" } else { "🔗 Copy Link" }}
                </button>
            </div>
        </div>

        // Calling Banner
        {move || match call_state.get() {
            CallState::Calling(c_type) => view! {
                <div class="call-bar">
                    <div class="call-info">
                        <span class="status-dot status-connecting"></span>
                        <span>{format!("Calling Peer with {}... Waiting for answer", c_type.label())}</span>
                    </div>
                    <div class="call-actions">
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                        >
                            "Cancel"
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
                        <span>"🔊 Audio Call Active"</span>
                        <span style="font-weight: normal; font-size: 0.75rem; color: var(--text-muted);">
                            "(DTLS-SRTP P2P)"
                        </span>
                    </div>
                    <div class="call-actions">
                        <button
                            class=move || if is_mic_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_mute
                        >
                            {move || if is_mic_muted.get() { "🔇 Unmute Mic" } else { "🎙️ Mute Mic" }}
                        </button>
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                        >
                            "🔴 End Call"
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
                    <video id="remote-video-feed" class="remote-video" autoplay playsinline></video>
                    <video id="local-video-preview" class="local-preview" autoplay playsinline muted></video>
                    <div class="video-overlay-controls">
                        <button
                            class=move || if is_mic_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_mute
                            title="Mute/Unmute Mic"
                        >
                            {move || if is_mic_muted.get() { "🔇" } else { "🎙️" }}
                        </button>
                        <button
                            class=move || if is_video_muted.get() { "btn btn-mute muted" } else { "btn btn-mute" }
                            on:click=handle_toggle_video
                            title="Enable/Disable Camera"
                        >
                            {move || if is_video_muted.get() { "🙈" } else { "📹" }}
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=handle_flip_camera
                            title="Flip Front/Rear Camera"
                        >
                            "📷"
                        </button>
                        <button
                            class="btn btn-secondary"
                            on:click=handle_fullscreen
                            title="Toggle Fullscreen"
                        >
                            "⛶"
                        </button>
                        <button
                            class="btn btn-danger"
                            on:click=handle_end_call
                            title="End Call"
                        >
                            "🔴 End"
                        </button>
                    </div>
                </div>
            }.into_view(),
            _ => view! { <div></div> }.into_view(),
        }}

        <div class="feature-bar">
            <span class="feature-tag active">"💬 Phase 1: Text Chat"</span>
            <span class="feature-tag active">"🎙️ Phase 2: Audio Call"</span>
            <span class="feature-tag active">"📹 Phase 3: Video & Screen"</span>
        </div>

        <main class="chat-container">
            {move || {
                let msgs = messages.get();
                if msgs.is_empty() {
                    view! {
                        <div class="empty-state">
                            <h3>"Ephemeral P2P Encrypted Session"</h3>
                            <p>"Share your link or QR code with another device to chat, call, or share screens."</p>
                            <div class="security-checklist">
                                <li>"🛡️ 256-bit ChaCha20-Poly1305 E2EE Text"</li>
                                <li>"🎙️ DTLS-SRTP Audio Calls"</li>
                                <li>"📹 Video Calls & Screen Sharing with Camera Flip"</li>
                                <li>"🔑 Zero-Knowledge: Keys never touch server"</li>
                                <li>"🚫 Zero Storage: Strictly in volatile Wasm RAM"</li>
                                <li>"💥 Instant Destruction: Refresh or close tab wipes all history"</li>
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
                                view! {
                                    <div class=row_class>
                                        <div class="message-bubble">{msg.text}</div>
                                        <div class="message-meta">
                                            <span>{msg.sender}</span>
                                            <span>" • "</span>
                                            <span>{msg.time}</span>
                                        </div>
                                    </div>
                                }
                            }
                        />
                    }.into_view()
                }
            }}
        </main>

        <footer class="input-bar">
            <input
                type="text"
                placeholder={move || if is_connected() { "Type an encrypted message..." } else { "Waiting for peer to establish connection..." }}
                prop:value=move || input_text.get()
                on:input=move |ev| {
                    let target: HtmlInputElement = event_target(&ev);
                    set_input_text.set(target.value());
                }
                on:keydown=move |ev| {
                    if ev.key() == "Enter" {
                        send_message();
                    }
                }
            />
            <button
                class="btn btn-primary"
                disabled=move || !is_connected() || input_text.get().trim().is_empty()
                on:click=move |_| send_message()
            >
                "Send"
            </button>
        </footer>

        // Incoming Call Modal
        {move || match call_state.get() {
            CallState::Incoming(c_type) => view! {
                <div class="modal-backdrop">
                    <div class="incoming-call-box">
                        <h3>{format!("📞 Incoming {} Session", c_type.label())}</h3>
                        <p>{format!("Your peer wants to start an encrypted {} session.", c_type.label())}</p>
                        <div style="display: flex; gap: 12px; width: 100%; justify-content: center;">
                            <button
                                class="btn btn-call"
                                style="padding: 10px 20px; font-size: 1rem;"
                                on:click=move |_| handle_accept_call(c_type)
                            >
                                "🟢 Accept"
                            </button>
                            <button
                                class="btn btn-danger"
                                style="padding: 10px 20px; font-size: 1rem;"
                                on:click=handle_reject_call
                            >
                                "🔴 Decline"
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
                            <h3>"Scan with Phone Camera"</h3>
                            <p>"Scan to instantly open this room with the zero-knowledge decryption key included in the URL fragment."</p>
                            <div class="qr-wrapper" inner_html=qr_svg></div>
                            <button
                                class="btn btn-secondary"
                                style="width: 100%;"
                                on:click=move |_| set_show_qr.set(false)
                            >
                                "Close"
                            </button>
                        </div>
                    </div>
                }.into_view()
            } else {
                view! { <div></div> }.into_view()
            }
        }}

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
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(|| view! { <App/> });
}
