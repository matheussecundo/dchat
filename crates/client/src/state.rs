use protocol::{
    generate_key, generate_room_id, key_from_base64, key_to_base64, FragmentParams, KEY_LENGTH,
};
use serde::{Deserialize, Serialize};
use web_sys::window;

use crate::i18n::{t, Language};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Idle,
    ConnectingRelay,
    WaitingForPeer,
    NegotiatingWebRtc,
    Connected,
    Disconnected,
    Error(String),
}

impl ConnectionStatus {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        self.label_i18n(Language::En)
    }

    pub fn label_i18n(&self, lang: Language) -> &'static str {
        match self {
            ConnectionStatus::Idle => t(lang, "status_idle"),
            ConnectionStatus::ConnectingRelay => t(lang, "status_connecting"),
            ConnectionStatus::WaitingForPeer => t(lang, "status_waiting"),
            ConnectionStatus::NegotiatingWebRtc => t(lang, "status_negotiating"),
            ConnectionStatus::Connected => t(lang, "status_connected"),
            ConnectionStatus::Disconnected => t(lang, "status_disconnected"),
            ConnectionStatus::Error(_) => t(lang, "status_error"),
        }
    }

    pub fn color_class(&self) -> &'static str {
        match self {
            ConnectionStatus::Idle => "status-idle",
            ConnectionStatus::ConnectingRelay => "status-connecting",
            ConnectionStatus::WaitingForPeer => "status-waiting",
            ConnectionStatus::NegotiatingWebRtc => "status-negotiating",
            ConnectionStatus::Connected => "status-connected",
            ConnectionStatus::Disconnected => "status-disconnected",
            ConnectionStatus::Error(_) => "status-error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallType {
    None,
    Audio,
    Video,
    ScreenShare,
}

impl CallType {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        self.label_i18n(Language::En)
    }

    pub fn label_i18n(&self, lang: Language) -> &'static str {
        match self {
            CallType::None => t(lang, "call_type_none"),
            CallType::Audio => t(lang, "call_type_audio"),
            CallType::Video => t(lang, "call_type_video"),
            CallType::ScreenShare => t(lang, "call_type_screen"),
        }
    }
}

/// Browser-native microphone processing requested via getUserMedia constraints.
/// Held in RAM only; resets to all-on on reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSettings {
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    pub auto_gain_control: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            noise_suppression: true,
            echo_cancellation: true,
            auto_gain_control: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallState {
    Idle,
    Calling(CallType),
    Incoming(CallType),
    Active(CallType),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileTransferStatus {
    Offered,
    Downloading { progress: u8, speed_kb: u64 },
    Completed,
    Cancelled { reason: String },
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOfferInfo {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub status: FileTransferStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessageUi {
    pub id: String,
    pub sender: String,
    pub is_self: bool,
    pub text: String,
    pub time: String,
    pub file: Option<FileOfferInfo>,
}

pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Parameters of the current URL fragment (empty when unavailable).
pub fn current_fragment() -> FragmentParams {
    window()
        .and_then(|w| w.location().hash().ok())
        .map(|hash| FragmentParams::parse(&hash))
        .unwrap_or_default()
}

/// Parse or initialize the ephemeral room ID and 256-bit secret key from the URL hash.
/// Format: #room=<room_id>&key=<base64_secret_key>[&other=params...]
/// Every other fragment parameter (e.g. `relays`) is preserved when the hash is rewritten.
/// Invariant: URL hash fragments are NEVER sent to the HTTP or WebSocket server.
pub fn get_or_init_credentials() -> Option<(String, [u8; KEY_LENGTH], String)> {
    let location = window()?.location();
    let hash = location.hash().ok()?;
    let mut params = FragmentParams::parse(&hash);

    // A key without a room is not trusted: a fresh room always gets a fresh key.
    let existing_room = params.get("room").map(str::to_string);
    let existing_key = existing_room
        .as_ref()
        .and(params.get("key"))
        .and_then(|k| key_from_base64(k).ok().map(|bytes| (bytes, k.to_string())));
    let room = existing_room.unwrap_or_else(generate_room_id);
    let (key_bytes, b64_str) = existing_key.unwrap_or_else(|| {
        let k = generate_key();
        (k, key_to_base64(&k))
    });

    params.set("room", &room);
    params.set("key", &b64_str);

    // Update URL hash without page reload if it changed
    let target_hash = params.to_hash();
    if hash != target_hash {
        let _ = location.set_hash(&target_hash);
    }

    Some((room, key_bytes, b64_str))
}

pub fn get_full_share_url() -> String {
    if let Some(win) = window() {
        if let Ok(href) = win.location().href() {
            return href;
        }
    }
    String::new()
}

pub fn current_time_string() -> String {
    let date = js_sys::Date::new_0();
    let hours = date.get_hours();
    let minutes = date.get_minutes();
    let seconds = date.get_seconds();
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

/// Retrieve the list of Nostr relays to connect to.
/// Resolves custom relays from the URL fragment (&relays=...),
/// auto-detects localhost mock relay in dev/test mode,
/// and defaults to curated reliable public relays.
pub fn get_default_relays() -> Vec<String> {
    let mut relays = Vec::new();

    if let Some(val) = current_fragment().get("relays") {
        relays.extend(
            val.split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_string),
        );
    }

    if let Some(win) = window() {
        if let Ok(hostname) = win.location().hostname() {
            let is_local = hostname == "localhost" || hostname == "127.0.0.1";
            let is_lan = hostname.starts_with("192.168.")
                || hostname.starts_with("10.")
                || hostname.ends_with(".local")
                || (hostname.starts_with("172.") && {
                    let parts: Vec<&str> = hostname.split('.').collect();
                    parts.get(1).and_then(|p| p.parse::<u8>().ok()).map_or(false, |b| (16..=31).contains(&b))
                });

            if is_local || is_lan {
                if let (Ok(protocol), Ok(host)) = (win.location().protocol(), win.location().host()) {
                    let ws_proto = if protocol == "https:" { "wss:" } else { "ws:" };
                    let local_relay = format!("{}//{}/nostr", ws_proto, host);
                    if !relays.contains(&local_relay) {
                        relays.push(local_relay);
                    }
                }
            }

            // If on LAN or a public domain (not isolated localhost test runner),
            // also ensure public Nostr relays are available
            if !is_local && relays.len() <= 1 {
                for r in &["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"] {
                    let r_str = r.to_string();
                    if !relays.contains(&r_str) {
                        relays.push(r_str);
                    }
                }
            }
        }
    }

    if relays.is_empty() {
        relays.push("wss://relay.damus.io".into());
        relays.push("wss://nos.lol".into());
        relays.push("wss://relay.primal.net".into());
    }

    relays
}
