mod i18n;
mod media;
mod mesh;
mod names;
mod nostr_pool;
mod qr;
mod session;
mod state;

use i18n::{
    detect_browser_language, large_file_warning_desc, t, t_replace_1, update_document_direction,
    Language,
};
use leptos::*;
use names::{pubkey_tag, random_name, sanitize_name, MAX_NAME_CHARS};
use protocol::{parse_cap, VideoKind, DEFAULT_MEMBER_CAP, DEFAULT_VIDEO_CAP, DEFAULT_VOICE_CAP, REACTIONS};
use qr::generate_qr_svg;
use session::{RoomSession, SessionSignals};
use state::{
    admin_url, create_room, format_file_size, invite_url, read_credentials, AudioSettings,
    ChatMessageUi, ConnectionStatus, DmUi, FileOfferInfo, FileTransferStatus, LinkUi,
    LoungeMemberUi, MemberUi, MyVoiceUi, Notice, RekeyTarget, RoomCaps,
};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::JsCast;
use web_sys::{window, HtmlInputElement};

/// Without direct-to-disk streaming, downloads above this size are buffered in RAM: warn first.
const LARGE_FILE_BYTES: u64 = 250 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    /// No room in the URL: pick a name and the room settings.
    Create,
    /// A room link was opened: pick a name, then enter.
    Join,
    Room,
    /// This tab lost the deterministic race for the last seat.
    Full,
    /// An admin removed this tab from the room.
    Removed,
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
    let (history_input, set_history_input) = create_signal(false);

    let (status, set_status) = create_signal(ConnectionStatus::Idle);
    let (messages, set_messages) = create_signal(Vec::<ChatMessageUi>::new());
    let (members, set_members) = create_signal(Vec::<MemberUi>::new());
    let (names, set_names) = create_signal(HashMap::<String, String>::new());
    let (room_full, set_room_full) = create_signal(false);
    let (removed, set_removed) = create_signal(false);
    let (rekey, set_rekey) = create_signal(None::<RekeyTarget>);
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

    // Chat extras
    let (typing, set_typing) = create_signal(Vec::<String>::new());
    let (mention_count, set_mention_count) = create_signal(0usize);
    let (dms, set_dms) = create_signal(HashMap::<String, Vec<DmUi>>::new());
    let (dm_unread, set_dm_unread) = create_signal(HashMap::<String, usize>::new());
    let (dm_open, set_dm_open) = create_signal(Option::<String>::None);
    let (dm_input, set_dm_input) = create_signal(String::new());
    let (editing, set_editing) = create_signal(Option::<String>::None);
    let (react_picker, set_react_picker) = create_signal(Option::<String>::None);

    // File sharing
    let (staged_file, set_staged_file) = create_signal(Option::<web_sys::File>::None);
    let (large_file_warning, set_large_file_warning) = create_signal(Option::<(String, String)>::None);
    let (show_audio_settings, set_show_audio_settings) = create_signal(false);

    let session_ref = store_value(None::<RoomSession>);
    // The session name, reused when an admin moves the room to a new link.
    let my_name = store_value(String::new());

    let start_session = move |room_id: String, key: [u8; protocol::KEY_LENGTH], migrated: bool| -> Option<RoomSession> {
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
            removed: set_removed,
            rekey: set_rekey,
            typing: set_typing,
            mention: set_mention_count,
            dms: set_dms,
            dm_unread: set_dm_unread,
        };
        set_room_id_sig.set(room_id.clone());
        match RoomSession::start(room_id, key, my_name.get_value(), signals, migrated) {
            Ok(session) => {
                session_ref.set_value(Some(session.clone()));
                set_screen.set(Screen::Room);
                Some(session)
            }
            Err(err) => {
                set_status.set(ConnectionStatus::Error(err));
                None
            }
        }
    };

    let enter_room = move || {
        let Some((room_id, key)) = read_credentials() else {
            return;
        };
        my_name.set_value(sanitize_name(&name_input.get_untracked()));
        start_session(room_id, key, false);
    };

    // An admin moved the room: follow it with a fresh session, keeping the chat on screen.
    create_effect(move |_| {
        if let Some(target) = rekey.get() {
            set_rekey.set(None);
            if let Some(session) = start_session(target.room, target.key, true) {
                if target.rejoin_voice {
                    session.join_voice();
                }
            }
        }
    });
    create_effect(move |_| {
        if removed.get() {
            set_screen.set(Screen::Removed);
        }
    });

    let create_and_enter = move || {
        let caps = RoomCaps {
            members: parse_cap(Some(&cap_input.get_untracked()), DEFAULT_MEMBER_CAP),
            voice: parse_cap(Some(&voice_cap_input.get_untracked()), DEFAULT_VOICE_CAP),
            video: parse_cap(Some(&video_cap_input.get_untracked()), DEFAULT_VIDEO_CAP),
            history: history_input.get_untracked(),
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
        if let Some(target) = editing.get_untracked() {
            session_ref.with_value(|s| {
                if let Some(s) = s {
                    s.edit_message(&target, &text);
                }
            });
            set_editing.set(None);
            set_input_text.set(String::new());
            return;
        }
        let staged = staged_file.get_untracked();
        if text.is_empty() && staged.is_none() {
            return;
        }
        session_ref.with_value(|session| {
            let Some(session) = session else {
                return;
            };
            let result = match staged {
                Some(file) => session.share_file(file, (!text.is_empty()).then_some(text)),
                None => session.send_chat(&text),
            };
            match result {
                Ok(()) => {
                    set_input_text.set(String::new());
                    set_staged_file.set(None);
                }
                Err(err) => log::warn!("Failed to send: {err}"),
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

    let download_file = move |file_id: String| {
        let large = messages.with(|msgs| {
            msgs.iter()
                .find(|m| m.id == file_id)
                .and_then(|m| m.file.as_ref())
                .filter(|f| f.size > LARGE_FILE_BYTES)
                .map(|f| f.name.clone())
        });
        let has_picker = window()
            .and_then(|w| js_sys::Reflect::get(&w, &"showSaveFilePicker".into()).ok())
            .is_some_and(|f| f.is_function());
        match large {
            Some(name) if !has_picker => set_large_file_warning.set(Some((file_id, name))),
            _ => with_session(&|s| s.download_file(&file_id)),
        }
    };
    let file_action = move |action: FileAction, file_id: String| match action {
        FileAction::Download => download_file(file_id),
        FileAction::Decline => with_session(&|s| s.decline_file(&file_id)),
        FileAction::Cancel => with_session(&|s| s.cancel_download(&file_id)),
        FileAction::Withdraw => with_session(&|s| s.withdraw_file(&file_id)),
    };
    let has_messages = create_memo(move |_| messages.with(|m| !m.is_empty()));
    let start_edit = move |id: String, text: String| {
        set_editing.set(Some(id));
        set_input_text.set(text);
        if let Some(input) = window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector("footer.input-bar input[type=text]").ok().flatten())
            .and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
        {
            let _ = input.focus();
        }
    };
    let delete_message = move |id: String| {
        let ok = window()
            .and_then(|w| w.confirm_with_message(t(lang.get_untracked(), "confirm_delete")).ok())
            .unwrap_or(false);
        if ok {
            with_session(&|s| s.delete_message(&id));
        }
    };
    let react = move |id: String, emoji: &'static str| {
        set_react_picker.set(None);
        with_session(&|s| s.toggle_reaction(&id, emoji));
    };
    let open_dm = move |pubkey: String| {
        set_dm_unread.update(|u| {
            u.remove(&pubkey);
        });
        set_dm_open.set(Some(pubkey));
    };
    // Reading an open conversation clears its unread count.
    create_effect(move |_| {
        if let Some(peer) = dm_open.get() {
            if dm_unread.with(|u| u.get(&peer).copied().unwrap_or(0)) > 0 {
                set_dm_unread.update(|u| {
                    u.remove(&peer);
                });
            }
        }
    });
    let send_dm = move || {
        let (Some(peer), text) = (dm_open.get_untracked(), dm_input.get_untracked()) else {
            return;
        };
        let sent = session_ref.with_value(|s| s.as_ref().map(|s| s.send_dm(&peer, &text)));
        match sent {
            Some(Ok(())) => set_dm_input.set(String::new()),
            Some(Err(err)) => log::warn!("DM not sent: {err}"),
            None => {}
        }
    };
    // @mentions: chime, and a "(n)" title badge while the tab is in the background.
    let base_title = window().and_then(|w| w.document()).map(|d| d.title()).unwrap_or_default();
    let page_hidden = || window().and_then(|w| w.document()).is_some_and(|d| d.hidden());
    create_effect(move |previous: Option<usize>| {
        let count = mention_count.get();
        if count > previous.unwrap_or(0) {
            media::play_chime();
        }
        if let Some(doc) = window().and_then(|w| w.document()) {
            if count > 0 && page_hidden() {
                doc.set_title(&format!("({count}) {base_title}"));
            } else {
                doc.set_title(&base_title);
                if count > 0 {
                    set_mention_count.set(0);
                }
            }
        }
        count
    });
    {
        let on_visible = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            if !page_hidden() {
                set_mention_count.set(0);
            }
        }) as Box<dyn FnMut()>);
        if let Some(doc) = window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("visibilitychange", on_visible.as_ref().unchecked_ref());
        }
        on_visible.forget();
    }
    let am_admin = create_memo(move |_| members.with(|m| m.iter().any(|x| x.link == LinkUi::Me && x.is_admin)));
    let confirm = |text: String| window().and_then(|w| w.confirm_with_message(&text).ok()).unwrap_or(false);
    let kick_member = move |pubkey: String, name: String| {
        if confirm(t_replace_1(lang.get_untracked(), "confirm_kick", "{name}", &name)) {
            with_session(&|s| s.kick(&pubkey));
        }
    };
    let rotate_link = move |_| {
        if confirm(t(lang.get_untracked(), "confirm_rotate").to_string()) {
            with_session(&|s| s.rotate_link());
        }
    };
    let history_on = move || session_ref.with_value(|s| s.as_ref().is_some_and(|s| s.history_enabled()));

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
                    <label class="lobby-check" for="history-checkbox">
                        <input
                            type="checkbox"
                            id="history-checkbox"
                            prop:checked=move || history_input.get()
                            on:change=move |ev| set_history_input.set(event_target_checked(&ev))
                        />
                        <span>{move || t(lang.get(), "history_label")}</span>
                    </label>
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

    let removed_view = move || {
        view! {
            <div class="lobby">
                <div class="lobby-card room-removed">
                    <h2>{move || t(lang.get(), "removed_title")}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "removed_desc")}</p>
                    <button
                        id="back-to-start-btn"
                        class="btn btn-primary lobby-submit"
                        on:click=move |_| {
                            if let Some(win) = window() {
                                let _ = win.location().set_href("/");
                            }
                        }
                    >
                        {move || t(lang.get(), "btn_back_to_start")}
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
                        Notice::HistoryShown => t(lang.get(), "sys_history_shown").to_string(),
                        Notice::Rekeyed => t(lang.get(), "sys_rekeyed").to_string(),
                    }}
                </div>
            }
            .into_view();
        }
        let row_class = match (msg.is_self, msg.mentions_me) {
            (true, _) => "message-row self",
            (false, true) => "message-row peer mention",
            (false, false) => "message-row peer",
        };
        let author = msg.author.clone();
        let is_file = msg.file.is_some();
        let body = match msg.file {
            Some(file) => file_card(lang, file, msg.text.clone(), msg.author.clone(), members, file_action).into_view(),
            None => view! { <div class="message-bubble" dir="auto">{msg.text.clone()}</div> }.into_view(),
        };
        let id = msg.id.clone();
        let me = members.with_untracked(|m| m.iter().find(|x| x.link == LinkUi::Me).map(|x| x.pubkey.clone()));
        let reactions = msg.reactions.clone();
        let (id_pick, id_edit, id_delete, id_picker) = (id.clone(), id.clone(), id.clone(), id.clone());
        let text_for_edit = msg.text.clone();
        let is_self = msg.is_self;
        view! {
            <div class=row_class data-message-id=id.clone()>
                {body}
                <div class="message-actions">
                    <button class="msg-action react-btn" title=move || t(lang.get(), "title_react")
                        on:click=move |_| set_react_picker.update(|p| *p = if p.as_deref() == Some(id_pick.as_str()) { None } else { Some(id_pick.clone()) })>
                        "😀"
                    </button>
                    {(is_self && !is_file).then(|| view! {
                        <button class="msg-action edit-btn" title=move || t(lang.get(), "title_edit")
                            on:click=move |_| start_edit(id_edit.clone(), text_for_edit.clone())>
                            "✏️"
                        </button>
                    })}
                    {(is_self && !is_file).then(|| view! {
                        <button class="msg-action delete-btn" title=move || t(lang.get(), "title_delete")
                            on:click=move |_| delete_message(id_delete.clone())>
                            "🗑️"
                        </button>
                    })}
                </div>
                {move || (react_picker.get().as_deref() == Some(id_picker.as_str())).then(|| {
                    let id = id_picker.clone();
                    view! {
                        <div class="reaction-picker">
                            {REACTIONS.iter().map(|emoji| {
                                let id = id.clone();
                                view! { <button class="reaction-option" on:click=move |_| react(id.clone(), emoji)>{*emoji}</button> }
                            }).collect_view()}
                        </div>
                    }
                })}
                {(!reactions.is_empty()).then(|| view! {
                    <div class="reactions">
                        {reactions.into_iter().map(|(emoji, who)| {
                            let mine = me.as_ref().is_some_and(|me| who.contains(me));
                            let id = id.clone();
                            let emoji_static = REACTIONS.iter().copied().find(|e| *e == emoji).unwrap_or("👍");
                            view! {
                                <button class=if mine { "reaction-chip mine" } else { "reaction-chip" }
                                    data-emoji=emoji.clone()
                                    on:click=move |_| react(id.clone(), emoji_static)>
                                    {format!("{} {}", emoji, who.len())}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                })}
                <div class="message-meta">
                    <span class="message-author" dir="auto">{move || display_name(&author)}</span>
                    <span class="message-tag">{format!(" · {}", pubkey_tag(&msg.author))}</span>
                    <span>" • "</span>
                    <span>{msg.time}</span>
                    {msg.edited.then(|| view! { <span class="edited-mark">{move || format!(" {}", t(lang.get(), "edited_suffix"))}</span> })}
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
                    {move || history_on().then(|| view! {
                        <span class="history-badge" title=move || t(lang.get(), "history_badge_title")>
                            {move || t(lang.get(), "history_badge")}
                        </span>
                    })}
                    <button class="btn btn-secondary copy-invite-btn" on:click=copy_invite_link>
                        {move || if copied.get() { t(lang.get(), "btn_copied") } else { t(lang.get(), "btn_copy_link") }}
                    </button>
                    {is_admin_link.then(|| view! {
                        <button class="btn btn-secondary copy-admin-btn" on:click=copy_admin_link>
                            {move || t(lang.get(), "btn_copy_admin")}
                        </button>
                    })}
                    {move || am_admin.get().then(|| view! {
                        <button
                            id="rotate-link-btn"
                            class="btn btn-secondary"
                            on:click=rotate_link
                            title=move || t(lang.get(), "title_rotate_link")
                        >
                            {move || t(lang.get(), "btn_rotate_link")}
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
                        if !has_messages.get() {
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
                            }
                            .into_view()
                        } else {
                            view! {
                                <For
                                    each=move || messages.get()
                                    key=|msg| (msg.id.clone(), msg.rev)
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
                            children=move |m| member_row(lang, m, am_admin, kick_member, dm_unread, open_dm)
                        />
                    </ul>
                </aside>
            </div>

            <div class="typing-indicator">
                {move || {
                    let names = typing.get();
                    match names.as_slice() {
                        [] => String::new(),
                        [one] => t_replace_1(lang.get(), "typing_one", "{name}", one),
                        [a, b] => t_replace_1(lang.get(), "typing_two", "{a}", a).replace("{b}", b),
                        _ => t(lang.get(), "typing_many").to_string(),
                    }
                }}
            </div>
            {move || editing.get().is_some().then(|| view! {
                <div class="editing-hint">{move || t(lang.get(), "editing_hint")}</div>
            })}

            {move || staged_file.get().map(|file| view! {
                <div class="attachment-chip">
                    <span class="attachment-icon">"📎"</span>
                    <span class="attachment-name">{file.name()}</span>
                    <span class="attachment-size">{format!("({})", format_file_size(file.size() as u64))}</span>
                    <button
                        class="btn-remove-attachment"
                        on:click=move |_| set_staged_file.set(None)
                        title=move || t(lang.get(), "file_remove_title")
                    >
                        "✕"
                    </button>
                </div>
            })}

            <input
                type="file"
                id="file-input-hidden"
                style="display: none;"
                on:change=move |ev| {
                    let target: HtmlInputElement = event_target(&ev);
                    if let Some(file) = target.files().and_then(|files| files.get(0)) {
                        set_staged_file.set(Some(file));
                    }
                    target.set_value("");
                }
            />

            <footer class="input-bar">
                <button
                    class="btn btn-secondary attach-btn"
                    disabled=move || !is_connected()
                    on:click=move |_| {
                        if let Some(input) = window()
                            .and_then(|w| w.document())
                            .and_then(|d| d.get_element_by_id("file-input-hidden"))
                            .and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
                        {
                            input.click();
                        }
                    }
                    title=move || t(lang.get(), "file_attach_title")
                >
                    "📎"
                </button>
                <input
                    type="text"
                    placeholder=move || {
                        if !is_connected() {
                            t(lang.get(), "placeholder_waiting")
                        } else if staged_file.with(|f| f.is_some()) {
                            t(lang.get(), "placeholder_caption")
                        } else {
                            t(lang.get(), "placeholder_connected")
                        }
                    }
                    prop:value=move || input_text.get()
                    on:input=move |ev| {
                        let target: HtmlInputElement = event_target(&ev);
                        set_input_text.set(target.value());
                        with_session(&|s| s.notify_typing());
                    }
                    on:keydown=move |ev| {
                        match ev.key().as_str() {
                            "Enter" => send_message(),
                            "Escape" if editing.get_untracked().is_some() => {
                                set_editing.set(None);
                                set_input_text.set(String::new());
                            }
                            _ => {}
                        }
                    }
                />
                <button
                    class="btn btn-primary send-btn"
                    disabled=move || !is_connected() || (input_text.get().trim().is_empty() && staged_file.with(|f| f.is_none()))
                    on:click=move |_| send_message()
                >
                    {move || if editing.get().is_some() { t(lang.get(), "btn_save") } else { t(lang.get(), "btn_send") }}
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
                Screen::Removed => removed_view().into_view(),
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

            // Private conversation panel
            {move || dm_open.get().map(|peer| {
                let peer_name = peer.clone();
                let name = Signal::derive(move || names.with(|n| n.get(&peer_name).cloned()).unwrap_or_else(|| pubkey_tag(&peer_name)));
                let thread_peer = peer.clone();
                view! {
                    <aside id="dm-panel" class="dm-panel" data-peer=peer.clone()>
                        <div class="dm-header">
                            <h4 dir="auto">{move || t_replace_1(lang.get(), "dm_title", "{name}", &name.get())}</h4>
                            <button class="btn btn-secondary btn-sm" title=move || t(lang.get(), "title_close") on:click=move |_| set_dm_open.set(None)>"✕"</button>
                        </div>
                        <div class="dm-thread">
                            <p class="dm-empty">{move || t_replace_1(lang.get(), "dm_empty", "{name}", &name.get())}</p>
                            {move || dms.with(|d| d.get(&thread_peer).cloned().unwrap_or_default()).into_iter().map(|line| {
                                if line.notice {
                                    view! { <div class="dm-notice">{move || t_replace_1(lang.get(), "dm_peer_left", "{name}", &line.text)}</div> }.into_view()
                                } else {
                                    view! {
                                        <div class=if line.from_me { "dm-line self" } else { "dm-line peer" }>
                                            <span class="dm-text" dir="auto">{line.text}</span>
                                            <span class="dm-time">{line.time}</span>
                                        </div>
                                    }.into_view()
                                }
                            }).collect_view()}
                        </div>
                        <div class="dm-input-row">
                            <input
                                id="dm-input"
                                type="text"
                                placeholder=move || t_replace_1(lang.get(), "dm_placeholder", "{name}", &name.get())
                                prop:value=move || dm_input.get()
                                on:input=move |ev| set_dm_input.set(event_target_value(&ev))
                                on:keydown=move |ev| if ev.key() == "Enter" { send_dm() }
                            />
                            <button id="dm-send-btn" class="btn btn-primary btn-sm"
                                disabled=move || dm_input.get().trim().is_empty()
                                on:click=move |_| send_dm()>
                                {move || t(lang.get(), "btn_send")}
                            </button>
                        </div>
                    </aside>
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

            // Large file notice (no direct-to-disk streaming in this browser)
            {move || large_file_warning.get().map(|(file_id, name)| view! {
                <div class="modal-backdrop">
                    <div class="modal-content">
                        <h3>{move || t(lang.get(), "large_file_title")}</h3>
                        <p>{move || large_file_warning_desc(lang.get(), &name)}</p>
                        <p class="modal-subtext">{move || t(lang.get(), "large_file_subdesc")}</p>
                        <div class="modal-actions">
                            <button class="btn btn-secondary" on:click=move |_| set_large_file_warning.set(None)>
                                {move || t(lang.get(), "btn_cancel")}
                            </button>
                            <button
                                class="btn btn-primary"
                                on:click=move |_| {
                                    set_large_file_warning.set(None);
                                    with_session(&|s| s.download_file(&file_id));
                                }
                            >
                                {move || t(lang.get(), "btn_proceed_anyway")}
                            </button>
                        </div>
                    </div>
                </div>
            })}

            {move || toast.get().map(|key| {
                set_timeout(move || set_toast.set(None), std::time::Duration::from_secs(3));
                view! { <div class="toast">{move || t(lang.get(), key)}</div> }
            })}
        </div>
    }
}

#[derive(Clone, Copy)]
enum FileAction {
    Download,
    Decline,
    Cancel,
    Withdraw,
}

/// A shared file card. Rendered again whenever its status changes (it is part of the key).
fn file_card(
    lang: ReadSignal<Language>,
    file: FileOfferInfo,
    caption: String,
    author: String,
    members: ReadSignal<Vec<MemberUi>>,
    on_action: impl Fn(FileAction, String) + Copy + 'static,
) -> impl IntoView {
    let id = file.file_id.clone();
    let button = move |action: FileAction, class: &'static str, key: &'static str| {
        let id = id.clone();
        view! {
            <button class=class on:click=move |_| on_action(action, id.clone())>
                {move || t(lang.get(), key)}
            </button>
        }
    };
    let status_line = |key: &'static str, class: &'static str| {
        view! { <span class=format!("file-status-text {class}")>{move || t(lang.get(), key)}</span> }.into_view()
    };
    let actions = match file.status {
        FileTransferStatus::Sharing { active, waiting, done } => view! {
            <div class="file-status-row">
                <span class="file-status-text">{move || t(lang.get(), "file_shared_room")}</span>
                {button(FileAction::Withdraw, "btn btn-sm btn-danger file-withdraw-btn", "file_withdraw")}
            </div>
            <div class="file-share-counts">
                <span class="file-count-active">{move || t_replace_1(lang.get(), "file_sending", "{n}", &active.to_string())}</span>
                <span class="file-count-waiting">{move || t_replace_1(lang.get(), "file_waiting_count", "{n}", &waiting.to_string())}</span>
                <span class="file-count-done">{move || t_replace_1(lang.get(), "file_done_count", "{n}", &done.to_string())}</span>
            </div>
        }
        .into_view(),
        FileTransferStatus::Offered => {
            let reachable = move || members.with(|m| m.iter().any(|x| x.pubkey == author && x.link == LinkUi::Direct));
            let download = button(FileAction::Download, "btn btn-sm btn-primary file-download-btn", "file_download");
            let decline = button(FileAction::Decline, "btn btn-sm btn-secondary file-decline-btn", "file_decline");
            view! {
                <div class="file-status-row">
                    {move || if reachable() {
                        view! { {download.clone()} }.into_view()
                    } else {
                        view! { <span class="file-status-text cancelled file-unreachable">{move || t(lang.get(), "file_unreachable")}</span> }.into_view()
                    }}
                    {decline}
                </div>
            }
            .into_view()
        }
        FileTransferStatus::Queued { position } => view! {
            <div class="file-status-row">
                <span class="file-status-text file-queued">{move || t_replace_1(lang.get(), "file_queued", "{n}", &position.to_string())}</span>
                {button(FileAction::Cancel, "btn btn-sm btn-danger file-cancel-btn", "btn_cancel")}
            </div>
        }
        .into_view(),
        FileTransferStatus::Downloading { progress, speed_kb } => {
            let speed = if speed_kb > 1024 { format!("{:.1} MB/s", speed_kb as f64 / 1024.0) } else { format!("{speed_kb} KB/s") };
            view! {
                <div class="file-progress-container">
                    <div class="file-progress-bar">
                        <div class="file-progress-fill" style=format!("width: {progress}%;")></div>
                    </div>
                    <div class="file-progress-meta">
                        <span class="file-progress-label">
                            {move || format!("{}: {}% ({})", t(lang.get(), "file_downloading"), progress, speed)}
                        </span>
                        {button(FileAction::Cancel, "btn btn-sm btn-danger file-cancel-btn", "btn_cancel")}
                    </div>
                </div>
            }
            .into_view()
        }
        FileTransferStatus::Completed => status_line("file_download_complete", "completed"),
        FileTransferStatus::Declined => status_line("file_declined", "cancelled"),
        FileTransferStatus::Cancelled => status_line("file_cancelled", "cancelled"),
        FileTransferStatus::Withdrawn => status_line("file_withdrawn", "cancelled"),
        FileTransferStatus::SenderLeft => status_line("file_sender_left", "cancelled"),
        FileTransferStatus::Interrupted => status_line("file_interrupted", "cancelled"),
    };
    view! {
        <div class="file-card" data-file-id=file.file_id.clone()>
            <div class="file-card-header">
                <span class="file-icon">"📦"</span>
                <div class="file-info">
                    <span class="file-name" dir="auto">{file.name}</span>
                    <span class="file-size">{format_file_size(file.size)}</span>
                </div>
            </div>
            {(!caption.is_empty()).then(|| view! { <div class="file-caption" dir="auto">{caption}</div> })}
            <div class="file-card-actions">{actions}</div>
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

/// One member in the side panel: name, key tag, admin badge, how we reach them, and a
/// Kick button for admins.
fn member_row(
    lang: ReadSignal<Language>,
    member: MemberUi,
    am_admin: Memo<bool>,
    on_kick: impl Fn(String, String) + Copy + 'static,
    dm_unread: ReadSignal<HashMap<String, usize>>,
    on_dm: impl Fn(String) + Copy + 'static,
) -> impl IntoView {
    let kickable = member.link != LinkUi::Me && !member.is_admin;
    let dm_target = (member.link != LinkUi::Me).then(|| member.pubkey.clone());
    let kick_target = (member.pubkey.clone(), member.name.clone());
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
            {dm_target.map(|pubkey| {
                let unread_key = pubkey.clone();
                view! {
                    <button
                        class="btn btn-sm btn-secondary dm-btn"
                        title=move || t(lang.get(), "title_dm")
                        on:click=move |_| on_dm(pubkey.clone())
                    >
                        "✉️"
                        {move || {
                            let n = dm_unread.with(|u| u.get(&unread_key).copied().unwrap_or(0));
                            (n > 0).then(|| view! { <span class="dm-unread">{n}</span> })
                        }}
                    </button>
                }
            })}
            {move || (kickable && am_admin.get()).then(|| {
                let (pubkey, name) = kick_target.clone();
                view! {
                    <button
                        class="btn btn-sm btn-danger kick-btn"
                        title=move || t(lang.get(), "title_kick")
                        on:click=move |_| on_kick(pubkey.clone(), name.clone())
                    >
                        {move || t(lang.get(), "btn_kick")}
                    </button>
                }
            })}
        </li>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(|| view! { <App/> });
}
