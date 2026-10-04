mod agent;
mod i18n;
mod ice;
mod layout;
mod media;
mod mesh;
mod names;
mod nostr_pool;
mod qr;
mod remote_input;
mod session;
mod state;

use i18n::{
    detect_browser_language, large_file_warning_desc, t, t_replace_1, update_document_direction,
    Language,
};
use leptos::*;
use names::{pubkey_tag, random_name, sanitize_name, MAX_NAME_CHARS};
use protocol::{
    format_relay_list, parse_cap, password_room_key, split_relay_input, stretch_password, RoomParams, VideoKind,
    DEFAULT_MEMBER_CAP, DEFAULT_VIDEO_CAP, DEFAULT_VOICE_CAP, KEY_LENGTH, REACTIONS, ControlWants, MonitorInfo,
    DEFAULT_AGENT_PORT, PointerMode,
};
use agent::{AgentLink, AgentSignals, AgentStatus};
use qr::generate_qr_svg;
use remote_input::{InputCapture, InputSink, PadPoller};
use std::rc::Rc;
use session::{RoomSession, SessionSignals};
use state::{
    admin_url, create_room, current_fragment, format_file_size, fragment_relay_choice, invite_url, read_credentials,
    AudioSettings, RelayMode,
    ChatMessageUi, ConnectionStatus, DmUi, FileOfferInfo, FileTransferStatus, LinkUi,
    LoungeMemberUi, MemberUi, MyVoiceUi, Notice, RekeyTarget, RoomCaps, ControlUi, ControlPromptUi,
};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::JsCast;
use web_sys::{window, HtmlInputElement};

/// Space between video tiles (keep in sync with `.video-grid { gap }`).
const GRID_GAP_PX: f64 = 8.0;

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
    let (hide_ip_input, set_hide_ip_input) = create_signal(false);
    // Typed at creation or on the join screen; cleared once stretched.
    let (password_input, set_password_input) = create_signal(String::new());
    // Spell checking the message boxes (some browsers' enhanced spell check sends text away).
    let (spellcheck_on, set_spellcheck_on) = create_signal(true);
    // Relay choice: what the link already says, else public; a deployment can pre-fill its
    // own relay address at build time (DCHAT_RELAY_URL).
    let initial_relays = fragment_relay_choice();
    let (relay_mode, set_relay_mode) = create_signal(initial_relays.as_ref().map_or(RelayMode::Public, |c| c.0));
    let (relay_input, set_relay_input) = create_signal(
        initial_relays
            .map(|c| c.1)
            .unwrap_or_else(|| option_env!("DCHAT_RELAY_URL").unwrap_or_default().to_string()),
    );
    // The custom relay URLs, or `None` while what was typed isn't usable.
    let custom_relays = create_memo(move |_| {
        let (valid, invalid) = split_relay_input(&relay_input.get());
        let page_is_https = window().and_then(|w| w.location().protocol().ok()).as_deref() == Some("https:");
        // An HTTPS page can only open encrypted (wss://) connections.
        let secure_ok = !page_is_https || valid.iter().all(|u| u.starts_with("wss://"));
        (!valid.is_empty() && invalid.is_empty() && secure_ok).then_some(valid)
    });
    let relay_choice_valid = move || relay_mode.get() == RelayMode::Public || custom_relays.get().is_some();

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
    let (update_required, set_update_required) = create_signal(false);

    // Remote control: the paired dchat-host app (app level: it survives a room rekey) and
    // which shared screen this tab is controlling right now.
    let (control, set_control) = create_signal(ControlUi::default());
    let (agent_status, set_agent_status) = create_signal(AgentStatus::Off);
    let (agent_monitors, set_agent_monitors) = create_signal(None::<(Vec<MonitorInfo>, Option<u32>)>);
    let (agent_warning, set_agent_warning) = create_signal(None::<String>);
    let agent_link = store_value(None::<Rc<AgentLink>>);
    let (show_control_host, set_show_control_host) = create_signal(false);
    let (agent_port_input, set_agent_port_input) = create_signal(DEFAULT_AGENT_PORT.to_string());
    let (agent_code_input, set_agent_code_input) = create_signal(String::new());
    let (allow_control, set_allow_control) = create_signal(true);
    let (controlling, set_controlling) = create_signal(None::<String>);
    let capture = store_value(None::<InputCapture>);
    let pad_poller = store_value(None::<PadPoller>);
    // Toasts and control prompts live in one layer that moves into whatever element is in
    // fullscreen, so they stay visible there.
    let overlay_ref = create_node_ref::<leptos::html::Div>();
    let overlay_home = store_value(None::<web_sys::Node>);
    let (no_turn, set_no_turn) = create_signal(false);

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
    // TURN servers the host offered (fetched once, reused if the room moves).
    let host_ice = store_value(None::<js_sys::Array>);
    // A password room's stretched password, RAM only: it also opens the room after a rekey.
    let stretched_password = store_value(None::<[u8; KEY_LENGTH]>);

    let start_session = move |room_id: String, link_key: [u8; KEY_LENGTH], migrated: bool| -> Option<RoomSession> {
        // In a password room the link's key alone opens nothing.
        let key = match (RoomParams::from_fragment(&current_fragment()).password_salt, stretched_password.get_value()) {
            (Some(_), Some(stretched)) => password_room_key(&link_key, &stretched),
            _ => link_key,
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
            removed: set_removed,
            rekey: set_rekey,
            typing: set_typing,
            mention: set_mention_count,
            dms: set_dms,
            dm_unread: set_dm_unread,
            update_required: set_update_required,
            no_turn: set_no_turn,
            control: set_control,
        };
        set_room_id_sig.set(room_id.clone());
        match RoomSession::start(room_id, key, my_name.get_value(), signals, migrated, host_ice.get_value()) {
            Ok(session) => {
                session_ref.set_value(Some(session.clone()));
                session.set_allow_control(allow_control.get_untracked());
                session.attach_agent(agent_link.get_value());
                set_screen.set(Screen::Room);
                Some(session)
            }
            Err(err) => {
                set_status.set(ConnectionStatus::Error(err));
                None
            }
        }
    };

    let (entering, set_entering) = create_signal(false);
    let enter_room = move || {
        let Some((room_id, key)) = read_credentials() else {
            return;
        };
        if entering.get_untracked() {
            return;
        }
        let salt = RoomParams::from_fragment(&current_fragment()).password_salt;
        let password = password_input.get_untracked();
        if salt.is_some() && password.trim().is_empty() {
            return;
        }
        set_entering.set(true);
        my_name.set_value(sanitize_name(&name_input.get_untracked()));
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(salt) = salt {
                // Let "Unlocking…" render before the deliberately slow hash blocks the page.
                media::sleep_ms(50).await;
                let Ok(stretched) = stretch_password(&password, &salt) else {
                    set_entering.set(false);
                    return;
                };
                stretched_password.set_value(Some(stretched));
                set_password_input.set(String::new());
            }
            // Optional host TURN servers (Cloudflare Worker); quick 404 on static hosts.
            host_ice.set_value(ice::fetch_ice_servers().await);
            start_session(room_id, key, false);
            set_entering.set(false);
        });
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
            hide_ip: hide_ip_input.get_untracked(),
            password: !password_input.get_untracked().trim().is_empty(),
            relays: match (relay_mode.get_untracked(), custom_relays.get_untracked()) {
                (RelayMode::Public, _) => None,
                (mode, Some(urls)) => Some(format_relay_list(&urls, mode == RelayMode::CustomWithPublic)),
                (_, None) => return,
            },
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
            let _ = win.location().set_href(&state::page_base_url());
        }
    };

    let with_session = move |f: &dyn Fn(&RoomSession)| {
        session_ref.with_value(|session| {
            if let Some(session) = session {
                f(session);
            }
        });
    };

    // ---- Remote control ------------------------------------------------------------------
    let connect_agent = move || {
        let code = agent_code_input.get_untracked();
        if code.trim().is_empty() {
            return;
        }
        let port = agent_port_input.get_untracked().trim().parse::<u16>().unwrap_or(DEFAULT_AGENT_PORT);
        if let Some(old) = agent_link.get_value() {
            old.disconnect();
        }
        let signals = AgentSignals { status: set_agent_status, monitors: set_agent_monitors, warning: set_agent_warning };
        let link = AgentLink::connect(port, &code, signals);
        agent_link.set_value(Some(link.clone()));
        set_agent_code_input.set(String::new());
        with_session(&|s| s.attach_agent(Some(link.clone())));
    };
    let disconnect_agent = move || {
        if let Some(link) = agent_link.get_value() {
            link.disconnect();
        }
        agent_link.set_value(None);
        set_agent_monitors.set(None);
        with_session(&|s| s.attach_agent(None));
    };
    let open_control_host = move |_| {
        if let Some(doc) = window().and_then(|w| w.document()) {
            if doc.fullscreen_element().is_some() {
                doc.exit_fullscreen();
            }
        }
        set_show_control_host.set(true);
    };
    let disengage = move || {
        capture.set_value(None);
        set_controlling.set(None);
    };
    let engage = move |sharer: String| {
        let Some(doc) = window().and_then(|w| w.document()) else {
            return;
        };
        let surface = doc.get_element_by_id(&format!("control-surface-{sharer}")).and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok());
        let video = doc.get_element_by_id(&format!("tile-video-{sharer}")).and_then(|e| e.dyn_into::<web_sys::HtmlVideoElement>().ok());
        let (Some(surface), Some(video)) = (surface, video) else {
            return;
        };
        capture.set_value(None);
        let mode = control.with_untracked(|c| c.mine.get(&sharer).map(|m| m.mode).unwrap_or_default());
        let sink: InputSink = {
            let sharer = sharer.clone();
            Rc::new(move |events| with_session(&|s| s.send_input(&sharer, events.clone())))
        };
        let on_release: Rc<dyn Fn()> = Rc::new(move || disengage());
        if let Ok(engaged) = InputCapture::engage(&surface, &video, mode, sink, on_release) {
            capture.set_value(Some(engaged));
            set_controlling.set(Some(sharer));
        }
    };
    let on_tile_control = move |sharer: String, action: TileControlAction| match action {
        TileControlAction::Request => {
            with_session(&|s| s.request_control(&sharer, ControlWants { mouse_keyboard: true, controller: false }));
        }
        TileControlAction::RequestPad => {
            with_session(&|s| s.request_control(&sharer, ControlWants { mouse_keyboard: false, controller: true }));
        }
        TileControlAction::Release => {
            if controlling.get_untracked().as_deref() == Some(sharer.as_str()) {
                disengage();
            }
            with_session(&|s| s.release_control(&sharer));
        }
        TileControlAction::Engage => engage(sharer),
        TileControlAction::ToggleMode => {
            let mode = control.with_untracked(|c| c.mine.get(&sharer).map(|m| m.mode).unwrap_or_default());
            let next = if mode == PointerMode::Game { PointerMode::Desktop } else { PointerMode::Game };
            with_session(&|s| s.set_control_mode(&sharer, next));
        }
    };
    // A controller slot anywhere: read this viewer's game controller while it lasts.
    let holds_pad = create_memo(move |_| control.with(|c| c.mine.values().any(|m| m.pad.is_some())));
    create_effect(move |_| {
        if !holds_pad.get() {
            pad_poller.set_value(None);
            return;
        }
        if pad_poller.with_value(Option::is_none) {
            let sink: InputSink = Rc::new(move |events| with_session(&|s| s.send_pad_input(events.clone())));
            if let Ok(poller) = PadPoller::start(sink) {
                pad_poller.set_value(Some(poller));
            }
        }
    });
    // Rights gone (revoked, taken over, share ended): stop capturing right away.
    create_effect(move |_| {
        let Some(sharer) = controlling.get() else {
            return;
        };
        if !control.with(|c| c.mine.get(&sharer).is_some_and(|m| m.mouse_keyboard)) {
            disengage();
        }
    });
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
    // Fullscreen for the whole video grid or a single tile. Browsers without element
    // fullscreen (iPhone Safari) get the grid expanded over the page instead.
    let (grid_expanded, set_grid_expanded) = create_signal(false);
    let (fullscreen_on, set_fullscreen_on) = create_signal(false);
    let toggle_fullscreen = move |tile_pubkey: Option<String>| {
        let Some(doc) = window().and_then(|w| w.document()) else {
            return;
        };
        if doc.fullscreen_element().is_some() {
            doc.exit_fullscreen();
            return;
        }
        if grid_expanded.get_untracked() {
            set_grid_expanded.set(false);
            return;
        }
        let target_id = tile_pubkey.map_or_else(|| "video-grid".to_string(), |pk| format!("tile-{pk}"));
        let Some(target) = doc.get_element_by_id(&target_id) else {
            return;
        };
        if doc.fullscreen_enabled() {
            let _ = target.request_fullscreen();
        } else {
            set_grid_expanded.set(true);
        }
    };
    {
        let on_change = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            let active = window().and_then(|w| w.document()).is_some_and(|d| d.fullscreen_element().is_some());
            set_fullscreen_on.set(active);
            if let (Some(overlay), Some(doc)) = (overlay_ref.get_untracked(), window().and_then(|w| w.document())) {
                let overlay: &web_sys::HtmlDivElement = &overlay;
                let node: web_sys::Node = overlay.clone().into();
                match doc.fullscreen_element() {
                    Some(fullscreen) => {
                        if overlay_home.get_value().is_none() {
                            overlay_home.set_value(node.parent_node());
                        }
                        let _ = fullscreen.append_child(&node);
                    }
                    None => {
                        if let Some(home) = overlay_home.get_value() {
                            let _ = home.append_child(&node);
                        }
                    }
                }
            }
        }) as Box<dyn FnMut()>);
        let on_key = wasm_bindgen::closure::Closure::wrap(Box::new(move |ev: web_sys::KeyboardEvent| {
            if ev.key() == "Escape" && grid_expanded.get_untracked() {
                set_grid_expanded.set(false);
            }
        }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
        if let Some(doc) = window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("fullscreenchange", on_change.as_ref().unchecked_ref());
            let _ = doc.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
        }
        on_change.forget();
        on_key.forget();
    }
    // The grid's content box, measured live so tiles always fit (resizes, fullscreen).
    let (grid_box, set_grid_box) = create_signal((0.0_f64, 0.0_f64));
    let grid_observer = store_value(None::<web_sys::ResizeObserver>);
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
    // Video elements are recreated when the grid appears: point them at their streams,
    // and start measuring the new grid element.
    create_effect(move |_| {
        if !show_video_grid.get() {
            set_grid_expanded.set(false);
            return;
        }
        with_session(&|s| s.attach_lounge_media());
        wasm_bindgen_futures::spawn_local(async move {
            for _ in 0..20 {
                if let Some(grid) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("video-grid")) {
                    let on_resize = wasm_bindgen::closure::Closure::wrap(Box::new(move |entries: js_sys::Array| {
                        if let Ok(entry) = entries.get(0).dyn_into::<web_sys::ResizeObserverEntry>() {
                            let rect = entry.content_rect();
                            set_grid_box.set((rect.width(), rect.height()));
                        }
                    }) as Box<dyn FnMut(js_sys::Array)>);
                    if let Ok(observer) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
                        observer.observe(&grid);
                        if let Some(old) = grid_observer.get_value() {
                            old.disconnect();
                        }
                        grid_observer.set_value(Some(observer));
                    }
                    on_resize.forget();
                    return;
                }
                media::sleep_ms(30).await;
            }
        });
    });
    let grid_style = move || {
        let count = lounge.with(|l| l.iter().filter(|m| m.video != VideoKind::None).count());
        let (width, height) = grid_box.get();
        let fit = layout::fit_grid(count, width, height, GRID_GAP_PX);
        format!(
            "grid-template-columns: repeat({}, {}px); grid-auto-rows: {}px;",
            fit.cols, fit.tile_w, fit.tile_h
        )
    };
    let grid_fullscreen_active = move || fullscreen_on.get() || grid_expanded.get();
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
    let hides_ip = move || session_ref.with_value(|s| s.as_ref().is_some_and(|s| s.hides_ip()));
    let has_password = move || session_ref.with_value(|s| s.as_ref().is_some_and(|s| s.has_password()));

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
                autocomplete="off"
                maxlength=MAX_NAME_CHARS
                prop:value=move || name_input.get()
                on:input=move |ev| set_name_input.set(event_target_value(&ev))
            />
        }
    };

    // Shown while a password is being stretched (it takes a moment on purpose).
    let enter_label = move |idle_key: &'static str| {
        if entering.get() && !password_input.get().trim().is_empty() {
            t(lang.get(), "unlocking")
        } else {
            t(lang.get(), idle_key)
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
                <form class="lobby-card" autocomplete="off" on:submit=move |ev| { ev.prevent_default(); create_and_enter(); }>
                    <h2>{move || t(lang.get(), "create_title")}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "create_desc")}</p>
                    {name_field}
                    <label class="lobby-label" for="cap-input">{move || t(lang.get(), "member_cap_label")}</label>
                    <input
                        id="cap-input"
                        class="lobby-input"
                        type="number"
                        autocomplete="off"
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
                                autocomplete="off"
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
                                autocomplete="off"
                                min="0"
                                prop:value=move || video_cap_input.get()
                                on:input=move |ev| set_video_cap_input.set(event_target_value(&ev))
                            />
                        </div>
                    </div>
                    <label class="lobby-label" for="relay-mode">{move || t(lang.get(), "relay_mode_label")}</label>
                    <select
                        id="relay-mode"
                        class="lobby-input"
                        on:change=move |ev| {
                            set_relay_mode.set(match event_target_value(&ev).as_str() {
                                "custom" => RelayMode::Custom,
                                "both" => RelayMode::CustomWithPublic,
                                _ => RelayMode::Public,
                            });
                        }
                    >
                        <option value="public" selected=move || relay_mode.get() == RelayMode::Public>
                            {move || t(lang.get(), "relay_mode_public")}
                        </option>
                        <option value="custom" selected=move || relay_mode.get() == RelayMode::Custom>
                            {move || t(lang.get(), "relay_mode_custom")}
                        </option>
                        <option value="both" selected=move || relay_mode.get() == RelayMode::CustomWithPublic>
                            {move || t(lang.get(), "relay_mode_both")}
                        </option>
                    </select>
                    {move || (relay_mode.get() != RelayMode::Public).then(|| view! {
                        <label class="lobby-label" for="relay-url">{move || t(lang.get(), "relay_url_label")}</label>
                        <input
                            id="relay-url"
                            class="lobby-input"
                            type="text"
                            autocomplete="off"
                            inputmode="url"
                            autocapitalize="off"
                            spellcheck="false"
                            placeholder="wss://relay.example.com"
                            prop:value=move || relay_input.get()
                            on:input=move |ev| set_relay_input.set(event_target_value(&ev))
                        />
                        {move || (!relay_input.get().trim().is_empty() && custom_relays.get().is_none()).then(|| view! {
                            <p class="lobby-warning relay-invalid">{move || t(lang.get(), "relay_url_invalid")}</p>
                        })}
                    })}
                    <p class="lobby-hint">{move || t(lang.get(), "relay_hint")}</p>
                    <label class="lobby-check" for="history-checkbox">
                        <input
                            type="checkbox"
                            id="history-checkbox"
                            prop:checked=move || history_input.get()
                            on:change=move |ev| set_history_input.set(event_target_checked(&ev))
                        />
                        <span>{move || t(lang.get(), "history_label")}</span>
                    </label>
                    <label class="lobby-check" for="hide-ip-checkbox">
                        <input
                            type="checkbox"
                            id="hide-ip-checkbox"
                            prop:checked=move || hide_ip_input.get()
                            on:change=move |ev| set_hide_ip_input.set(event_target_checked(&ev))
                        />
                        <span>{move || t(lang.get(), "hide_ip_label")}</span>
                    </label>
                    {move || hide_ip_input.get().then(|| view! {
                        <p class="lobby-hint">{move || t(lang.get(), "hide_ip_hint")}</p>
                    })}
                    <label class="lobby-label" for="password-input">{move || t(lang.get(), "password_label")}</label>
                    <input
                        id="password-input"
                        class="lobby-input"
                        type="password"
                        autocomplete="off"
                        prop:value=move || password_input.get()
                        on:input=move |ev| set_password_input.set(event_target_value(&ev))
                    />
                    {move || (!password_input.get().trim().is_empty()).then(|| view! {
                        <p class="lobby-hint">{move || t(lang.get(), "password_hint")}</p>
                    })}
                    {move || cap_is_large().then(|| view! {
                        <p class="lobby-warning">{move || t(lang.get(), "cap_warning")}</p>
                    })}
                    <button id="create-room-btn" type="submit" class="btn btn-primary lobby-submit" disabled=move || entering.get() || !relay_choice_valid()>
                        {move || enter_label("btn_create_room")}
                    </button>
                </form>
            </div>
        }
    };

    let join_view = move || {
        let room = read_credentials().map(|(room, _)| room).unwrap_or_default();
        let password_room = RoomParams::from_fragment(&current_fragment()).password_salt.is_some();
        view! {
            <div class="lobby">
                <form class="lobby-card" autocomplete="off" on:submit=move |ev| { ev.prevent_default(); enter_room(); }>
                    <h2>{move || t_replace_1(lang.get(), "join_title", "{room}", &room)}</h2>
                    <p class="lobby-desc">{move || t(lang.get(), "join_desc")}</p>
                    {name_field}
                    {password_room.then(|| view! {
                        <label class="lobby-label" for="password-input">{move || t(lang.get(), "join_password_label")}</label>
                        <input
                            id="password-input"
                            class="lobby-input"
                            type="password"
                            autocomplete="off"
                            required
                            prop:value=move || password_input.get()
                            on:input=move |ev| set_password_input.set(event_target_value(&ev))
                        />
                        <p class="lobby-hint">{move || t(lang.get(), "join_password_hint")}</p>
                    })}
                    <button id="enter-room-btn" type="submit" class="btn btn-primary lobby-submit" disabled=move || entering.get()>
                        {move || enter_label("btn_enter_room")}
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
                                let _ = win.location().set_href(&state::page_base_url());
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
                    {move || has_password().then(|| view! {
                        <span class="history-badge password-badge" title=move || t(lang.get(), "password_badge_title")>
                            {move || t(lang.get(), "password_badge")}
                        </span>
                    })}
                    {move || hides_ip().then(|| view! {
                        <span class="history-badge hide-ip-badge" title=move || t(lang.get(), "hide_ip_badge_title")>
                            {move || t(lang.get(), "hide_ip_badge")}
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
                            {move || show_video_grid.get().then(|| view! {
                                <button
                                    id="fullscreen-btn"
                                    class="btn btn-secondary"
                                    on:click=move |_| toggle_fullscreen(None)
                                    title=move || t(lang.get(), "title_fullscreen")
                                >
                                    "⛶"
                                </button>
                            })}
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
                        title=move || t(lang.get(), "btn_settings_title")
                    >
                        "⚙️"
                    </button>
                    {move || my_voice.get().in_voice.then(|| view! {
                        <button
                            id="control-host-btn"
                            class="btn btn-secondary"
                            class:active=move || control.with(|c| c.hosting)
                            title=move || t(lang.get(), "btn_control_host_title")
                            on:click=open_control_host
                        >
                            "🖱️"
                        </button>
                    })}
                    {move || control.with(|c| c.hosting && (c.host_mouse_keyboard.is_some() || c.host_pads.iter().any(Option::is_some))).then(|| view! {
                        <button id="stop-control-btn" class="btn btn-danger btn-sm" on:click=move |_| with_session(&|s| s.stop_all_control())>
                            {move || t(lang.get(), "btn_stop_control")}
                        </button>
                    })}
                </div>
            </div>

            {move || no_turn.get().then(|| view! {
                <div id="no-turn-banner" class="update-banner" role="alert">
                    <span>{move || t(lang.get(), "no_turn_banner")}</span>
                </div>
            })}

            {move || update_required.get().then(|| view! {
                <div id="update-banner" class="update-banner" role="alert">
                    <span>{move || t(lang.get(), "update_required")}</span>
                    <button
                        id="reload-btn"
                        class="btn btn-call btn-sm"
                        on:click=move |_| {
                            if let Some(win) = web_sys::window() {
                                let _ = win.location().reload();
                            }
                        }
                    >
                        {move || t(lang.get(), "btn_reload")}
                    </button>
                </div>
            })}

            {move || controlling.get().map(|sharer| {
                let name = names.with(|n| n.get(&sharer).cloned()).unwrap_or_else(|| pubkey_tag(&sharer));
                view! {
                    <div id="controlling-bar" class="voice-prompt controlling-bar">
                        <span>{move || format!("🖱️ {} · {}", t_replace_1(lang.get(), "control_controlling", "{name}", &name), t(lang.get(), "control_engaged_hint"))}</span>
                        <button
                            id="controlling-release-btn"
                            class="btn btn-secondary btn-sm"
                            on:click=move |_| on_tile_control(sharer.clone(), TileControlAction::Release)
                        >
                            {move || t(lang.get(), "btn_release_control")}
                        </button>
                    </div>
                }
            })}

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
                <div
                    id="video-grid"
                    class=move || if grid_expanded.get() { "video-grid expanded" } else { "video-grid" }
                    style=grid_style
                >
                    <For
                        each=video_members
                        key=|m| (m.pubkey.clone(), m.video)
                        children=move |m| video_tile(lang, m, lounge, speaking, toggle_fullscreen, control, controlling, names, on_tile_control)
                    />
                    <button
                        class="btn btn-secondary grid-fullscreen"
                        on:click=move |_| toggle_fullscreen(None)
                        title=move || if grid_fullscreen_active() { t(lang.get(), "title_exit_fullscreen") } else { t(lang.get(), "title_fullscreen") }
                    >
                        {move || if grid_fullscreen_active() { "✕" } else { "⛶" }}
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
                            children=move |m| member_row(lang, m, am_admin, kick_member, dm_unread, open_dm, control, move |pk: String| with_session(&|s| s.revoke_control(&pk)))
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
                    autocomplete="off"
                    spellcheck=move || if spellcheck_on.get() { "true" } else { "false" }
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
                                autocomplete="off"
                                spellcheck=move || if spellcheck_on.get() { "true" } else { "false" }
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
                                <h3>{move || t(lang.get(), "settings_title")}</h3>
                                <button class="btn btn-secondary" on:click=move |_| set_show_audio_settings.set(false)>"✕"</button>
                            </div>
                            <h4 class="settings-section">{move || t(lang.get(), "settings_audio")}</h4>
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
                            <h4 class="settings-section">{move || t(lang.get(), "settings_typing")}</h4>
                            <div class="audio-options">
                                {audio_option_row(
                                    lang,
                                    "spellcheck-toggle",
                                    "spellcheck_label",
                                    true,
                                    spellcheck_on.into(),
                                    move |on| set_spellcheck_on.set(on),
                                )}
                                <p class="settings-hint">{move || t(lang.get(), "spellcheck_hint")}</p>
                            </div>
                            <button class="btn btn-primary" style="width: 100%;" on:click=move |_| set_show_audio_settings.set(false)>
                                {move || t(lang.get(), "qr_close")}
                            </button>
                        </div>
                    </div>
                }
            })}

            // Remote control of this computer (the sharer's side)
            {move || show_control_host.get().then(|| view! {
                <div class="modal-backdrop" on:click=move |_| set_show_control_host.set(false)>
                    <div id="control-host-modal" class="modal-content control-host-modal" on:click=|ev| ev.stop_propagation()>
                        <div class="modal-title-row">
                            <h3>{move || t(lang.get(), "control_host_title")}</h3>
                            <button class="btn btn-secondary" on:click=move |_| set_show_control_host.set(false)>"✕"</button>
                        </div>
                        <p class="modal-subtext">{move || t(lang.get(), "control_host_desc")}</p>
                        <p id="agent-status" class="agent-status" data-status=move || agent_status.with(|s| s.key())>
                            {move || t(lang.get(), agent_status_key(&agent_status.get()))}
                        </p>
                        {move || (!agent_status.with(AgentStatus::is_paired)).then(|| view! {
                            <div class="agent-connect">
                                <label class="lobby-label" for="agent-code-input">{move || t(lang.get(), "agent_code_label")}</label>
                                <input
                                    id="agent-code-input"
                                    class="lobby-input"
                                    type="text"
                                    autocomplete="off"
                                    spellcheck="false"
                                    autocapitalize="characters"
                                    placeholder="K7QM-4XPA"
                                    prop:value=move || agent_code_input.get()
                                    on:input=move |ev| set_agent_code_input.set(event_target_value(&ev))
                                    on:keydown=move |ev| if ev.key() == "Enter" { connect_agent() }
                                />
                                <label class="lobby-label" for="agent-port-input">{move || t(lang.get(), "agent_port_label")}</label>
                                <input
                                    id="agent-port-input"
                                    class="lobby-input"
                                    type="number"
                                    min="1"
                                    max="65535"
                                    autocomplete="off"
                                    prop:value=move || agent_port_input.get()
                                    on:input=move |ev| set_agent_port_input.set(event_target_value(&ev))
                                />
                                <button
                                    id="agent-connect-btn"
                                    class="btn btn-primary"
                                    disabled=move || agent_code_input.get().trim().is_empty()
                                    on:click=move |_| connect_agent()
                                >
                                    {move || t(lang.get(), "btn_agent_connect")}
                                </button>
                            </div>
                        })}
                        {move || agent_status.with(AgentStatus::is_paired).then(|| view! {
                            <div class="agent-paired">
                                {move || agent_status.with(|s| match s {
                                    AgentStatus::Paired { caps, .. } => caps.mouse_keyboard_error.clone(),
                                    _ => None,
                                }).map(|err| view! {
                                    <p class="lobby-warning">{move || t_replace_1(lang.get(), "agent_kbm_unavailable", "{error}", &err)}</p>
                                })}
                                {move || agent_status.with(|s| match s {
                                    AgentStatus::Paired { caps, .. } => caps.pads_error.clone(),
                                    _ => None,
                                }).map(|err| view! {
                                    <p class="lobby-hint agent-pads-unavailable">{move || t_replace_1(lang.get(), "agent_pads_unavailable", "{error}", &err)}</p>
                                })}
                                <label class="lobby-check" for="allow-control-toggle">
                                    <input
                                        type="checkbox"
                                        id="allow-control-toggle"
                                        prop:checked=move || allow_control.get()
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            set_allow_control.set(on);
                                            with_session(&|s| s.set_allow_control(on));
                                        }
                                    />
                                    <span>{move || t(lang.get(), "allow_control_label")}</span>
                                </label>
                                <p id="control-host-state" class="lobby-hint" data-hosting=move || control.with(|c| c.hosting).to_string()>
                                    {move || if control.with(|c| c.hosting) { t(lang.get(), "control_hosting") } else { t(lang.get(), "control_needs_screen") }}
                                </p>
                                {move || agent_monitors.get().filter(|(list, _)| list.len() > 1).map(|(list, chosen)| view! {
                                    <label class="lobby-label" for="agent-monitor">{move || t(lang.get(), "control_monitor_label")}</label>
                                    <select
                                        id="agent-monitor"
                                        class="lobby-input"
                                        on:change=move |ev| {
                                            if let Ok(id) = event_target_value(&ev).parse::<u32>() {
                                                with_session(&|s| s.use_monitor(id));
                                            }
                                        }
                                    >
                                        {chosen.is_none().then(|| view! { <option value="" selected=true>"—"</option> })}
                                        {list.into_iter().map(|m| view! {
                                            <option value=m.id.to_string() selected=chosen == Some(m.id)>
                                                {format!("{} ({}×{})", m.name, m.width, m.height)}
                                            </option>
                                        }).collect_view()}
                                    </select>
                                })}
                                <button id="agent-disconnect-btn" class="btn btn-secondary" on:click=move |_| disconnect_agent()>
                                    {move || t(lang.get(), "btn_agent_disconnect")}
                                </button>
                            </div>
                        })}
                        {move || agent_warning.get().map(|warning| view! { <p class="lobby-warning agent-warning">{warning}</p> })}
                    </div>
                </div>
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

            <div id="overlay-layer" class="overlay-layer" node_ref=overlay_ref>
                <div id="control-prompts" class="control-prompts">
                    <For
                        each=move || control.get().prompts
                        key=|p| p.clone()
                        children=move |p| control_prompt(lang, p, names, move |member: String, allow: bool| {
                            with_session(&|s| if allow { s.grant_control(&member) } else { s.deny_control(&member) });
                        })
                    />
                </div>
                {move || toast.get().map(|key| {
                    set_timeout(move || set_toast.set(None), std::time::Duration::from_secs(3));
                    view! { <div class="toast">{move || t(lang.get(), key)}</div> }
                })}
            </div>
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileControlAction {
    Request,
    RequestPad,
    Release,
    Engage,
    ToggleMode,
}

/// What a viewer can do with a shared screen's tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileControl {
    Hidden,
    Offer,
    Requested,
    Granted,
    /// A controller slot, no mouse and keyboard.
    PadOnly,
    Engaged,
}

impl TileControl {
    fn key(self) -> &'static str {
        match self {
            TileControl::Hidden => "none",
            TileControl::Offer => "offer",
            TileControl::Requested => "requested",
            TileControl::Granted => "granted",
            TileControl::PadOnly => "pad",
            TileControl::Engaged => "engaged",
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn video_tile(
    lang: ReadSignal<Language>,
    member: LoungeMemberUi,
    lounge: ReadSignal<Vec<LoungeMemberUi>>,
    speaking: ReadSignal<HashSet<String>>,
    on_fullscreen: impl Fn(Option<String>) + Copy + 'static,
    control: ReadSignal<ControlUi>,
    controlling: ReadSignal<Option<String>>,
    names: ReadSignal<HashMap<String, String>>,
    on_control: impl Fn(String, TileControlAction) + Copy + 'static,
) -> impl IntoView {
    let pk_speaking = member.pubkey.clone();
    let pk_mic = member.pubkey.clone();
    let pk_dbl = member.pubkey.clone();
    let pk_btn = member.pubkey.clone();
    let is_self = member.is_self;
    let pk = member.pubkey.clone();
    let controllable = !is_self && member.video == VideoKind::Screen;
    // Memos, so the capture surface is only rebuilt when control really starts or stops.
    let tile_control = {
        let pk = pk.clone();
        create_memo(move |_| {
            if !controllable {
                return TileControl::Hidden;
            }
            let (offered, mine) = control.with(|c| (c.offers.contains_key(&pk), c.mine.get(&pk).copied()));
            match mine {
                Some(m) if m.mouse_keyboard && controlling.with(|c| c.as_deref() == Some(pk.as_str())) => TileControl::Engaged,
                Some(m) if m.mouse_keyboard => TileControl::Granted,
                Some(m) if m.pad.is_some() => TileControl::PadOnly,
                Some(m) if m.requested => TileControl::Requested,
                _ if offered => TileControl::Offer,
                _ => TileControl::Hidden,
            }
        })
    };
    let has_surface = create_memo(move |_| matches!(tile_control.get(), TileControl::Granted | TileControl::Engaged));
    // Everything the tile's buttons depend on, so they re-render only when it changes.
    let bar_info = {
        let pk = pk.clone();
        create_memo(move |_| {
            control.with(|c| {
                let mine = c.mine.get(&pk).copied().unwrap_or_default();
                let controllers = c.offers.get(&pk).map_or(0, |o| o.controllers);
                (mine.mode, mine.pad, mine.requested, controllers)
            })
        })
    };
    let holder = {
        let pk = pk.clone();
        move || {
            control
                .with(|c| c.offers.get(&pk).and_then(|o| o.mouse_keyboard.clone()))
                .map(|h| names.with(|n| n.get(&h).cloned()).unwrap_or_else(|| pubkey_tag(&h)))
        }
    };
    let pk_surface = pk.clone();
    let pk_bar = pk.clone();
    view! {
        <div
            id=format!("tile-{}", member.pubkey)
            class="tile"
            class:speaking=move || speaking.with(|s| s.contains(&pk_speaking))
            data-pubkey=member.pubkey.clone()
            data-control=move || tile_control.get().key()
            on:dblclick=move |_| on_fullscreen(Some(pk_dbl.clone()))
        >
            <video id=format!("tile-video-{}", member.pubkey) autoplay playsinline muted></video>
            {move || has_surface.get().then(|| {
                let pk = pk_surface.clone();
                view! {
                    <div
                        id=format!("control-surface-{pk}")
                        class="control-surface"
                        tabindex="0"
                        data-engaged=move || (tile_control.get() == TileControl::Engaged).to_string()
                        on:click=move |_| {
                            if tile_control.get_untracked() == TileControl::Granted {
                                on_control(pk.clone(), TileControlAction::Engage);
                            }
                        }
                    >
                        {move || (tile_control.get() == TileControl::Granted).then(|| view! {
                            <span class="control-hint">{move || t(lang.get(), "control_click_to_start")}</span>
                        })}
                    </div>
                }
            })}
            <button
                class="tile-fullscreen"
                title=move || t(lang.get(), "title_fullscreen")
                on:click=move |_| on_fullscreen(Some(pk_btn.clone()))
            >
                "⛶"
            </button>
            {move || {
                let pk = pk_bar.clone();
                let (mode, pad, requested, controllers) = bar_info.get();
                // Buttons shared by several states.
                let pad_badge = move || pad.map(|slot| view! {
                    <span class="control-pad-badge" title=move || t_replace_1(lang.get(), "control_pad_hint", "{n}", &(slot + 1).to_string())>
                        {format!("🎮 P{}", slot + 1)}
                    </span>
                });
                let waiting = move || requested.then(|| view! {
                    <span class="control-waiting">{move || t(lang.get(), "control_waiting")}</span>
                });
                let pad_button = {
                    let pk = pk.clone();
                    move || (controllers > 0 && pad.is_none() && !requested).then(|| {
                        let pk = pk.clone();
                        view! {
                            <button class="btn btn-sm btn-secondary control-request-pad-btn" on:click=move |_| on_control(pk.clone(), TileControlAction::RequestPad)>
                                {move || t(lang.get(), "btn_request_pad")}
                            </button>
                        }
                    })
                };
                match tile_control.get() {
                    TileControl::Offer => view! {
                        <div class="control-bar">
                            <button class="btn btn-sm btn-call control-request-btn" on:click=move |_| on_control(pk.clone(), TileControlAction::Request)>
                                {move || t(lang.get(), "btn_request_control")}
                            </button>
                            {pad_button}
                        </div>
                    }.into_view(),
                    TileControl::PadOnly => {
                        let pk_release = pk.clone();
                        view! {
                            <div class="control-bar">
                                {pad_badge}
                                {waiting}
                                {(!requested).then(|| view! {
                                    <button class="btn btn-sm btn-secondary control-request-btn" on:click=move |_| on_control(pk.clone(), TileControlAction::Request)>
                                        {move || t(lang.get(), "btn_request_control")}
                                    </button>
                                })}
                                <button class="btn btn-sm btn-secondary control-release-btn" on:click=move |_| on_control(pk_release.clone(), TileControlAction::Release)>
                                    {move || t(lang.get(), "btn_release_control")}
                                </button>
                            </div>
                        }.into_view()
                    }
                    TileControl::Requested => view! {
                        <div class="control-bar">
                            <span class="control-waiting">{move || t(lang.get(), "control_waiting")}</span>
                            <button class="btn btn-sm btn-secondary control-cancel-btn" on:click=move |_| on_control(pk.clone(), TileControlAction::Release)>
                                {move || t(lang.get(), "btn_cancel")}
                            </button>
                        </div>
                    }.into_view(),
                    TileControl::Granted => {
                        let pk_mode = pk.clone();
                        let game = mode == PointerMode::Game;
                        view! {
                            <div class="control-bar">
                                {pad_badge}
                                {waiting}
                                {pad_button}
                                <button class="btn btn-sm btn-secondary control-release-btn" on:click=move |_| on_control(pk.clone(), TileControlAction::Release)>
                                    {move || t(lang.get(), "btn_release_control")}
                                </button>
                                <button
                                    class="btn btn-sm btn-secondary control-mode-btn"
                                    data-mode=if game { "game" } else { "desktop" }
                                    title=move || t(lang.get(), "control_mode_title")
                                    on:click=move |_| on_control(pk_mode.clone(), TileControlAction::ToggleMode)
                                >
                                    {move || if game { t(lang.get(), "control_mode_game") } else { t(lang.get(), "control_mode_desktop") }}
                                </button>
                            </div>
                        }.into_view()
                    }
                    TileControl::Engaged | TileControl::Hidden => ().into_view(),
                }
            }}
            <span class="tile-label">
                <span dir="auto">{member.name}</span>
                {move || is_self.then(|| format!(" {}", t(lang.get(), "you_suffix")))}
                {move || lounge.with(|l| l.iter().any(|m| m.pubkey == pk_mic && m.mic_muted)).then(|| " 🔇")}
                {move || holder().map(|name| view! { <span class="control-holder">{format!(" · 🖱️ {name}")}</span> })}
            </span>
        </div>
    }
}

fn agent_status_key(status: &AgentStatus) -> &'static str {
    match status {
        AgentStatus::Off => "agent_status_off",
        AgentStatus::Connecting => "agent_status_connecting",
        AgentStatus::Paired { .. } => "agent_status_paired",
        AgentStatus::WrongCode => "agent_status_wrong_code",
        AgentStatus::Locked { .. } => "agent_status_locked",
        AgentStatus::Busy => "agent_status_busy",
        AgentStatus::Unreachable => "agent_status_unreachable",
        AgentStatus::VersionMismatch => "agent_status_version",
        AgentStatus::NotTheApp => "agent_status_not_the_app",
        AgentStatus::Stopped => "agent_status_stopped",
    }
}

/// A member asks to control this computer: Allow or Deny.
fn control_prompt(
    lang: ReadSignal<Language>,
    prompt: ControlPromptUi,
    names: ReadSignal<HashMap<String, String>>,
    on_answer: impl Fn(String, bool) + Copy + 'static,
) -> impl IntoView {
    let label = move |pk: &str| {
        let name = names.with_untracked(|n| n.get(pk).cloned()).unwrap_or_default();
        format!("{name} · {}", pubkey_tag(pk))
    };
    let who = label(&prompt.member);
    let who_pad = who.clone();
    let takes_over = prompt.takes_over.as_deref().map(label);
    let (allow_pk, deny_pk) = (prompt.member.clone(), prompt.member.clone());
    view! {
        <div class="control-prompt" role="alertdialog" data-pubkey=prompt.member.clone()>
            {prompt.mouse_keyboard.then(|| view! {
                <p class="control-prompt-text">{move || t_replace_1(lang.get(), "control_prompt_kbm", "{name}", &who)}</p>
            })}
            {(prompt.controller && !prompt.mouse_keyboard).then(|| view! {
                <p class="control-prompt-text">{move || t_replace_1(lang.get(), "control_prompt_pad", "{name}", &who_pad)}</p>
            })}
            {takes_over.map(|name| view! {
                <p class="control-prompt-note">{move || t_replace_1(lang.get(), "control_prompt_takes_over", "{name}", &name)}</p>
            })}
            {prompt.mouse_keyboard.then(|| view! {
                <p class="control-prompt-warning">{move || t(lang.get(), "control_prompt_warning")}</p>
            })}
            <div class="control-prompt-actions">
                <button class="btn btn-call btn-sm control-allow-btn" on:click=move |_| on_answer(allow_pk.clone(), true)>
                    {move || t(lang.get(), "btn_allow")}
                </button>
                <button class="btn btn-secondary btn-sm control-deny-btn" on:click=move |_| on_answer(deny_pk.clone(), false)>
                    {move || t(lang.get(), "btn_deny")}
                </button>
            </div>
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
#[allow(clippy::too_many_arguments)]
fn member_row(
    lang: ReadSignal<Language>,
    member: MemberUi,
    am_admin: Memo<bool>,
    on_kick: impl Fn(String, String) + Copy + 'static,
    dm_unread: ReadSignal<HashMap<String, usize>>,
    on_dm: impl Fn(String) + Copy + 'static,
    control: ReadSignal<ControlUi>,
    on_revoke: impl Fn(String) + Copy + 'static,
) -> impl IntoView {
    let control_pk = member.pubkey.clone();
    // What this member controls on this computer (shown to the sharer, with a revoke button).
    let controls_here = move || {
        control.with(|c| {
            let kbm = c.host_mouse_keyboard.as_deref() == Some(control_pk.as_str());
            let pad = c.host_pads.iter().position(|p| p.as_deref() == Some(control_pk.as_str()));
            (kbm || pad.is_some()).then_some((kbm, pad))
        })
    };
    let revoke_pk = member.pubkey.clone();
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
            {move || controls_here().map(|(kbm, pad)| {
                let pk = revoke_pk.clone();
                let mut badge = String::new();
                if kbm {
                    badge.push_str("🖱️");
                }
                if let Some(slot) = pad {
                    badge.push_str(&format!("🎮P{}", slot + 1));
                }
                view! {
                    <span class="control-badge" data-kind=if kbm { "kbm" } else { "pad" }>{badge}</span>
                    <button
                        class="btn btn-sm btn-secondary control-revoke-btn"
                        title=move || t(lang.get(), "title_revoke_control")
                        on:click=move |_| on_revoke(pk.clone())
                    >
                        "✕"
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
