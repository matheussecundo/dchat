mod i18n;
mod media;
mod mesh;
mod names;
mod nostr_pool;
mod qr;
mod session;
mod state;

use i18n::{detect_browser_language, t, t_replace_1, update_document_direction, Language};
use leptos::*;
use names::{pubkey_tag, random_name, sanitize_name, MAX_NAME_CHARS};
use protocol::{parse_cap, VideoKind, DEFAULT_MEMBER_CAP, DEFAULT_VIDEO_CAP, DEFAULT_VOICE_CAP};
use qr::generate_qr_svg;
use session::{RoomSession, SessionSignals};
use state::{
    admin_url, create_room, invite_url, read_credentials, AudioSettings, ChatMessageUi,
    ConnectionStatus, LinkUi, LoungeMemberUi, MemberUi, MyVoiceUi, Notice, RoomCaps,
};
use std::collections::{HashMap, HashSet};
use web_sys::{window, HtmlInputElement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    /// No room in the URL: pick a name and the room settings.
    Create,
    /// A room link was opened: pick a name, then enter.
    Join,
    Room,
    /// This tab lost the deterministic race for the last seat.
    Full,
}

#[component]
fn App() -> impl IntoView {
    let (lang, set_lang) = create_signal(detect_browser_language());
    create_effect(move |_| {
        update_document_direction(lang.get());
    });

    let initial_screen = if read_credentials().is_some() { Screen::Join } else { Screen::Create };
    let (screen, set_screen) = create_signal(initial_screen);
    let (name_input, set_name_input) = create_signal(random_name());
    let (cap_input, set_cap_input) = create_signal(DEFAULT_MEMBER_CAP.to_string());
    let (voice_cap_input, set_voice_cap_input) = create_signal(DEFAULT_VOICE_CAP.to_string());
    let (video_cap_input, set_video_cap_input) = create_signal(DEFAULT_VIDEO_CAP.to_string());

    let (status, set_status) = create_signal(ConnectionStatus::Idle);
    let (messages, set_messages) = create_signal(Vec::<ChatMessageUi>::new());
    let (members, set_members) = create_signal(Vec::<MemberUi>::new());
    let (names, set_names) = create_signal(HashMap::<String, String>::new());
    let (room_full, set_room_full) = create_signal(false);
    let (connected_relays, set_connected_relays) = create_signal(0usize);
    let (input_text, set_input_text) = create_signal(String::new());
    let (show_qr, set_show_qr) = create_signal(false);
    let (show_relays, set_show_relays) = create_signal(false);
    let (show_members, set_show_members) = create_signal(false);
    let (copied, set_copied) = create_signal(false);
    let (room_id_sig, set_room_id_sig) = create_signal(String::new());
    let (toast, set_toast) = create_signal(Option::<&'static str>::None);

    // Voice lounge
    let (lounge, set_lounge) = create_signal(Vec::<LoungeMemberUi>::new());
    let (my_voice, set_my_voice) = create_signal(MyVoiceUi::default());
    let (speaking, set_speaking) = create_signal(HashSet::<String>::new());
    let (voice_prompt, set_voice_prompt) = create_signal(Option::<String>::None);
    let (audio_settings, set_audio_settings) = create_signal(AudioSettings::default());
    let (show_audio_settings, set_show_audio_settings) = create_signal(false);

    let session_ref = store_value(None::<RoomSession>);

    let enter_room = move || {
        let Some((room_id, key)) = read_credentials() else {
            return;
        };
        let signals = SessionSignals {
            status: set_status,
            messages: set_messages,
            members: set_members,
            names: set_names,
            room_full: set_room_full,
            connected_relays: set_connected_relays,
            lounge: set_lounge,
            my_voice: set_my_voice,
            speaking: set_speaking,
            voice_prompt: set_voice_prompt,
            toast: set_toast,
        };
        set_room_id_sig.set(room_id.clone());
        let name = sanitize_name(&name_input.get_untracked());
        match RoomSession::start(room_id, key, name, signals) {
            Ok(session) => {
                session_ref.set_value(Some(session));
                set_screen.set(Screen::Room);
            }
            Err(err) => set_status.set(ConnectionStatus::Error(err)),
        }
    };

    let create_and_enter = move || {
        let caps = RoomCaps {
            members: parse_cap(Some(&cap_input.get_untracked()), DEFAULT_MEMBER_CAP),
            voice: parse_cap(Some(&voice_cap_input.get_untracked()), DEFAULT_VOICE_CAP),
            video: parse_cap(Some(&video_cap_input.get_untracked()), DEFAULT_VIDEO_CAP),
        };
        match create_room(caps) {
            Ok(()) => enter_room(),
            Err(err) => set_status.set(ConnectionStatus::Error(err)),
        }
    };

    create_effect(move |_| {
        if room_full.get() {
            set_screen.set(Screen::Full);
        }
    });

    let send_message = move || {
        let text = input_text.get_untracked().trim().to_string();
        if text.is_empty() {
            return;
        }
        session_ref.with_value(|session| {
            if let Some(session) = session {
                match session.send_chat(&text) {
                    Ok(()) => set_input_text.set(String::new()),
                    Err(err) => log::warn!("Failed to send message: {err}"),
                }
            }
        });
    };

    let write_clipboard = |text: &str| {
        if let Some(win) = window() {
            let _ = win.navigator().clipboard().write_text(text);
        }
    };

    let copy_invite_link = move |_| {
        write_clipboard(&invite_url());
        set_copied.set(true);
        set_timeout(move || set_copied.set(false), std::time::Duration::from_secs(2));
    };

    let copy_admin_link = move |_| {
        if let Some(url) = admin_url() {
            write_clipboard(&url);
            set_toast.set(Some("toast_admin_copied"));
        }
    };

    let destroy_session = move |_| {
        session_ref.with_value(|session| {
            if let Some(session) = session {
                session.leave();
            }
        });
        if let Some(win) = window() {
            let _ = win.location().set_href("/");
        }
    };

    let with_session = move |f: &dyn Fn(&RoomSession)| {
        session_ref.with_value(|session| {
            if let Some(session) = session {
                f(session);
            }
        });
    };
    let join_voice = move |_| {
        set_voice_prompt.set(None);
        with_session(&|s| s.join_voice());
    };
    let apply_audio_settings = move |settings: AudioSettings| {
        set_audio_settings.set(settings);
        with_session(&|s| s.set_audio_settings(settings));
    };
    let open_audio_settings = move |_| {
        // The modal lives outside the video grid, so it would be hidden in fullscreen.
        if let Some(doc) = window().and_then(|w| w.document()) {
            if doc.fullscreen_element().is_some() {
                doc.exit_fullscreen();
            }
        }
        set_show_audio_settings.set(true);
    };
    let enter_fullscreen = move |_| {
        if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("video-grid")) {
            let _ = el.request_fullscreen();
        }
    };
    let voice_cap = move || session_ref.with_value(|s| s.as_ref().and_then(|s| s.voice_cap()));
    let video_cap = move || session_ref.with_value(|s| s.as_ref().and_then(|s| s.video_cap()));
    let voice_full = move || {
        !my_voice.get().in_voice && voice_cap().is_some_and(|cap| lounge.with(|l| l.len()) >= cap)
    };
    let video_full = move || {
        video_cap().is_some_and(|cap| {
            lounge.with(|l| l.iter().filter(|m| !m.is_self && m.video != VideoKind::None).count()) >= cap
        })
    };
    let video_members = move || {
        lounge.with(|l| l.iter().filter(|m| m.video != VideoKind::None).cloned().collect::<Vec<_>>())
    };
    let show_video_grid = create_memo(move |_| {
        my_voice.get().in_voice && lounge.with(|l| l.iter().any(|m| m.video != VideoKind::None))
    });
    // Video elements are recreated when the grid appears: point them at their streams.
    create_effect(move |_| {
        if show_video_grid.get() {
            with_session(&|s| s.attach_lounge_media());
        }
    });
    create_effect(move |_| {
        if let Some(name) = voice_prompt.get() {
            set_timeout(
                move || {
                    if voice_prompt.get_untracked().as_deref() == Some(name.as_str()) {
                        set_voice_prompt.set(None);
                    }
                },
                std::time::Duration::from_secs(10),
            );
        }
    });

    let is_connected = move || status.get() == ConnectionStatus::Connected;
    let display_name = move |pubkey: &str| {
        names.with(|n| n.get(pubkey).cloned()).unwrap_or_else(|| pubkey_tag(pubkey))
    };

    let name_field = move || {
        view! {
            <label class="lobby-label" for="name-input">{move || t(lang.get(), "your_name_label")}</label>
            <input
                id="name-input"
                class="lobby-input"
                type="text"
                maxlength=MAX_NAME_CHARS
                prop:value=move || name_input.get()
                on:input=move |ev| set_name_input.set(event_target_value(&ev))
            />
        }
    };

    let create_view = move || {
        let cap_is_large = move || {
            [
                (cap_input.get(), DEFAULT_MEMBER_CAP),
                (voice_cap_input.get(), DEFAULT_VOICE_CAP),
                (video_cap_input.get(), DEFAULT_VIDEO_CAP),
            ]
            .iter()
            .any(|(input, default)| parse_cap(Some(input), *default).map_or(true, |cap| cap > *default))
        };
        view! {
            <div class="lobby">
                <form class="lobby-card" on:submit=move |ev| { ev.prevent_default(); create_and_enter(); }>
                    <h2>{move || t(lang.get(), "create_title")}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "create_desc")}</p>
                    {name_field}
                    <label class="lobby-label" for="cap-input">{move || t(lang.get(), "member_cap_label")}</label>
                    <input
                        id="cap-input"
                        class="lobby-input"
                        type="number"
                        min="0"
                        prop:value=move || cap_input.get()
                        on:input=move |ev| set_cap_input.set(event_target_value(&ev))
                    />
                    <div class="lobby-row">
                        <div class="lobby-field">
                            <label class="lobby-label" for="voice-cap-input">{move || t(lang.get(), "voice_cap_label")}</label>
                            <input
                                id="voice-cap-input"
                                class="lobby-input"
                                type="number"
                                min="0"
                                prop:value=move || voice_cap_input.get()
                                on:input=move |ev| set_voice_cap_input.set(event_target_value(&ev))
                            />
                        </div>
                        <div class="lobby-field">
                            <label class="lobby-label" for="video-cap-input">{move || t(lang.get(), "video_cap_label")}</label>
                            <input
                                id="video-cap-input"
                                class="lobby-input"
                                type="number"
                                min="0"
                                prop:value=move || video_cap_input.get()
                                on:input=move |ev| set_video_cap_input.set(event_target_value(&ev))
                            />
                        </div>
                    </div>
                    {move || cap_is_large().then(|| view! {
                        <p class="lobby-warning">{move || t(lang.get(), "cap_warning")}</p>
                    })}
                    <button id="create-room-btn" type="submit" class="btn btn-primary lobby-submit">
                        {move || t(lang.get(), "btn_create_room")}
                    </button>
                </form>
            </div>
        }
    };

    let join_view = move || {
        let room = read_credentials().map(|(room, _)| room).unwrap_or_default();
        view! {
            <div class="lobby">
                <form class="lobby-card" on:submit=move |ev| { ev.prevent_default(); enter_room(); }>
                    <h2>{move || t_replace_1(lang.get(), "join_title", "{room}", &room)}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "join_desc")}</p>
                    {name_field}
                    <button id="enter-room-btn" type="submit" class="btn btn-primary lobby-submit">
                        {move || t(lang.get(), "btn_enter_room")}
                    </button>
                </form>
            </div>
        }
    };

    let full_view = move || {
        view! {
            <div class="lobby">
                <div class="lobby-card room-full">
                    <h2>{move || t(lang.get(), "room_full_title")}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "room_full_desc")}</p>
                    <button
                        id="try-again-btn"
                        class="btn btn-primary lobby-submit"
                        on:click=move |_| {
                            if let Some(win) = window() {
                                let _ = win.location().reload();
                            }
                        }
                    >
                        {move || t(lang.get(), "btn_try_again")}
                    </button>
                </div>
            </div>
        }
    };

    let message_view = move |msg: ChatMessageUi| {
        if let Some(notice) = msg.notice {
            return view! {
                <div class="system-notice">
                    {move || match &notice {
                        Notice::Joined(name) => t_replace_1(lang.get(), "sys_joined", "{name}", name),
                        Notice::Left(name) => t_replace_1(lang.get(), "sys_left", "{name}", name),
                        Notice::LateJoin => t(lang.get(), "sys_late_join").to_string(),
                    }}
                </div>
            }
            .into_view();
        }
        let row_class = if msg.is_self { "message-row self" } else { "message-row peer" };
        let author = msg.author.clone();
        view! {
            <div class=row_class>
                <div class="message-bubble" dir="auto">{msg.text}</div>
                <div class="message-meta">
                    <span class="message-author" dir="auto">{move || display_name(&author)}</span>
                    <span class="message-tag">{format!(" · {}", pubkey_tag(&msg.author))}</span>
                    <span>" • "</span>
                    <span>{msg.time}</span>
                </div>
            </div>
        }
        .into_view()
    };

    let room_view = move || {
        let is_admin_link = admin_url().is_some();
        view! {
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
                        title=move || t(lang.get(), "nostr_relays_title")
                    >
                        <span>{move || t(lang.get(), "nostr_badge")}</span>
                        <span>{move || {
                            let c = connected_relays.get();
                            if c > 0 { format!("({} active)", c) } else { "(connecting...)".to_string() }
                        }}</span>
                    </button>
                </div>
                <div class="bar-actions">
                    <button
                        class="btn btn-secondary members-toggle"
                        on:click=move |_| set_show_members.update(|open| *open = !*open)
                        title=move || t(lang.get(), "members_toggle_title")
                    >
                        {move || format!("👥 {}", members.get().len())}
                    </button>
                    <button class="btn btn-secondary copy-invite-btn" on:click=copy_invite_link>
                        {move || if copied.get() { t(lang.get(), "btn_copied") } else { t(lang.get(), "btn_copy_link") }}
                    </button>
                    {is_admin_link.then(|| view! {
                        <button class="btn btn-secondary copy-admin-btn" on:click=copy_admin_link>
                            {move || t(lang.get(), "btn_copy_admin")}
                        </button>
                    })}
                </div>
            </div>

            <div class="lounge-bar">
                <div class="lounge-info">
                    <span class="lounge-title">{move || format!("🔊 {}", t(lang.get(), "voice_title"))}</span>
                    <span class="lounge-count">{move || {
                        let n = lounge.with(|l| l.len());
                        match voice_cap() {
                            Some(cap) => format!("{n}/{cap}"),
                            None => n.to_string(),
                        }
                    }}</span>
                    <div class="voice-chips">
                        <For
                            each=move || lounge.get()
                            key=|m| m.clone()
                            children=move |m| voice_chip(lang, m, speaking)
                        />
                        {move || lounge.with(|l| l.is_empty()).then(|| view! {
                            <span class="voice-empty">{move || t(lang.get(), "voice_empty")}</span>
                        })}
                    </div>
                </div>
                <div class="lounge-controls">
                    {move || {
                        let mine = my_voice.get();
                        if !mine.in_voice {
                            return view! {
                                <button
                                    id="join-voice-btn"
                                    class="btn btn-call"
                                    disabled=move || my_voice.get().joining || voice_full() || !is_connected()
                                    on:click=join_voice
                                >
                                    {move || if voice_full() { t(lang.get(), "voice_full") } else { t(lang.get(), "btn_join_voice") }}
                                </button>
                            }.into_view();
                        }
                        view! {
                            <button
                                id="mic-btn"
                                class=if mine.mic_muted { "btn btn-mute muted" } else { "btn btn-mute" }
                                on:click=move |_| with_session(&|s| s.toggle_mic())
                                title=move || t(lang.get(), "title_mute_mic")
                            >
                                {if mine.mic_muted { "🔇" } else { "🎙️" }}
                            </button>
                            <button
                                id="speaker-btn"
                                class=if mine.speaker_muted { "btn btn-mute muted" } else { "btn btn-mute" }
                                on:click=move |_| with_session(&|s| s.toggle_speaker())
                                title=move || t(lang.get(), "title_mute_speaker")
                            >
                                {if mine.speaker_muted { "🔈" } else { "🔊" }}
                            </button>
                            <button
                                id="camera-btn"
                                class=if mine.video == VideoKind::Camera { "btn btn-mute active" } else { "btn btn-mute" }
                                disabled=move || mine.video == VideoKind::None && video_full()
                                on:click=move |_| with_session(&|s| s.toggle_camera())
                                title=move || if mine.video == VideoKind::None && video_full() { t(lang.get(), "video_full_title") } else { t(lang.get(), "title_camera") }
                            >
                                "📹"
                            </button>
                            {(mine.video == VideoKind::Camera).then(|| view! {
                                <button
                                    id="flip-camera-btn"
                                    class="btn btn-secondary"
                                    on:click=move |_| with_session(&|s| s.flip_camera())
                                    title=move || t(lang.get(), "title_flip_camera")
                                >
                                    "🔄"
                                </button>
                            })}
                            <button
                                id="screen-btn"
                                class=if mine.video == VideoKind::Screen { "btn btn-mute active" } else { "btn btn-mute" }
                                disabled=move || mine.video == VideoKind::None && video_full()
                                on:click=move |_| with_session(&|s| s.toggle_screen())
                                title=move || if mine.video == VideoKind::Screen { t(lang.get(), "title_stop_screen") } else { t(lang.get(), "title_screen_share") }
                            >
                                "🖥️"
                            </button>
                            <button
                                id="leave-voice-btn"
                                class="btn btn-danger"
                                on:click=move |_| with_session(&|s| s.leave_voice())
                                title=move || t(lang.get(), "title_leave_voice")
                            >
                                {move || t(lang.get(), "btn_leave_voice")}
                            </button>
                        }.into_view()
                    }}
                    <button
                        id="audio-settings-btn"
                        class="btn btn-secondary"
                        on:click=open_audio_settings
                        title=move || t(lang.get(), "btn_audio_settings_title")
                    >
                        "⚙️"
                    </button>
                </div>
            </div>

            {move || voice_prompt.get().filter(|_| !my_voice.get().in_voice).map(|name| view! {
                <div id="voice-prompt" class="voice-prompt">
                    <span>{move || t_replace_1(lang.get(), "voice_joined_prompt", "{name}", &name)}</span>
                    <button id="voice-prompt-join" class="btn btn-call btn-sm" on:click=join_voice>
                        {move || t(lang.get(), "btn_join")}
                    </button>
                    <button
                        class="btn btn-secondary btn-sm"
                        title=move || t(lang.get(), "title_dismiss")
                        on:click=move |_| set_voice_prompt.set(None)
                    >
                        "✕"
                    </button>
                </div>
            })}

            {move || show_video_grid.get().then(|| view! {
                <div id="video-grid" class="video-grid">
                    <For
                        each=video_members
                        key=|m| (m.pubkey.clone(), m.video)
                        children=move |m| video_tile(lang, m, lounge, speaking)
                    />
                    <button class="btn btn-secondary grid-fullscreen" on:click=enter_fullscreen title=move || t(lang.get(), "title_fullscreen")>
                        "⛶"
                    </button>
                </div>
            })}

            <div class="room-body">
                <main class="chat-container">
                    {move || {
                        if messages.with(|m| m.is_empty()) {
                            view! {
                                <div class="empty-state">
                                    <h3>{move || t(lang.get(), "empty_title")}</h3>
                                    <p>{move || t(lang.get(), "empty_desc")}</p>
                                    <div class="security-checklist">
                                        <li>{move || t(lang.get(), "check_e2ee")}</li>
                                        <li>{move || t(lang.get(), "check_audio")}</li>
                                        <li>{move || t(lang.get(), "check_video")}</li>
                                        <li>{move || t(lang.get(), "check_zk")}</li>
                                        <li>{move || t(lang.get(), "check_zero_storage")}</li>
                                        <li>{move || t(lang.get(), "check_destruction")}</li>
                                    </div>
                                </div>
                            }
                            .into_view()
                        } else {
                            view! {
                                <For
                                    each=move || messages.get()
                                    key=|msg| msg.id.clone()
                                    children=message_view
                                />
                            }
                            .into_view()
                        }
                    }}
                </main>

                <aside class=move || if show_members.get() { "members-panel open" } else { "members-panel" }>
                    <div class="members-header">
                        <h4>{move || t(lang.get(), "members_title")}</h4>
                        <span class="members-count">{move || members.get().len()}</span>
                    </div>
                    <ul class="members-list">
                        <For
                            each=move || members.get()
                            key=|m| (m.pubkey.clone(), m.name.clone(), m.is_admin, m.link.clone())
                            children=move |m| member_row(lang, m)
                        />
                    </ul>
                </aside>
            </div>

            <footer class="input-bar">
                <input
                    type="text"
                    placeholder=move || {
                        if is_connected() { t(lang.get(), "placeholder_connected") } else { t(lang.get(), "placeholder_waiting") }
                    }
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
                    class="btn btn-primary send-btn"
                    disabled=move || !is_connected() || input_text.get().trim().is_empty()
                    on:click=move |_| send_message()
                >
                    {move || t(lang.get(), "btn_send")}
                </button>
            </footer>
        }
    };

    view! {
        // Any tap can unlock remote audio the browser held back for lack of a gesture.
        <div id="app" on:click=move |_| media::resume_remote_audio()>
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
                            if let Some(l) = Language::from_code(&event_target_value(&ev)) {
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
                    {move || (screen.get() == Screen::Room).then(|| view! {
                        <button class="btn btn-secondary" on:click=move |_| set_show_qr.set(true)>
                            {move || t(lang.get(), "scan_qr")}
                        </button>
                        <button
                            class="btn btn-danger"
                            on:click=destroy_session
                            title=move || t(lang.get(), "wipe_session_title")
                        >
                            {move || t(lang.get(), "wipe_session")}
                        </button>
                    })}
                </div>
            </header>

            {move || match screen.get() {
                Screen::Create => create_view().into_view(),
                Screen::Join => join_view().into_view(),
                Screen::Room => room_view().into_view(),
                Screen::Full => full_view().into_view(),
            }}

            // QR code of the invite link (never the admin link) for phone pairing
            {move || show_qr.get().then(|| {
                let qr_svg = generate_qr_svg(&invite_url()).unwrap_or_default();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_qr.set(false)>
                        <div class="modal-content" on:click=|ev| ev.stop_propagation()>
                            <h3>{move || t(lang.get(), "qr_title")}</h3>
                            <p>{move || t(lang.get(), "qr_desc")}</p>
                            <div class="qr-wrapper" inner_html=qr_svg></div>
                            <button class="btn btn-secondary" style="width: 100%;" on:click=move |_| set_show_qr.set(false)>
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }
            })}

            // Nostr relays modal
            {move || show_relays.get().then(|| {
                let relays_list = state::get_default_relays();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_relays.set(false)>
                        <div class="modal-content relays-modal" on:click=|ev| ev.stop_propagation()>
                            <div class="modal-title-row">
                                <h3>{move || t(lang.get(), "nostr_relays_title")}</h3>
                                <button class="btn btn-secondary" on:click=move |_| set_show_relays.set(false)>"✕"</button>
                            </div>
                            <p class="relays-desc">{move || t(lang.get(), "nostr_relays_desc")}</p>
                            <div class="relays-box">
                                <div class="relays-box-title">{move || t(lang.get(), "nostr_active_relays")}</div>
                                <ul class="relays-list">
                                    {relays_list.into_iter().map(|r| view! {
                                        <li><span class="relay-dot">"●"</span><span>{r}</span></li>
                                    }).collect_view()}
                                </ul>
                            </div>
                            <button class="btn btn-primary" style="width: 100%;" on:click=move |_| set_show_relays.set(false)>
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }
            })}

            // Audio settings modal
            {move || show_audio_settings.get().then(|| {
                let (ns_supported, ec_supported, agc_supported) = media::supported_audio_constraints();
                view! {
                    <div class="modal-backdrop" on:click=move |_| set_show_audio_settings.set(false)>
                        <div class="modal-content audio-settings-modal" on:click=|ev| ev.stop_propagation()>
                            <div class="modal-title-row">
                                <h3>{move || t(lang.get(), "audio_settings_title")}</h3>
                                <button class="btn btn-secondary" on:click=move |_| set_show_audio_settings.set(false)>"✕"</button>
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
                            <button class="btn btn-primary" style="width: 100%;" on:click=move |_| set_show_audio_settings.set(false)>
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }
            })}

            {move || toast.get().map(|key| {
                set_timeout(move || set_toast.set(None), std::time::Duration::from_secs(3));
                view! { <div class="toast">{move || t(lang.get(), key)}</div> }
            })}
        </div>
    }
}

/// A lounge member chip: name, mic/video state, speaking highlight, media reachability.
fn voice_chip(lang: ReadSignal<Language>, member: LoungeMemberUi, speaking: ReadSignal<HashSet<String>>) -> impl IntoView {
    let pubkey = member.pubkey.clone();
    let video = match member.video {
        VideoKind::None => "none",
        VideoKind::Camera => "camera",
        VideoKind::Screen => "screen",
    };
    view! {
        <span
            class="voice-chip"
            class:self-chip=member.is_self
            data-pubkey=member.pubkey.clone()
            data-mic=if member.mic_muted { "off" } else { "on" }
            data-video=video
            data-speaking=move || speaking.with(|s| s.contains(&pubkey)).to_string()
        >
            <span class="voice-chip-name" dir="auto">{member.name}</span>
            {member.mic_muted.then(|| "🔇")}
            {match member.video {
                VideoKind::Camera => Some("📹"),
                VideoKind::Screen => Some("🖥️"),
                VideoKind::None => None,
            }}
            {(!member.has_media_link).then(|| view! {
                <span class="voice-chip-warn" title=move || t(lang.get(), "no_direct_media")>"⚠"</span>
            })}
        </span>
    }
}

/// A video tile in the lounge grid; the session attaches the stream by element id.
fn video_tile(
    lang: ReadSignal<Language>,
    member: LoungeMemberUi,
    lounge: ReadSignal<Vec<LoungeMemberUi>>,
    speaking: ReadSignal<HashSet<String>>,
) -> impl IntoView {
    let pk_speaking = member.pubkey.clone();
    let pk_mic = member.pubkey.clone();
    let is_self = member.is_self;
    view! {
        <div
            class="tile"
            class:speaking=move || speaking.with(|s| s.contains(&pk_speaking))
            data-pubkey=member.pubkey.clone()
        >
            <video id=format!("tile-video-{}", member.pubkey) autoplay playsinline muted></video>
            <span class="tile-label">
                <span dir="auto">{member.name}</span>
                {move || is_self.then(|| format!(" {}", t(lang.get(), "you_suffix")))}
                {move || lounge.with(|l| l.iter().any(|m| m.pubkey == pk_mic && m.mic_muted)).then(|| " 🔇")}
            </span>
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

/// One member in the side panel: name, key tag, admin badge and how we reach them.
fn member_row(lang: ReadSignal<Language>, member: MemberUi) -> impl IntoView {
    let (kind, dot) = match member.link {
        LinkUi::Me => ("me", "link-me"),
        LinkUi::Direct => ("direct", "link-direct"),
        LinkUi::Via(_) => ("via", "link-via"),
        LinkUi::Connecting => ("connecting", "link-connecting"),
    };
    let link = member.link.clone();
    let is_via = matches!(member.link, LinkUi::Via(_));
    view! {
        <li class="member-row" data-pubkey=member.pubkey.clone() data-link=kind>
            <span class=format!("member-dot {dot}")></span>
            <span class="member-name" dir="auto">{member.name}</span>
            <span class="member-tag">{format!("· {}", member.tag)}</span>
            {member.is_admin.then(|| view! {
                <span class="member-badge">{move || t(lang.get(), "admin_badge")}</span>
            })}
            <span
                class="member-link"
                title=move || if is_via { t(lang.get(), "link_via_title") } else { "" }
            >
                {move || match &link {
                    LinkUi::Me => t(lang.get(), "you_suffix").to_string(),
                    LinkUi::Direct => t(lang.get(), "link_direct").to_string(),
                    LinkUi::Via(name) => t_replace_1(lang.get(), "link_via", "{name}", name),
                    LinkUi::Connecting => t(lang.get(), "link_connecting").to_string(),
                }}
            </span>
        </li>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(|| view! { <App/> });
}
