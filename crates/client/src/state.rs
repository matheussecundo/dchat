use protocol::{generate_key, generate_room_id, key_from_base64, key_to_base64, KEY_LENGTH};
use serde::{Deserialize, Serialize};
use web_sys::window;

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
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionStatus::Idle => "Idle",
            ConnectionStatus::ConnectingRelay => "Connecting to Relay...",
            ConnectionStatus::WaitingForPeer => "Waiting for Peer to Join...",
            ConnectionStatus::NegotiatingWebRtc => "Establishing P2P DTLS...",
            ConnectionStatus::Connected => "Connected (E2EE P2P Active)",
            ConnectionStatus::Disconnected => "Peer Disconnected",
            ConnectionStatus::Error(_) => "Connection Error",
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
    pub fn label(&self) -> &'static str {
        match self {
            CallType::None => "None",
            CallType::Audio => "Audio",
            CallType::Video => "Video",
            CallType::ScreenShare => "Screen Share",
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
pub struct ChatMessageUi {
    pub id: String,
    pub sender: String,
    pub is_self: bool,
    pub text: String,
    pub time: String,
}

/// Parse or initialize the ephemeral room ID and 256-bit secret key from the URL hash.
/// Format: #room=<room_id>&key=<base64_secret_key>
/// Invariant: URL hash fragments are NEVER sent to the HTTP or WebSocket server.
pub fn get_or_init_credentials() -> Option<(String, [u8; KEY_LENGTH], String)> {
    let win = window()?;
    let location = win.location();
    let hash = location.hash().ok()?;

    let mut room_id: Option<String> = None;
    let mut key_b64: Option<String> = None;

    if hash.starts_with('#') {
        let query = &hash[1..];
        for pair in query.split('&') {
            let mut parts = pair.split('=');
            match (parts.next(), parts.next()) {
                (Some("room"), Some(val)) if !val.is_empty() => {
                    room_id = Some(val.to_string());
                }
                (Some("key"), Some(val)) if !val.is_empty() => {
                    key_b64 = Some(val.to_string());
                }
                _ => {}
            }
        }
    }

    let (room, key_bytes, b64_str) = match (room_id, key_b64) {
        (Some(r), Some(k_str)) => {
            if let Ok(k) = key_from_base64(&k_str) {
                (r, k, k_str)
            } else {
                let k = generate_key();
                let b64 = key_to_base64(&k);
                (r, k, b64)
            }
        }
        (Some(r), None) => {
            let k = generate_key();
            let b64 = key_to_base64(&k);
            (r, k, b64)
        }
        (None, _) => {
            let r = generate_room_id();
            let k = generate_key();
            let b64 = key_to_base64(&k);
            (r, k, b64)
        }
    };

    // Update URL hash without page reload if it changed
    let target_hash = format!("#room={}&key={}", room, b64_str);
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
