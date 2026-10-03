use protocol::{
    format_cap, generate_key, generate_room_id, invite_fragment, key_from_base64, key_to_base64,
    FragmentParams, NostrBurnerKey, VideoKind, DEFAULT_MEMBER_CAP, DEFAULT_VIDEO_CAP,
    DEFAULT_VOICE_CAP, KEY_LENGTH,
};
use wasm_bindgen::JsValue;
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

/// How this tab reaches a member.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkUi {
    Me,
    Direct,
    /// No direct link; text is relayed through the named member.
    Via(String),
    /// A direct link is being negotiated and no relay path exists yet.
    Connecting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberUi {
    pub pubkey: String,
    pub name: String,
    pub tag: String,
    pub is_admin: bool,
    pub link: LinkUi,
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

/// A member currently in the voice lounge, as shown to this tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LoungeMemberUi {
    pub pubkey: String,
    pub name: String,
    pub tag: String,
    pub is_self: bool,
    pub mic_muted: bool,
    pub video: VideoKind,
    /// Media needs a direct link; relayed members are listed but silent.
    pub has_media_link: bool,
}

/// This tab's own lounge controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MyVoiceUi {
    pub in_voice: bool,
    /// Mic capture is in flight after Join Voice.
    pub joining: bool,
    pub mic_muted: bool,
    pub speaker_muted: bool,
    pub video: VideoKind,
}

/// Room settings chosen at creation: caps (`None` = unlimited) and history for late joiners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomCaps {
    pub members: Option<usize>,
    pub voice: Option<usize>,
    pub video: Option<usize>,
    pub history: bool,
}

/// A file card's state, as seen by this tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileTransferStatus {
    /// Someone else's offer we may download.
    Offered,
    Queued { position: usize },
    Downloading { progress: u8, speed_kb: u64 },
    Completed,
    Declined,
    Cancelled,
    Withdrawn,
    SenderLeft,
    /// The direct link dropped mid-transfer.
    Interrupted,
    /// Our own offer: uploads running, waiting, finished.
    Sharing { active: usize, waiting: usize, done: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOfferInfo {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub status: FileTransferStatus,
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

/// System lines shown in the chat timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Joined(String),
    Left(String),
    LateJoin,
    /// History from before we joined was inserted above.
    HistoryShown,
    /// An admin moved the room to a new link and we followed.
    Rekeyed,
}

/// Where an admin moved the room: the new room ID and key, already written to the URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RekeyTarget {
    pub room: String,
    pub key: [u8; KEY_LENGTH],
    /// Rejoin the voice lounge in the new room.
    pub rejoin_voice: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessageUi {
    pub id: String,
    /// Author session pubkey; the display name is looked up live so it can arrive later.
    pub author: String,
    pub is_self: bool,
    pub text: String,
    pub time: String,
    /// Author's send time (ms), for placing history from before we joined.
    pub ts: u64,
    pub notice: Option<Notice>,
    pub file: Option<FileOfferInfo>,
}

/// Parameters of the current URL fragment (empty when unavailable).
pub fn current_fragment() -> FragmentParams {
    window()
        .and_then(|w| w.location().hash().ok())
        .map(|hash| FragmentParams::parse(&hash))
        .unwrap_or_default()
}

/// Rewrite the fragment without adding a history entry (so Back never lands on a
/// half-initialized room).
pub fn replace_fragment(params: &FragmentParams) {
    if let Some(win) = window() {
        if let Ok(history) = win.history() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&params.to_hash()));
        }
    }
}

/// The room ID and 256-bit key from the URL fragment, when both are present and valid.
/// Format: #room=<room_id>&key=<base64_secret_key>[&other=params...]
/// Invariant: URL hash fragments are NEVER sent to the HTTP or WebSocket server.
pub fn read_credentials() -> Option<(String, [u8; KEY_LENGTH])> {
    let params = current_fragment();
    let room = params.get("room")?.to_string();
    let key = key_from_base64(params.get("key")?).ok()?;
    Some((room, key))
}

/// Create a new room in the fragment: fresh room ID, room key and admin keypair, plus each
/// cap that differs from its default. Other parameters (e.g. `relays`) are kept.
/// The admin secret only ever lives in this creator's fragment (the admin link).
pub fn create_room(caps: RoomCaps) -> Result<(), String> {
    let admin = NostrBurnerKey::generate().map_err(|e| e.to_string())?;
    let mut params = current_fragment();
    params.set("room", &generate_room_id());
    params.set("key", &key_to_base64(&generate_key()));
    params.set("adm", admin.pubkey());
    params.set("admsk", &admin.secret_hex());
    for (key, cap, default) in [
        ("max", caps.members, DEFAULT_MEMBER_CAP),
        ("maxa", caps.voice, DEFAULT_VOICE_CAP),
        ("maxv", caps.video, DEFAULT_VIDEO_CAP),
    ] {
        if cap == Some(default) {
            params.remove(key);
        } else {
            params.set(key, &format_cap(cap));
        }
    }
    if caps.history {
        params.set("hist", "1");
    } else {
        params.remove("hist");
    }
    replace_fragment(&params);
    Ok(())
}

fn page_base_url() -> String {
    window()
        .and_then(|w| {
            let loc = w.location();
            Some(format!("{}{}", loc.origin().ok()?, loc.pathname().ok()?))
        })
        .unwrap_or_default()
}

/// The link to share with members: the current fragment without the admin secret.
pub fn invite_url() -> String {
    format!("{}{}", page_base_url(), invite_fragment(&current_fragment()).to_hash())
}

/// The full admin link, when this tab holds the admin secret.
pub fn admin_url() -> Option<String> {
    let params = current_fragment();
    params.get("admsk")?;
    Some(format!("{}{}", page_base_url(), params.to_hash()))
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
