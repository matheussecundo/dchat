mod agent;
mod held_media;
mod i18n;
mod ice;
mod layout;
mod media;
mod media_card;
mod mesh;
mod names;
mod nostr_pool;
mod pwa;
mod qr;
mod recorder;
mod remote_input;
mod session;
mod staging;
mod state;
mod stats;
mod voice_mp3;

use i18n::{
    detect_browser_language, large_file_warning_desc, t, t_replace_1, update_document_direction,
    Language,
};
use leptos::*;
use names::{pubkey_tag, random_name, sanitize_name, MAX_NAME_CHARS};
use protocol::video::{CameraPreset, ScreenPreset, VideoPresets};
use protocol::{
    format_relay_list, parse_cap, password_room_key, split_relay_input, stretch_password, FragmentParams, RoomParams, VideoKind,
    DEFAULT_MEMBER_CAP, DEFAULT_VIDEO_CAP, DEFAULT_VOICE_CAP, KEY_LENGTH, REACTIONS, ControlWants, MonitorInfo,
    DEFAULT_AGENT_PORT, PointerMode,
};
use agent::{AgentLink, AgentSignals, AgentStatus};
use qr::generate_qr_svg;
use remote_input::{InputCapture, InputSink, PadPoller};
use std::rc::Rc;
use session::{save_blob, save_finished, start_save, RoomSession, SessionSignals};
use held_media::HeldMedia;
use media_card::{close_viewer, media_card, media_viewer, preview_of, MediaCtx, ViewerItem};
use protocol::media::MediaKind;
use recorder::{recording_panel, review_panel, MicPress, Phase, RecMode, Recorder};
use staging::{staged_chip, StagedFile};
use state::{
    admin_url, create_room, current_fragment, format_file_size, host_download_url, fragment_relay_choice, invite_url, read_credentials,
    selectable_devices, AudioSettings, DeviceChoice, DeviceEntry, RelayMode, CAMERA_KIND, MIC_KIND, SPEAKER_KIND,
    ChatMessageUi, ConnectionStatus, DmUi, DownloadSummary, FileOfferInfo, FileTransferStatus, LinkUi,
    LoungeMemberUi, MemberUi, MyVoiceUi, Notice, RekeyTarget, RoomCaps, ControlUi, ControlPromptUi, SessionCarry,
};
use protocol::chat_log::ChatLog;
use std::cell::RefCell;
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
    let (hide_ip_input, set_hide_ip_input) = create_signal(false);
    // Typed at creation or on the join screen; cleared once stretched.
    let (password_input, set_password_input) = create_signal(String::new());
    // Creating a password room is opt-in: the box exists only while this is ticked, so a
    // password the browser saved earlier is never filled into a new room by itself.
    let (password_wanted, set_password_wanted) = create_signal(false);
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
    // The room's chat log (signed originals, synced with members): RAM only, carried across
    // rekeys, emptied on a fresh entry or removal.
    let chat_log = store_value(Rc::new(RefCell::new(ChatLog::new())));
    let (history_loading, set_history_loading) = create_signal(false);
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

    // Installing the app (PWA): the browser's install offer, read again whenever it changes.
    let installed = pwa::is_installed();
    let apple_install_hint = pwa::apple_install_hint();
    let (install_offered, set_install_offered) = create_signal(pwa::install_available());
    pwa::on_install_change(move || set_install_offered.set(pwa::install_available()));
    // "Join with a link" in the lobby: any dchat site's link, of which only the fragment is used.
    let (join_link_input, set_join_link_input) = create_signal(String::new());
    let (join_link_invalid, set_join_link_invalid) = create_signal(false);
    let join_with_link = move || match FragmentParams::from_link(&join_link_input.get_untracked()) {
        Some(params) => state::join_link(&params),
        None => set_join_link_invalid.set(true),
    };

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
    let chat_container_ref = create_node_ref::<leptos::html::Main>();
    let chat_at_bottom = store_value(true);
    let (no_turn, set_no_turn) = create_signal(false);

    // Voice lounge
    let (lounge, set_lounge) = create_signal(Vec::<LoungeMemberUi>::new());
    let (my_voice, set_my_voice) = create_signal(MyVoiceUi::default());
    let (speaking, set_speaking) = create_signal(HashSet::<String>::new());
    let (voice_prompt, set_voice_prompt) = create_signal(Option::<String>::None);
    let (audio_settings, set_audio_settings) = create_signal(AudioSettings::default());
    // The sharer's video quality presets: RAM only, kept across voice rejoins and rekeys.
    let (video_presets, set_video_presets) = create_signal(VideoPresets::default());
    // Microphone, speaker and camera (None: the system default). RAM only, kept across
    // voice rejoins and rekeys.
    let (device_choice, set_device_choice) = create_signal(DeviceChoice::default());
    // Most connections one of our uploads may use (1 = the main link only). RAM only, kept
    // across rekeys.
    let (file_connections, set_file_connections) = create_signal(protocol::transfer::MAX_FILE_CONNECTIONS);
    // What the browser lists, refreshed while the settings are open.
    let (device_list, set_device_list) = create_signal(Vec::<DeviceEntry>::new());
    // A speaker picked in the browser's own chooser (Firefox), which it may not list.
    let (picked_speaker, set_picked_speaker) = create_signal(None::<DeviceEntry>);
    // Safari can't choose where audio plays.
    let speaker_supported = media::speaker_selection_supported();
    // The open ▾ quality menu (camera or screen), placed next to its button.
    let (quality_menu, set_quality_menu) = create_signal(None::<QualityMenu>);
    // Mobile browsers can't share a screen: no screen quality controls there.
    let screen_supported = media::screen_capture_supported();

    // Chat extras
    let (typing, set_typing) = create_signal(Vec::<String>::new());
    let (mention_count, set_mention_count) = create_signal(0usize);
    let (dms, set_dms) = create_signal(HashMap::<String, Vec<DmUi>>::new());
    let (dm_unread, set_dm_unread) = create_signal(HashMap::<String, usize>::new());
    let (dm_open, set_dm_open) = create_signal(Option::<String>::None);
    let dm_thread_ref = create_node_ref::<leptos::html::Div>();
    let dm_at_bottom = store_value(true);
    let (dm_input, set_dm_input) = create_signal(String::new());
    let (editing, set_editing) = create_signal(Option::<String>::None);
    let (react_picker, set_react_picker) = create_signal(Option::<String>::None);

    // File sharing: files waiting to be sent (picked, dropped or pasted).
    let staged = create_rw_signal(Vec::<StagedFile>::new());
    // Media loaded for viewing in the chat (RAM only, kept across rekeys), files this browser
    // couldn't decode, and the fullscreen viewer.
    let held_media = create_rw_signal(HeldMedia::default());
    let failed_media = create_rw_signal(HashMap::<String, String>::new());
    let viewer = create_rw_signal(None::<ViewerItem>);
    let media_ctx = MediaCtx { lang, members, held: held_media, failed: failed_media, viewer, messages: set_messages };
    media_card::install_playback_rules();
    // Voice messages: app level, so a recording under review survives a rekey.
    let recorder = Recorder::new();
    // Files dragged over the window (a counter: enter and leave fire for every element).
    let drag_depth = store_value(0_i32);
    let (dragging, set_dragging) = create_signal(false);
    let (large_file_warning, set_large_file_warning) = create_signal(Option::<(String, String)>::None);
    // Finished downloads waiting for a tap on Save (iOS), by file id: RAM only, kept across
    // rekeys, dropped once handed over or when this tab leaves the room.
    let ready_files = store_value(HashMap::<String, web_sys::File>::new());
    let (show_audio_settings, set_show_audio_settings) = create_signal(false);

    let session_ref = store_value(None::<RoomSession>);
    // The session name, reused when an admin moves the room to a new link.
    let my_name = store_value(String::new());
    // TURN servers the host offered (fetched once, reused if the room moves).
    let host_ice = store_value(None::<js_sys::Array>);
    // A password room's stretched password, RAM only: it also opens the room after a rekey.
    let stretched_password = store_value(None::<[u8; KEY_LENGTH]>);

    let start_session = move |room_id: String, link_key: [u8; KEY_LENGTH], carry: Option<SessionCarry>| -> Option<RoomSession> {
        if carry.is_none() {
            chat_log.with_value(|log| log.borrow_mut().clear());
        }
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
            devices: set_device_choice,
            ready_files,
            chat_log,
            history_loading: set_history_loading,
            held_media,
        };
        set_room_id_sig.set(room_id.clone());
        match RoomSession::start(room_id, key, my_name.get_value(), signals, carry, host_ice.get_value()) {
            Ok(session) => {
                session_ref.set_value(Some(session.clone()));
                session.set_allow_control(allow_control.get_untracked());
                session.attach_agent(agent_link.get_value());
                // Before anyone joins voice here (a rekey rejoins right after this returns).
                session.set_audio_settings(audio_settings.get_untracked());
                session.set_video_presets(video_presets.get_untracked());
                session.set_devices(device_choice.get_untracked());
                session.set_file_connections(file_connections.get_untracked());
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
            start_session(room_id, key, None);
            set_entering.set(false);
        });
    };

    // An admin moved the room: follow it with a fresh session, keeping the chat on screen.
    create_effect(move |_| {
        if let Some(target) = rekey.get() {
            set_rekey.set(None);
            if let Some(session) = start_session(target.room, target.key, Some(target.carry)) {
                if target.rejoin_voice {
                    session.join_voice();
                }
            }
        }
    });
    create_effect(move |_| {
        if removed.get() {
            ready_files.update_value(|ready| ready.clear());
            viewer.set(None);
            held_media.update(|held| held.clear());
            failed_media.update(|failed| failed.clear());
            recorder.reset();
            staging::clear(staged);
            chat_log.with_value(|log| log.borrow_mut().clear());
            set_screen.set(Screen::Removed);
        }
    });

    let create_and_enter = move || {
        let caps = RoomCaps {
            members: parse_cap(Some(&cap_input.get_untracked()), DEFAULT_MEMBER_CAP),
            voice: parse_cap(Some(&voice_cap_input.get_untracked()), DEFAULT_VOICE_CAP),
            video: parse_cap(Some(&video_cap_input.get_untracked()), DEFAULT_VIDEO_CAP),
            hide_ip: hide_ip_input.get_untracked(),
            password: password_wanted.get_untracked() && !password_input.get_untracked().trim().is_empty(),
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
        let files = staged.get_untracked();
        if text.is_empty() && files.is_empty() {
            return;
        }
        // Their previews are still being made (a moment, at most a few seconds).
        if files.iter().any(|f| f.preparing) {
            return;
        }
        chat_at_bottom.set_value(true);
        if let Some(el) = chat_container_ref.get_untracked() {
            el.set_scroll_top(el.scroll_height());
        }
        session_ref.with_value(|session| {
            let Some(session) = session else {
                return;
            };
            if files.is_empty() {
                match session.send_chat(&text) {
                    Ok(()) => set_input_text.set(String::new()),
                    Err(err) => log::warn!("Failed to send: {err}"),
                }
                return;
            }
            // One card per file, in order; the caption goes on the first.
            let mut caption = (!text.is_empty()).then_some(text.clone());
            for item in files {
                match session.share_file(item.file.clone(), caption.clone(), item.media.clone()) {
                    Ok(file_id) => {
                        caption = None;
                        set_input_text.set(String::new());
                        // Our own card shows the file we picked (its URL moves to the store).
                        staged.update(|s| s.retain(|i| i.key != item.key));
                        if item.kind.is_some() {
                            held_media.update(|held| held.insert_own(&file_id, &item.file, item.url.clone()));
                        }
                    }
                    Err(err) => {
                        log::warn!("Failed to send: {err}");
                        set_toast.set(Some("toast_send_failed"));
                        return;
                    }
                }
            }
        });
    };
    // A voice message under review: offered like any file, pulled from this device.
    let send_take = move || {
        let Some(take) = recorder.take() else {
            return;
        };
        let Some(file) = take.file() else {
            recorder.phase.set(Phase::Review(take));
            return;
        };
        chat_at_bottom.set_value(true);
        let mut sent = false;
        session_ref.with_value(|session| {
            if let Some(session) = session {
                match session.share_file(file.clone(), None, Some(take.media())) {
                    Ok(file_id) => {
                        held_media.update(|held| held.insert_own(&file_id, &file, Some(take.url.clone())));
                        sent = true;
                    }
                    Err(err) => log::warn!("Failed to send the voice message: {err}"),
                }
            }
        });
        if !sent {
            set_toast.set(Some("toast_send_failed"));
            recorder.phase.set(Phase::Review(take));
        }
    };
    // Picked, dropped or pasted files join the ones waiting to be sent.
    let stage_files = move |files: Vec<web_sys::File>| {
        if !files.is_empty() && staging::stage(staged, files) > 0 {
            set_toast.set(Some("toast_too_many_files"));
        }
    };
    // What the message bar shows of the recorder (changes only between modes, not with
    // every level sample).
    let rec_mode = create_memo(move |_| recorder.phase.with(RecMode::of));
    // 🎤 stands in for Send while there is nothing to send.
    let show_mic = move || {
        rec_mode.get() == RecMode::Idle
            && editing.with(|e| e.is_none())
            && input_text.with(|t| t.trim().is_empty())
            && staged.with(|s| s.is_empty())
    };
    let start_recording = move |held: bool| {
        recorder.start(held, audio_settings.get_untracked(), device_choice.get_untracked().mic, set_toast);
    };
    let mic_press = MicPress::new(recorder, start_recording);

    create_effect(move |_| {
        let count = messages.with(|m| m.len());
        let in_room = screen.get() == Screen::Room;
        if in_room && count > 0 && chat_at_bottom.get_value() {
            request_animation_frame(move || {
                if let Some(el) = chat_container_ref.get_untracked() {
                    el.set_scroll_top(el.scroll_height());
                }
            });
        }
    });

    {
        let on_chat_resize = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            if chat_at_bottom.get_value() {
                if let Some(el) = chat_container_ref.get_untracked() {
                    el.set_scroll_top(el.scroll_height());
                }
            }
        }) as Box<dyn FnMut()>);
        if let Some(win) = window() {
            let _ = win.add_event_listener_with_callback("resize", on_chat_resize.as_ref().unchecked_ref());
        }
        on_chat_resize.forget();
    }

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
    // Recording a voice message mutes our lounge mic (people in voice don't hear what is
    // recorded for the chat), and puts it back as it was afterwards.
    let capturing = create_memo(move |_| recorder.phase.with(Phase::capturing));
    let unmute_after_recording = store_value(false);
    create_effect(move |_| {
        let voice = my_voice.get_untracked();
        if capturing.get() {
            if voice.in_voice && !voice.mic_muted {
                with_session(&|s| s.toggle_mic());
                unmute_after_recording.set_value(true);
            }
        } else if unmute_after_recording.get_value() {
            unmute_after_recording.set_value(false);
            if voice.in_voice && voice.mic_muted {
                with_session(&|s| s.toggle_mic());
            }
        }
    });
    #[cfg(feature = "e2e-hooks")]
    install_media_hooks(held_media, recorder);

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
    // For the video quality pickers (settings modal and the quick menus).
    let apply_video_presets = move |presets: VideoPresets| {
        set_video_presets.set(presets);
        with_session(&|s| s.set_video_presets(presets));
    };
    let toggle_quality_menu = move |source: QualitySource, ev: web_sys::MouseEvent| {
        if quality_menu.with_untracked(|m| m.is_some_and(|m| m.source == source)) {
            set_quality_menu.set(None);
            return;
        }
        // Delegated events have no useful currentTarget: anchor on the clicked split button.
        let anchor = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest(".split-btn").ok().flatten());
        if let Some(anchor) = anchor {
            let pos = MenuPos::next_to(&anchor, lang.get_untracked().is_rtl());
            set_quality_menu.set(Some(QualityMenu { source, pos }));
            // Keyboard users land on the current choice (Escape closes). Without scrolling:
            // a scroll of the page would close the menu again.
            if let Some(selected) =
                window().and_then(|w| w.document()).and_then(|d| d.query_selector(".quality-menu .quality-option.selected").ok().flatten())
            {
                let options = js_sys::Object::new();
                let _ = js_sys::Reflect::set(&options, &"preventScroll".into(), &true.into());
                if let Ok(focus) = js_sys::Reflect::get(&selected, &"focus".into()).and_then(|f| f.dyn_into::<js_sys::Function>()) {
                    let _ = focus.call1(&selected, &options);
                }
            }
        }
    };
    let pick_quality = move |presets: VideoPresets| {
        apply_video_presets(presets);
        set_quality_menu.set(None);
    };
    // The menus belong to the in-voice controls.
    create_effect(move |_| {
        if !my_voice.with(|v| v.in_voice) && quality_menu.with_untracked(Option::is_some) {
            set_quality_menu.set(None);
        }
    });
    {
        // A press anywhere but the menu (or its button) closes the quick quality menu, and
        // so does anything that moves its button: scrolling the page, resizing.
        let close = move || {
            if quality_menu.with_untracked(Option::is_some) {
                set_quality_menu.set(None);
            }
        };
        let on_press = wasm_bindgen::closure::Closure::wrap(Box::new(move |ev: web_sys::Event| {
            let inside = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .is_some_and(|el| el.closest(".quality-menu, .quality-btn").ok().flatten().is_some());
            if !inside {
                close();
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        let on_scroll = wasm_bindgen::closure::Closure::wrap(Box::new(move |ev: web_sys::Event| {
            // Only the page and #app carry the lounge bar; the chat or the menu scrolling doesn't.
            let moves_button = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .map_or(true, |el| el.id() == "app" || el.tag_name().eq_ignore_ascii_case("html"));
            if moves_button {
                close();
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        let on_resize = wasm_bindgen::closure::Closure::wrap(Box::new(close) as Box<dyn FnMut()>);
        if let Some(win) = window() {
            let _ = win.add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());
            if let Some(doc) = win.document() {
                let _ = doc.add_event_listener_with_callback("pointerdown", on_press.as_ref().unchecked_ref());
                // Scroll events don't bubble: listen in the capture phase to see #app's.
                let _ = doc.add_event_listener_with_callback_and_bool("scroll", on_scroll.as_ref().unchecked_ref(), true);
            }
        }
        on_press.forget();
        on_scroll.forget();
        on_resize.forget();
    }
    // ---- Devices --------------------------------------------------------------------------
    let apply_devices = move |choice: DeviceChoice| {
        set_device_choice.set(choice.clone());
        with_session(&|s| s.set_devices(choice.clone()));
    };
    let refresh_devices = move || {
        wasm_bindgen_futures::spawn_local(async move {
            match media::enumerate_devices().await {
                Ok(list) => set_device_list.set(list),
                Err(err) => log::warn!("enumerateDevices failed: {:?}", err),
            }
        });
    };
    // Firefox lists no speakers until one is picked in its own chooser (from this click).
    let choose_speaker = move |_| {
        let picked = media::select_audio_output();
        wasm_bindgen_futures::spawn_local(async move {
            match picked.await {
                Ok(device) if !device.device_id.is_empty() => {
                    set_picked_speaker.set(Some(device.clone()));
                    apply_devices(DeviceChoice { speaker: Some(device.device_id), ..device_choice.get_untracked() });
                    refresh_devices();
                }
                Ok(_) => {}
                Err(err) => log::info!("No speaker picked: {:?}", err),
            }
        });
    };
    {
        // Plugged or unplugged while the settings are open: list again (one listener for
        // the page's lifetime).
        let on_change = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
            if show_audio_settings.get_untracked() {
                refresh_devices();
            }
        }) as Box<dyn FnMut()>);
        if let Some(media_devices) = window().and_then(|w| w.navigator().media_devices().ok()) {
            let _ = media_devices.add_event_listener_with_callback("devicechange", on_change.as_ref().unchecked_ref());
        }
        on_change.forget();
    }
    let open_audio_settings = move |_| {
        // The modal lives outside the video grid, so it would be hidden in fullscreen.
        if let Some(doc) = window().and_then(|w| w.document()) {
            if doc.fullscreen_element().is_some() {
                doc.exit_fullscreen();
            }
        }
        refresh_devices();
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
            if ev.key() == "Escape" && quality_menu.with_untracked(Option::is_some) {
                set_quality_menu.set(None);
            }
            if ev.key() == "Escape" && viewer.with_untracked(Option::is_some) {
                close_viewer(media_ctx);
            }
        }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
        if let Some(doc) = window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback("fullscreenchange", on_change.as_ref().unchecked_ref());
            let _ = doc.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref());
        }
        on_change.forget();
        on_key.forget();
    }
    // Files dragged over the room: a full-window "Drop to attach". A file dropped anywhere in
    // the room is never opened by the browser in place of it (that would wipe the room).
    {
        let can_attach = move || {
            screen.get_untracked() == Screen::Room
                && status.with_untracked(ConnectionStatus::in_room)
                && recorder.phase.with_untracked(|p| *p == Phase::Idle)
        };
        let on_drag = wasm_bindgen::closure::Closure::wrap(Box::new(move |ev: web_sys::DragEvent| {
            if screen.get_untracked() != Screen::Room {
                return;
            }
            let Some(transfer) = ev.data_transfer().filter(staging::carries_files) else {
                return;
            };
            ev.prevent_default();
            match ev.type_().as_str() {
                "dragenter" => {
                    drag_depth.update_value(|d| *d += 1);
                    set_dragging.set(can_attach());
                }
                "dragleave" => {
                    drag_depth.update_value(|d| *d = (*d - 1).max(0));
                    if drag_depth.get_value() == 0 {
                        set_dragging.set(false);
                    }
                }
                "dragover" => transfer.set_drop_effect(if can_attach() { "copy" } else { "none" }),
                _ => {
                    drag_depth.set_value(0);
                    set_dragging.set(false);
                    if can_attach() {
                        stage_files(staging::files_of(&transfer));
                    }
                }
            }
        }) as Box<dyn FnMut(web_sys::DragEvent)>);
        if let Some(win) = window() {
            for name in ["dragenter", "dragover", "dragleave", "drop"] {
                let _ = win.add_event_listener_with_callback(name, on_drag.as_ref().unchecked_ref());
            }
        }
        on_drag.forget();
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
        // Already here (viewed, or our own): save the copy in RAM, nothing is pulled again.
        if let Some((blob, name)) = held_media.with_untracked(|held| held.blob(&file_id)) {
            save_blob(&blob, &name);
            return;
        }
        // Media that can be viewed here loads into the chat first, then saves.
        let viewable = messages.with_untracked(|msgs| {
            msgs.iter()
                .find(|m| m.id == file_id)
                .and_then(|m| m.file.as_ref())
                .is_some_and(|f| failed_media.with_untracked(|failed| preview_of(f, failed)).is_some_and(|p| p.playable))
        });
        if viewable {
            with_session(&|s| s.load_media(&file_id, true));
            return;
        }
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
    // Straight from the tap: the share sheet needs it.
    let save_file = move |file_id: String| {
        let Some(file) = ready_files.with_value(|ready| ready.get(&file_id).cloned()) else {
            return;
        };
        let started = start_save(&file);
        wasm_bindgen_futures::spawn_local(async move {
            if !save_finished(started).await {
                return;
            }
            ready_files.update_value(|ready| {
                ready.remove(&file_id);
            });
            set_messages.update(|msgs| {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                    if let Some(file) = msg.file.as_mut() {
                        if let FileTransferStatus::ReadyToSave { summary, withdrawn } = file.status {
                            file.status = FileTransferStatus::Completed { summary, withdrawn };
                        }
                        msg.rev += 1;
                    }
                }
            });
        });
    };
    let file_action = move |action: FileAction, file_id: String| match action {
        FileAction::Download => download_file(file_id),
        FileAction::Save => save_file(file_id),
        FileAction::Cancel => with_session(&|s| s.cancel_download(&file_id)),
        FileAction::Withdraw => with_session(&|s| s.withdraw_file(&file_id)),
        FileAction::Load => with_session(&|s| s.load_media(&file_id, false)),
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
        dm_at_bottom.set_value(true);
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
        dm_at_bottom.set_value(true);
        if let Some(el) = dm_thread_ref.get_untracked() {
            el.set_scroll_top(el.scroll_height());
        }
        let sent = session_ref.with_value(|s| s.as_ref().map(|s| s.send_dm(&peer, &text)));
        match sent {
            Some(Ok(())) => {
                set_dm_input.set(String::new());
                request_animation_frame(move || {
                    if let Some(el) = dm_thread_ref.get_untracked() {
                        el.set_scroll_top(el.scroll_height());
                    }
                });
            }
            Some(Err(err)) => log::warn!("DM not sent: {err}"),
            None => {}
        }
    };
    create_effect(move |_| {
        if let Some(peer) = dm_open.get() {
            let _ = dms.with(|d| d.get(&peer).map(|v| v.len()));
            if dm_at_bottom.get_value() {
                request_animation_frame(move || {
                    if let Some(el) = dm_thread_ref.get_untracked() {
                        el.set_scroll_top(el.scroll_height());
                    }
                });
            }
        }
    });
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
                pwa::set_badge(count);
            } else {
                doc.set_title(&base_title);
                pwa::set_badge(0);
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
    // A room link handed to the installed app's open window (Android has only one). A new
    // window (desktop) gets the link it was opened with: same room, nothing to do.
    pwa::on_launch(move |target| {
        let Some(params) = FragmentParams::from_link(&target) else {
            return;
        };
        if params.get("room") == read_credentials().map(|(room, _)| room).as_deref() {
            return;
        }
        if screen.get_untracked() == Screen::Room && !confirm(t(lang.get_untracked(), "confirm_switch_room").to_string()) {
            return;
        }
        state::join_link(&params);
    });
    let kick_member = move |pubkey: String, name: String| {
        if confirm(t_replace_1(lang.get_untracked(), "confirm_kick", "{name}", &name)) {
            with_session(&|s| s.kick(&pubkey));
        }
    };
    let make_admin_member = move |pubkey: String, name: String| {
        if confirm(t_replace_1(lang.get_untracked(), "confirm_make_admin", "{name}", &name)) {
            with_session(&|s| s.make_admin(&pubkey));
        }
    };
    let rotate_link = move |_| {
        if confirm(t(lang.get_untracked(), "confirm_rotate").to_string()) {
            with_session(&|s| s.rotate_link());
        }
    };
    let hides_ip = move || session_ref.with_value(|s| s.as_ref().is_some_and(|s| s.hides_ip()));
    let has_password = move || session_ref.with_value(|s| s.as_ref().is_some_and(|s| s.has_password()));

    let is_connected = move || status.get() == ConnectionStatus::Connected;
    // Messages and files can be posted from the moment we're in the room, even alone: they
    // wait in the chat log, and whoever joins gets them with the history.
    let can_post = move || status.with(ConnectionStatus::in_room);
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
                <div class="lobby-stack">
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
                        <label class="lobby-check" for="password-checkbox">
                            <input
                                type="checkbox"
                                id="password-checkbox"
                                prop:checked=move || password_wanted.get()
                                on:change=move |ev| {
                                    let wanted = event_target_checked(&ev);
                                    set_password_wanted.set(wanted);
                                    if !wanted {
                                        set_password_input.set(String::new());
                                    }
                                }
                            />
                            <span>{move || t(lang.get(), "password_checkbox_label")}</span>
                        </label>
                        // Chromium ignores autocomplete="off" on password boxes and would fill a
                        // saved password into every new room; "new-password" makes it never fill one.
                        {move || password_wanted.get().then(|| view! {
                            <label class="lobby-label" for="password-input">{move || t(lang.get(), "join_password_label")}</label>
                            <input
                                id="password-input"
                                class="lobby-input"
                                type="password"
                                autocomplete="new-password"
                                prop:value=move || password_input.get()
                                on:input=move |ev| set_password_input.set(event_target_value(&ev))
                            />
                            <p class="lobby-hint">{move || t(lang.get(), "password_hint")}</p>
                        })}
                        {move || cap_is_large().then(|| view! {
                            <p class="lobby-warning">{move || t(lang.get(), "cap_warning")}</p>
                        })}
                        <button id="create-room-btn" type="submit" class="btn btn-primary lobby-submit" disabled=move || entering.get() || !relay_choice_valid()>
                            {move || enter_label("btn_create_room")}
                        </button>
                    </form>
                    <form class="lobby-card join-link-card" autocomplete="off" on:submit=move |ev| { ev.prevent_default(); join_with_link(); }>
                        <label class="lobby-label" for="join-link-input">{move || t(lang.get(), "join_link_label")}</label>
                        <div class="join-link-row">
                            <input
                                id="join-link-input"
                                class="lobby-input"
                                type="text"
                                autocomplete="off"
                                inputmode="url"
                                autocapitalize="off"
                                spellcheck="false"
                                placeholder="https://…/#room=…&key=…"
                                prop:value=move || join_link_input.get()
                                on:input=move |ev| {
                                    set_join_link_input.set(event_target_value(&ev));
                                    set_join_link_invalid.set(false);
                                }
                            />
                            <button id="join-link-btn" type="submit" class="btn btn-secondary" disabled=move || join_link_input.get().trim().is_empty()>
                                {move || t(lang.get(), "join_link_btn")}
                            </button>
                        </div>
                        {move || join_link_invalid.get().then(|| view! {
                            <p class="lobby-warning join-link-invalid">{move || t(lang.get(), "join_link_invalid")}</p>
                        })}
                    </form>
                    // Installing is offered here only: from a page without a room in its address.
                    {move || (install_offered.get() && !installed).then(|| view! {
                        <button id="install-app-btn" type="button" class="btn btn-secondary install-app-btn" on:click=move |_| pwa::prompt_install()>
                            {move || t(lang.get(), "install_app")}
                        </button>
                    })}
                    {apple_install_hint.then(|| view! {
                        <p class="lobby-hint install-hint">{move || t(lang.get(), "install_hint_apple")}</p>
                    })}
                </div>
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
                        <p class="lobby-hint password-room-hint">{move || t(lang.get(), "join_password_ask")}</p>
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
                        Notice::NowAdmin(name) => t_replace_1(lang.get(), "sys_now_admin", "{name}", name),
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
            Some(file) => {
                let file_id = file.file_id.clone();
                let live = create_memo(move |_| {
                    messages.with(|msgs| msgs.iter().find(|m| m.id == file_id).and_then(|m| m.file.as_ref().map(|f| f.status.clone())))
                });
                match failed_media.with_untracked(|failed| preview_of(&file, failed)) {
                    Some(preview) => {
                        let actions = file_actions(lang, &file, live, msg.author.clone(), members, file_action, Some((held_media, preview.kind)));
                        media_card(media_ctx, preview, file, live, msg.text.clone(), msg.author.clone(), msg.is_self, actions, file_action)
                            .into_view()
                    }
                    None => file_card(lang, file, live, msg.text.clone(), msg.author.clone(), members, file_action).into_view(),
                }
            }
            None => view! { <div class="message-bubble" dir="auto">{msg.text.clone()}</div> }.into_view(),
        };
        let id = msg.id.clone();
        let me = members.with_untracked(|m| m.iter().find(|x| x.link == LinkUi::Me).map(|x| x.pubkey.clone()));
        // Read live: a reaction changes this, not the row (a video playing in it keeps playing).
        let reactions = {
            let id = id.clone();
            create_memo(move |_| messages.with(|msgs| msgs.iter().find(|m| m.id == id).map(|m| m.reactions.clone()).unwrap_or_default()))
        };
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
                {move || {
                    let reactions = reactions.get();
                    let (id, me) = (id.clone(), me.clone());
                    (!reactions.is_empty()).then(move || view! {
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
                    })
                }}
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
                    // Any admin whose link holds the admin secret: the creator, or a member who took
                    // over or was made admin (their link gained it then).
                    {move || (am_admin.get() && admin_url().is_some()).then(|| view! {
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
                            <div class="split-btn">
                                <button
                                    id="camera-btn"
                                    class=if mine.video == VideoKind::Camera { "btn btn-mute active" } else { "btn btn-mute" }
                                    disabled=move || mine.video == VideoKind::None && video_full()
                                    on:click=move |_| with_session(&|s| s.toggle_camera())
                                    title=move || if mine.video == VideoKind::None && video_full() { t(lang.get(), "video_full_title") } else { t(lang.get(), "title_camera") }
                                >
                                    "📹"
                                </button>
                                {quality_button(lang, QualitySource::Camera, quality_menu, toggle_quality_menu)}
                            </div>
                            {(mine.video == VideoKind::Camera).then(|| view! {
                                <button
                                    id="flip-camera-btn"
                                    class="btn btn-secondary"
                                    on:click=move |_| with_session(&|s| s.flip_camera())
                                    title=move || {
                                        let key = if device_choice.with(|d| d.camera.is_some()) { "title_next_camera" } else { "title_flip_camera" };
                                        t(lang.get(), key)
                                    }
                                >
                                    "🔄"
                                </button>
                            })}
                            <div class="split-btn">
                                <button
                                    id="screen-btn"
                                    class=if mine.video == VideoKind::Screen { "btn btn-mute active" } else { "btn btn-mute" }
                                    disabled=move || mine.video == VideoKind::None && video_full()
                                    on:click=move |_| with_session(&|s| s.toggle_screen())
                                    title=move || if mine.video == VideoKind::Screen { t(lang.get(), "title_stop_screen") } else { t(lang.get(), "title_screen_share") }
                                >
                                    "🖥️"
                                </button>
                                {screen_supported.then(|| quality_button(lang, QualitySource::Screen, quality_menu, toggle_quality_menu))}
                            </div>
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
                        children=move |m| video_tile(lang, m, lounge, speaking, toggle_fullscreen, control, controlling, names, on_tile_control, session_ref)
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
                <main
                    class="chat-container"
                    node_ref=chat_container_ref
                    on:scroll=move |_| {
                        if let Some(el) = chat_container_ref.get_untracked() {
                            let scroll_top = el.scroll_top();
                            let scroll_height = el.scroll_height();
                            let client_height = el.client_height();
                            chat_at_bottom.set_value(scroll_height - scroll_top - client_height <= 60);
                        }
                    }
                >
                    {move || history_loading.get().then(|| view! {
                        <div class="history-loading" role="status">{move || t(lang.get(), "history_loading")}</div>
                    })}
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
                            children=move |m| member_row(lang, m, am_admin, kick_member, make_admin_member, dm_unread, open_dm, control, move |pk: String| with_session(&|s| s.revoke_control(&pk)))
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

            {move || staged.with(|s| !s.is_empty()).then(|| view! {
                <div class="attachment-chips">
                    <For
                        each=move || staged.get()
                        key=|item| (item.key, item.preparing, item.gps, item.url.clone())
                        children=move |item| staged_chip(lang, staged, item)
                    />
                </div>
            })}

            <input
                type="file"
                id="file-input-hidden"
                multiple=true
                style="display: none;"
                on:change=move |ev| {
                    let target: HtmlInputElement = event_target(&ev);
                    let files = target.files().map(|list| (0..list.length()).filter_map(|i| list.get(i)).collect()).unwrap_or_default();
                    stage_files(files);
                    target.set_value("");
                }
            />

            <footer class="input-bar" class:recording=move || rec_mode.get().capturing() class:reviewing=move || rec_mode.get() == RecMode::Review>
                <button
                    class="btn btn-secondary attach-btn"
                    class:is-hidden=move || rec_mode.get() != RecMode::Idle
                    disabled=move || !can_post()
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
                {move || match rec_mode.get() {
                    RecMode::Idle => ().into_view(),
                    RecMode::Review => match recorder.phase.get_untracked() {
                        Phase::Review(take) => review_panel(lang, take, recorder, can_post, send_take).into_view(),
                        _ => ().into_view(),
                    },
                    mode => recording_panel(lang, recorder, mode).into_view(),
                }}
                <input
                    type="text"
                    autocomplete="off"
                    class:is-hidden=move || rec_mode.get() != RecMode::Idle
                    spellcheck=move || if spellcheck_on.get() { "true" } else { "false" }
                    on:paste=move |ev| {
                        // Pasted files (a screenshot, files copied in a file manager) are staged;
                        // pasted text goes into the box as usual.
                        let ev: web_sys::ClipboardEvent = ev.unchecked_into();
                        let Some(transfer) = ev.clipboard_data() else {
                            return;
                        };
                        let files = staging::files_of(&transfer);
                        if files.is_empty() || !can_post() {
                            return;
                        }
                        ev.prevent_default();
                        stage_files(files.into_iter().map(staging::named_for_paste).collect());
                    }
                    placeholder=move || {
                        if !is_connected() {
                            t(lang.get(), "placeholder_waiting")
                        } else if staged.with(|s| !s.is_empty()) {
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
                    class:is-hidden=move || show_mic() || rec_mode.get() != RecMode::Idle
                    disabled=move || !can_post() || staged.with(|s| s.iter().any(|f| f.preparing))
                        || (input_text.get().trim().is_empty() && staged.with(|s| s.is_empty()))
                    on:click=move |_| send_message()
                >
                    {move || if editing.get().is_some() { t(lang.get(), "btn_save") } else { t(lang.get(), "btn_send") }}
                </button>
                // One element from press to release: holding records, and the pointer stays
                // captured by it while the bar around it changes.
                <button
                    class="btn btn-primary record-btn"
                    class:is-hidden=move || !(show_mic() || rec_mode.get().capturing())
                    class:held=move || rec_mode.get() == RecMode::Held
                    title=move || t(lang.get(), if rec_mode.get() == RecMode::HandsFree { "rec_stop" } else { "title_record" })
                    on:pointerdown=move |ev: web_sys::PointerEvent| mic_press.down(ev)
                    on:pointermove=move |ev: web_sys::PointerEvent| mic_press.moved(ev)
                    on:pointerup=move |_| mic_press.up(false)
                    on:pointercancel=move |_| mic_press.up(true)
                    on:click=move |_| mic_press.click()
                >
                    {move || if rec_mode.get() == RecMode::HandsFree { "⏹" } else { "🎤" }}
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
                        <div
                            class="dm-thread"
                            node_ref=dm_thread_ref
                            on:scroll=move |_| {
                                if let Some(el) = dm_thread_ref.get_untracked() {
                                    let scroll_top = el.scroll_top();
                                    let scroll_height = el.scroll_height();
                                    let client_height = el.client_height();
                                    dm_at_bottom.set_value(scroll_height - scroll_top - client_height <= 60);
                                }
                            }
                        >
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
                            <h4 class="settings-section">{move || t(lang.get(), "settings_devices")}</h4>
                            <div class="device-pickers">
                                {device_picker(
                                    lang,
                                    "mic-device-select",
                                    "device_mic_label",
                                    "device_mic_n",
                                    Signal::derive(move || device_list.with(|l| selectable_devices(l, MIC_KIND))),
                                    Signal::derive(move || device_choice.with(|d| d.mic.clone())),
                                    move |mic| apply_devices(DeviceChoice { mic, ..device_choice.get_untracked() }),
                                )}
                                {speaker_supported.then(|| {
                                    // Listed speakers, plus one picked in the browser's chooser.
                                    let speakers = Signal::derive(move || {
                                        let mut list = device_list.with(|l| selectable_devices(l, SPEAKER_KIND));
                                        if let Some(picked) = picked_speaker.get() {
                                            if !list.iter().any(|d| d.device_id == picked.device_id) {
                                                list.push(picked);
                                            }
                                        }
                                        list
                                    });
                                    // Firefox lists no speakers until one is picked in its own chooser:
                                    // offer that (always, where it exists), and the list once there is one.
                                    let chooser = media::audio_output_chooser_supported();
                                    let something_to_pick = move || {
                                        !chooser || speakers.with(|l| !l.is_empty()) || device_choice.with(|d| d.speaker.is_some())
                                    };
                                    view! {
                                        {move || something_to_pick().then(|| device_picker(
                                            lang,
                                            "speaker-device-select",
                                            "device_speaker_label",
                                            "device_speaker_n",
                                            speakers,
                                            Signal::derive(move || device_choice.with(|d| d.speaker.clone())),
                                            move |speaker| apply_devices(DeviceChoice { speaker, ..device_choice.get_untracked() }),
                                        ))}
                                        {chooser.then(|| view! {
                                            <button id="speaker-choose-btn" type="button" class="btn btn-secondary btn-sm" on:click=choose_speaker>
                                                {move || t(lang.get(), "btn_choose_speaker")}
                                            </button>
                                        })}
                                    }
                                })}
                                {device_picker(
                                    lang,
                                    "camera-device-select",
                                    "device_camera_label",
                                    "device_camera_n",
                                    Signal::derive(move || device_list.with(|l| selectable_devices(l, CAMERA_KIND))),
                                    Signal::derive(move || device_choice.with(|d| d.camera.clone())),
                                    move |camera| apply_devices(DeviceChoice { camera, ..device_choice.get_untracked() }),
                                )}
                                {move || device_list.with(|l| l.iter().any(|d| d.kind != SPEAKER_KIND && d.label.is_empty())).then(|| view! {
                                    <p class="settings-hint">{move || t(lang.get(), "devices_permission_hint")}</p>
                                })}
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
                            <h4 class="settings-section">{move || t(lang.get(), "settings_video")}</h4>
                            {preset_group(lang, QualitySource::Camera, video_presets, apply_video_presets)}
                            {screen_supported.then(|| preset_group(lang, QualitySource::Screen, video_presets, apply_video_presets))}
                            <h4 class="settings-section">{move || t(lang.get(), "settings_files")}</h4>
                            <div class="file-settings">
                                <label class="device-picker" for="file-connections-select">
                                    <span class="device-picker-label">{move || t(lang.get(), "file_connections_label")}</span>
                                    <select
                                        id="file-connections-select"
                                        class="lobby-input device-select"
                                        prop:value=move || file_connections.get().to_string()
                                        on:change=move |ev| {
                                            if let Ok(cap) = event_target_value(&ev).parse::<usize>() {
                                                set_file_connections.set(cap);
                                                with_session(&|s| s.set_file_connections(cap));
                                            }
                                        }
                                    >
                                        {FILE_CONNECTION_CHOICES.into_iter().map(|n| view! {
                                            <option value=n.to_string() selected=move || file_connections.get() == n>{n}</option>
                                        }).collect_view()}
                                    </select>
                                </label>
                                <p class="settings-hint">{move || t(lang.get(), "file_connections_hint")}</p>
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
                        {move || (!agent_status.with(AgentStatus::is_paired)).then(|| view! {
                            <p class="lobby-hint agent-download">
                                {match host_download_url() {
                                    Some(url) => view! {
                                        <a id="agent-download-link" href=url target="_blank" rel="noopener noreferrer">
                                            {move || t(lang.get(), "agent_download_link")}
                                        </a>
                                        " · "
                                        <span>{move || t(lang.get(), "agent_download_hint")}</span>
                                    }.into_view(),
                                    None => view! { <span>{move || t(lang.get(), "agent_download_ask")}</span> }.into_view(),
                                }}
                            </p>
                        })}
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

            // Quick video quality menu (▾ next to the camera and screen buttons)
            {move || quality_menu.get().map(|menu| quality_menu_view(lang, menu, video_presets, pick_quality))}

            // Images and videos opened from the chat, over everything.
            {media_viewer(media_ctx, download_file)}
            // Files dragged over the room.
            {move || dragging.get().then(|| view! {
                <div class="drop-overlay">
                    <div class="drop-overlay-box">{move || format!("📎 {}", t(lang.get(), "drop_to_attach"))}</div>
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
    Save,
    Cancel,
    Withdraw,
    /// Pull an image, video or voice message into the chat to view it.
    Load,
}

/// A shared file card. Rendered again whenever its status changes (it is part of the key).
/// `live`: this card's current status. A progress update changes it without rebuilding the
/// card (see `set_file_status`); a change of state rebuilds it.
fn file_card(
    lang: ReadSignal<Language>,
    file: FileOfferInfo,
    live: Memo<Option<FileTransferStatus>>,
    caption: String,
    author: String,
    members: ReadSignal<Vec<MemberUi>>,
    on_action: impl Fn(FileAction, String) + Copy + 'static,
) -> impl IntoView {
    let actions = file_actions(lang, &file, live, author, members, on_action, None);
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

/// A file card's status line and buttons, for its status when the card was built. `media`:
/// the card shows the file itself (media cards), so Download saves the copy held here, and
/// images and videos show their progress over the picture instead.
fn file_actions(
    lang: ReadSignal<Language>,
    file: &FileOfferInfo,
    live: Memo<Option<FileTransferStatus>>,
    author: String,
    members: ReadSignal<Vec<MemberUi>>,
    on_action: impl Fn(FileAction, String) + Copy + 'static,
    media: Option<(RwSignal<HeldMedia>, MediaKind)>,
) -> View {
    let id = file.file_id.clone();
    let held = {
        let id = id.clone();
        create_memo(move |_| media.is_some_and(|(store, _)| store.with(|h| h.url(&id).is_some())))
    };
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
    let display_name = move |pubkey: &str| {
        members.with(|m| {
            m.iter()
                .find(|x| x.pubkey == pubkey)
                .map(|x| x.name.clone())
                .unwrap_or_else(|| pubkey_tag(pubkey))
        })
    };
    let reachable = move || members.with(|m| m.iter().any(|x| x.pubkey == author && x.link == LinkUi::Direct));
    match file.status.clone() {
        FileTransferStatus::Sharing { .. } => {
            // Progress updates arrive several times a second: only these parts follow them,
            // so the Withdraw button is never rebuilt under the pointer.
            let sharing = move || match live.get() {
                Some(FileTransferStatus::Sharing { active, waiting, done, active_peers, queued_peers }) => {
                    (active, waiting, done, active_peers, queued_peers)
                }
                _ => Default::default(),
            };
            let active_list = move || {
                let active_peers = sharing().3;
                (!active_peers.is_empty()).then(|| view! {
                    <div class="file-active-transfers">
                        {active_peers.into_iter().map(|p| {
                            let peer_name = display_name(&p.peer);
                            let progress = p.progress;
                            let rate = move || transfer_rate(lang.get(), p.speed_kb, p.connections);
                            view! {
                                <div class="file-peer-transfer">
                                    <div class="file-peer-meta">
                                        <span class="file-peer-name" dir="auto">{peer_name}</span>
                                        <span class="file-peer-stats">{move || format!("{progress}% ({})", rate())}</span>
                                    </div>
                                    <div class="file-progress-bar">
                                        <div class="file-progress-fill" style=format!("width: {progress}%;")></div>
                                    </div>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                })
            };
            let queued_list = move || {
                let queued_peers = sharing().4;
                (!queued_peers.is_empty()).then(|| view! {
                    <div class="file-queued-transfers">
                        {queued_peers.into_iter().map(|q| {
                            let peer_name = display_name(&q.peer);
                            let pos = q.position.to_string();
                            view! {
                                <div class="file-peer-queued">
                                    <span class="file-peer-name" dir="auto">{peer_name}</span>
                                    <span class="file-peer-pos">{move || t_replace_1(lang.get(), "file_queued", "{n}", &pos)}</span>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                })
            };

            // A voice message exists only here: it can be saved.
            let save = matches!(media, Some((_, MediaKind::Voice)))
                .then(|| button(FileAction::Download, "btn btn-sm btn-secondary file-save-btn voice-save-btn", "file_save"));
            view! {
                <div class="file-status-row">
                    <span class="file-status-text">{move || t(lang.get(), "file_shared_room")}</span>
                    {save}
                    {button(FileAction::Withdraw, "btn btn-sm btn-danger file-withdraw-btn", "file_withdraw")}
                </div>
                <div class="file-share-counts">
                    <span class="file-count-active">{move || t_replace_1(lang.get(), "file_sending", "{n}", &sharing().0.to_string())}</span>
                    <span class="file-count-waiting">{move || t_replace_1(lang.get(), "file_waiting_count", "{n}", &sharing().1.to_string())}</span>
                    <span class="file-count-done">{move || t_replace_1(lang.get(), "file_done_count", "{n}", &sharing().2.to_string())}</span>
                </div>
                {active_list}
                {queued_list}
            }
            .into_view()
        }
        FileTransferStatus::Offered => {
            let download = button(FileAction::Download, "btn btn-sm btn-primary file-download-btn", "file_download");
            view! {
                <div class="file-status-row">
                    {move || if reachable() {
                        view! { {download.clone()} }.into_view()
                    } else {
                        view! { <span class="file-status-text cancelled file-unreachable">{move || t(lang.get(), "file_unreachable")}</span> }.into_view()
                    }}
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
        FileTransferStatus::Downloading { .. } => {
            // As for Sharing: the numbers follow the live status, the Cancel button stays put.
            let numbers = move || match live.get() {
                Some(FileTransferStatus::Downloading { progress, speed_kb, connections }) => (progress, speed_kb, connections),
                _ => (0, 0, 1),
            };
            view! {
                <div class="file-progress-container">
                    <div class="file-progress-bar">
                        <div class="file-progress-fill" style=move || format!("width: {}%;", numbers().0)></div>
                    </div>
                    <div class="file-progress-meta">
                        <span class="file-progress-label">
                            {move || {
                                let (progress, speed_kb, connections) = numbers();
                                format!("{}: {}% ({})", t(lang.get(), "file_downloading"), progress, transfer_rate(lang.get(), speed_kb, connections))
                            }}
                        </span>
                        {button(FileAction::Cancel, "btn btn-sm btn-danger file-cancel-btn", "btn_cancel")}
                    </div>
                </div>
            }
            .into_view()
        }
        FileTransferStatus::ReadyToSave { summary, .. } => view! {
            <div class="file-status-row">
                {status_line("file_ready_to_save", "completed")}
                {button(FileAction::Save, "btn btn-sm btn-primary file-save-btn", "file_save")}
            </div>
            <div class="file-summary">{move || download_summary(lang.get(), summary)}</div>
        }
        .into_view(),
        // Viewed media: Download saves the copy held here (or loads it again, if released).
        FileTransferStatus::Completed { summary, withdrawn } if media.is_some() => {
            let download = button(FileAction::Download, "btn btn-sm btn-secondary file-download-btn media-download-btn", "file_download");
            view! {
                <div class="file-status-row">
                    <span class="file-summary">{move || download_summary(lang.get(), summary)}</span>
                    {move || (held.get() || (!withdrawn && reachable())).then(|| download.clone())}
                </div>
            }
            .into_view()
        }
        // The offer still stands (unless withdrawn): the file can be downloaded again.
        FileTransferStatus::Completed { summary, withdrawn } => {
            let again = button(FileAction::Download, "btn btn-sm btn-secondary file-download-again-btn", "file_download_again");
            view! {
                <div class="file-status-row">
                    {status_line("file_download_complete", "completed")}
                    {move || (!withdrawn && reachable()).then(|| again.clone())}
                </div>
                <div class="file-summary">{move || download_summary(lang.get(), summary)}</div>
            }
            .into_view()
        }
        // The offer still stands: the download can be started again.
        status @ (FileTransferStatus::Cancelled | FileTransferStatus::Interrupted) => {
            let key = if status == FileTransferStatus::Cancelled { "file_cancelled" } else { "file_interrupted" };
            let download = button(FileAction::Download, "btn btn-sm btn-primary file-download-btn", "file_download");
            view! {
                <div class="file-status-row">
                    {status_line(key, "cancelled")}
                    {move || reachable().then(|| download.clone())}
                </div>
            }
            .into_view()
        }
        // Viewed before the sender withdrew or left: it can still be saved from here.
        status @ (FileTransferStatus::Withdrawn | FileTransferStatus::SenderLeft) => {
            let key = if status == FileTransferStatus::Withdrawn { "file_withdrawn" } else { "file_sender_left" };
            let download = button(FileAction::Download, "btn btn-sm btn-secondary file-download-btn media-download-btn", "file_download");
            view! {
                <div class="file-status-row">
                    {status_line(key, "cancelled")}
                    {move || held.get().then(|| download.clone())}
                </div>
            }
            .into_view()
        }
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
    session: StoredValue<Option<RoomSession>>,
) -> impl IntoView {
    let video_ref = create_node_ref::<leptos::html::Video>();
    // The ⓘ panel; it polls only while open and stops when it closes or the tile goes.
    let (stats_open, set_stats_open) = create_signal(false);
    let pk_stats = member.pubkey.clone();
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
            <video id=format!("tile-video-{}", member.pubkey) node_ref=video_ref autoplay playsinline muted></video>
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
            // Above the control surface (never inside it), so these never reach the shared screen.
            <div class="tile-actions">
                <button
                    class="tile-stats-btn"
                    class:active=move || stats_open.get()
                    aria-pressed=move || stats_open.get().to_string()
                    title=move || t(lang.get(), "title_tile_stats")
                    on:click=move |_| set_stats_open.update(|open| *open = !*open)
                    on:dblclick=|ev| ev.stop_propagation()
                >
                    "ⓘ"
                </button>
                <button
                    class="tile-fullscreen"
                    title=move || t(lang.get(), "title_fullscreen")
                    on:click=move |_| on_fullscreen(Some(pk_btn.clone()))
                >
                    "⛶"
                </button>
            </div>
            {move || stats_open.get().then(|| {
                let video = video_ref.get_untracked().map(|v| {
                    let el: &web_sys::HtmlVideoElement = &v;
                    el.clone()
                });
                stats::stats_panel(lang, pk_stats.clone(), is_self, session, names, video)
            })}
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

/// The two video sources with their own quality presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualitySource {
    Camera,
    Screen,
}

impl QualitySource {
    /// For element ids (`camera-preset-hd`, `#screen-quality-btn`, …).
    fn key(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Screen => "screen",
        }
    }

    fn title_key(self) -> &'static str {
        match self {
            Self::Camera => "video_camera_quality",
            Self::Screen => "video_screen_quality",
        }
    }

    fn button_title_key(self) -> &'static str {
        match self {
            Self::Camera => "title_camera_quality",
            Self::Screen => "title_screen_quality",
        }
    }

    /// `(id, name key, description key)` of every preset, in ladder order.
    fn choices(self) -> Vec<(&'static str, &'static str, &'static str)> {
        match self {
            Self::Camera => CameraPreset::ALL
                .into_iter()
                .map(|p| {
                    let (name, desc) = camera_preset_keys(p);
                    (p.id(), name, desc)
                })
                .collect(),
            Self::Screen => ScreenPreset::ALL
                .into_iter()
                .map(|p| {
                    let (name, desc) = screen_preset_keys(p);
                    (p.id(), name, desc)
                })
                .collect(),
        }
    }

    /// The id of this source's current preset.
    fn current(self, presets: VideoPresets) -> &'static str {
        match self {
            Self::Camera => presets.camera.id(),
            Self::Screen => presets.screen.id(),
        }
    }

    /// `presets` with this source's preset set to `id` (the other source unchanged).
    fn with(self, presets: VideoPresets, id: &str) -> VideoPresets {
        match self {
            Self::Camera => VideoPresets { camera: CameraPreset::from_id(id).unwrap_or(presets.camera), ..presets },
            Self::Screen => VideoPresets { screen: ScreenPreset::from_id(id).unwrap_or(presets.screen), ..presets },
        }
    }
}

fn camera_preset_keys(preset: CameraPreset) -> (&'static str, &'static str) {
    match preset {
        CameraPreset::Smooth60 => ("preset_camera_smooth60", "preset_camera_smooth60_desc"),
        CameraPreset::Balanced => ("preset_camera_balanced", "preset_camera_balanced_desc"),
        CameraPreset::Hd => ("preset_camera_hd", "preset_camera_hd_desc"),
        CameraPreset::FullHd => ("preset_camera_fullhd", "preset_camera_fullhd_desc"),
        CameraPreset::DataSaver => ("preset_camera_datasaver", "preset_camera_datasaver_desc"),
    }
}

fn screen_preset_keys(preset: ScreenPreset) -> (&'static str, &'static str) {
    match preset {
        ScreenPreset::Fastest => ("preset_screen_fastest", "preset_screen_fastest_desc"),
        ScreenPreset::Smooth => ("preset_screen_smooth", "preset_screen_smooth_desc"),
        ScreenPreset::Balanced => ("preset_screen_balanced", "preset_screen_balanced_desc"),
        ScreenPreset::Sharp => ("preset_screen_sharp", "preset_screen_sharp_desc"),
        ScreenPreset::Text => ("preset_screen_text", "preset_screen_text_desc"),
    }
}

/// Where an open quick menu sits: fixed to the viewport next to its split button, kept
/// inside the screen (phones), flipped above the button when there is more room there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MenuPos {
    left: f64,
    top: Option<f64>,
    bottom: Option<f64>,
    width: f64,
    max_height: f64,
}

/// Viewport edge margin for the quick menus, in CSS pixels.
const MENU_MARGIN: f64 = 8.0;
const MENU_MAX_WIDTH: f64 = 300.0;

impl MenuPos {
    fn next_to(anchor: &web_sys::Element, rtl: bool) -> Self {
        let rect = anchor.get_bounding_client_rect();
        let root = window().and_then(|w| w.document()).and_then(|d| d.document_element());
        let (vw, vh) = root.map_or((1024.0, 768.0), |r| (f64::from(r.client_width()), f64::from(r.client_height())));
        let width = (vw - 2.0 * MENU_MARGIN).min(MENU_MAX_WIDTH);
        // Start-aligned with the button: its left edge, or its right edge in RTL.
        let start = if rtl { rect.right() - width } else { rect.left() };
        let left = start.min(vw - width - MENU_MARGIN).max(MENU_MARGIN);
        let room_below = vh - rect.bottom() - 2.0 * MENU_MARGIN;
        let room_above = rect.top() - 2.0 * MENU_MARGIN;
        if room_below >= 240.0 || room_below >= room_above {
            Self { left, top: Some(rect.bottom() + 4.0), bottom: None, width, max_height: room_below.max(120.0) }
        } else {
            Self { left, top: None, bottom: Some(vh - rect.top() + 4.0), width, max_height: room_above }
        }
    }

    fn style(&self) -> String {
        let vertical = match (self.top, self.bottom) {
            (Some(top), _) => format!("top: {top:.0}px;"),
            (None, Some(bottom)) => format!("bottom: {bottom:.0}px;"),
            (None, None) => String::new(),
        };
        format!(
            "left: {:.0}px; {vertical} width: {:.0}px; max-height: {:.0}px;",
            self.left, self.width, self.max_height
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct QualityMenu {
    source: QualitySource,
    pos: MenuPos,
}

/// The ▾ half of a camera or screen split button: opens that source's quality menu.
fn quality_button(
    lang: ReadSignal<Language>,
    source: QualitySource,
    menu: ReadSignal<Option<QualityMenu>>,
    on_toggle: impl Fn(QualitySource, web_sys::MouseEvent) + Copy + 'static,
) -> impl IntoView {
    let open = move || menu.with(|m| m.is_some_and(|m| m.source == source));
    view! {
        <button
            id=format!("{}-quality-btn", source.key())
            class="btn btn-mute quality-btn"
            class:open=open
            aria-haspopup="menu"
            aria-expanded=move || open().to_string()
            title=move || t(lang.get(), source.button_title_key())
            on:click=move |ev| on_toggle(source, ev)
        >
            "▾"
        </button>
    }
}

/// The open quick menu: one button per preset (name and a one-line description), the
/// current one marked. Works before a source starts (it is used for the next one) and
/// while it runs (applied at once).
fn quality_menu_view(
    lang: ReadSignal<Language>,
    menu: QualityMenu,
    presets: ReadSignal<VideoPresets>,
    on_pick: impl Fn(VideoPresets) + Copy + 'static,
) -> impl IntoView {
    let source = menu.source;
    view! {
        <div
            id=format!("{}-quality-menu", source.key())
            class="quality-menu"
            role="menu"
            data-source=source.key()
            style=menu.pos.style()
        >
            <div class="quality-menu-title">{move || t(lang.get(), source.title_key())}</div>
            {source.choices().into_iter().map(|(id, name_key, desc_key)| {
                let selected = move || presets.with(|p| source.current(*p) == id);
                view! {
                    <button
                        type="button"
                        class="quality-option"
                        class:selected=selected
                        role="menuitemradio"
                        aria-checked=move || selected().to_string()
                        data-preset=id
                        on:click=move |_| on_pick(source.with(presets.get_untracked(), id))
                    >
                        <span class="quality-option-name">{move || t(lang.get(), name_key)}</span>
                        <span class="quality-option-desc">{move || t(lang.get(), desc_key)}</span>
                    </button>
                }
            }).collect_view()}
        </div>
    }
}

/// A radio group in the settings modal: one row per preset, like the audio options.
fn preset_group(
    lang: ReadSignal<Language>,
    source: QualitySource,
    presets: ReadSignal<VideoPresets>,
    on_pick: impl Fn(VideoPresets) + Copy + 'static,
) -> impl IntoView {
    let group = source.key();
    let heading_id = format!("{group}-preset-heading");
    view! {
        <div id=format!("{group}-preset-group") class="preset-group" role="radiogroup" aria-labelledby=heading_id.clone()>
            <h5 id=heading_id class="settings-subsection">{move || t(lang.get(), source.title_key())}</h5>
            <div class="audio-options">
                {source.choices().into_iter().map(|(id, name_key, desc_key)| {
                    let input_id = format!("{group}-preset-{id}");
                    view! {
                        <label class="audio-option preset-option" for=input_id.clone()>
                            <input
                                type="radio"
                                id=input_id
                                name=format!("{group}-preset")
                                value=id
                                prop:checked=move || presets.with(|p| source.current(*p) == id)
                                on:change=move |_| on_pick(source.with(presets.get_untracked(), id))
                            />
                            <span class="preset-text">
                                <span class="preset-name">{move || t(lang.get(), name_key)}</span>
                                <span class="preset-desc">{move || t(lang.get(), desc_key)}</span>
                            </span>
                        </label>
                    }
                }).collect_view()}
            </div>
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

/// One device picker in the settings: "System default" first, then the devices the browser
/// lists (numbered while it hides their names), and the chosen device if it isn't listed
/// (unplugged). Picking "System default" chooses `None`.
fn device_picker(
    lang: ReadSignal<Language>,
    id: &'static str,
    label_key: &'static str,
    numbered_key: &'static str,
    devices: Signal<Vec<DeviceEntry>>,
    chosen: Signal<Option<String>>,
    on_pick: impl Fn(Option<String>) + 'static,
) -> impl IntoView {
    let options = move || {
        let lang = lang.get();
        let chosen = chosen.get();
        let listed = devices.get();
        let mut options = vec![(String::new(), t(lang, "device_default").to_string())];
        for (i, device) in listed.iter().enumerate() {
            let label = if device.label.trim().is_empty() {
                t_replace_1(lang, numbered_key, "{n}", &(i + 1).to_string())
            } else {
                device.label.clone()
            };
            options.push((device.device_id.clone(), label));
        }
        if let Some(missing) = chosen.as_ref().filter(|c| !listed.iter().any(|d| &d.device_id == *c)) {
            options.push((missing.clone(), t(lang, "device_unavailable").to_string()));
        }
        let selected = chosen.unwrap_or_default();
        options
            .into_iter()
            .map(|(value, label)| {
                let is_selected = value == selected;
                view! { <option value=value selected=is_selected>{label}</option> }
            })
            .collect_view()
    };
    view! {
        <label class="device-picker" for=id>
            <span class="device-picker-label">{move || t(lang.get(), label_key)}</span>
            <select
                id=id
                class="lobby-input device-select"
                prop:value=move || chosen.get().unwrap_or_default()
                on:change=move |ev| {
                    let value = event_target_value(&ev);
                    on_pick((!value.is_empty()).then_some(value));
                }
            >
                {options}
            </select>
        </label>
    }
}

/// The settings' choices for the most connections an upload may use.
const FILE_CONNECTION_CHOICES: [usize; 4] = [1, 2, 4, protocol::transfer::MAX_FILE_CONNECTIONS];

/// "146.5 MB in 23.4 s · 6.3 MB/s average": a finished download's size, time and speed.
fn download_summary(lang: Language, summary: DownloadSummary) -> String {
    let seconds = summary.millis / 1000;
    let time = if seconds < 60 {
        format!("{:.1} s", summary.millis as f64 / 1000.0)
    } else {
        format!("{} min {:02} s", seconds / 60, seconds % 60)
    };
    t(lang, "file_download_summary")
        .replace("{size}", &format_file_size(summary.bytes))
        .replace("{time}", &time)
        .replace("{speed}", &transfer_rate(lang, summary.speed_kb(), 1))
}

/// "12.3 MB/s", followed by "· 4 connections" when a transfer uses more than one.
fn transfer_rate(lang: Language, speed_kb: u64, connections: u8) -> String {
    let speed = if speed_kb > 1024 { format!("{:.1} MB/s", speed_kb as f64 / 1024.0) } else { format!("{speed_kb} KB/s") };
    if connections > 1 {
        format!("{speed} · {}", t_replace_1(lang, "file_connections", "{n}", &connections.to_string()))
    } else {
        speed
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
    on_make_admin: impl Fn(String, String) + Copy + 'static,
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
                let (admin_pubkey, admin_name) = kick_target.clone();
                view! {
                    <button
                        class="btn btn-sm btn-secondary make-admin-btn"
                        title=move || t(lang.get(), "title_make_admin")
                        on:click=move |_| on_make_admin(admin_pubkey.clone(), admin_name.clone())
                    >
                        {move || t(lang.get(), "btn_make_admin")}
                    </button>
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

/// Test-only probes for media (`window.__dchatMedia`): the RAM budget, the recording cap,
/// and which files are held. Never in release builds.
#[cfg(feature = "e2e-hooks")]
fn install_media_hooks(held: RwSignal<HeldMedia>, recorder: Recorder) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsValue;
    let Some(win) = window() else {
        return;
    };
    let hooks = js_sys::Object::new();
    let budget = Closure::wrap(Box::new(move |bytes: f64| held.update(|h| h.set_budget(bytes as u64))) as Box<dyn Fn(f64)>);
    let _ = js_sys::Reflect::set(&hooks, &"mediaBudget".into(), budget.as_ref());
    budget.forget();
    let ids = Closure::wrap(Box::new(move || {
        let ids: js_sys::Array = held.with_untracked(|h| h.ids()).into_iter().map(JsValue::from).collect();
        JsValue::from(ids)
    }) as Box<dyn Fn() -> JsValue>);
    let _ = js_sys::Reflect::set(&hooks, &"heldMedia".into(), ids.as_ref());
    ids.forget();
    let cap = Closure::wrap(Box::new(move |ms: f64| recorder.set_cap_ms(ms)) as Box<dyn Fn(f64)>);
    let _ = js_sys::Reflect::set(&hooks, &"recordCapMs".into(), cap.as_ref());
    cap.forget();
    let _ = js_sys::Reflect::set(&win, &"__dchatMedia".into(), &hooks);
}
