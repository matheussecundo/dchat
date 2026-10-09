use protocol::{
    format_cap, generate_key, generate_password_salt, generate_room_id, invite_fragment, is_relay_url, key_from_base64,
    key_to_base64, parse_relay_list, FragmentParams, NostrBurnerKey, VideoKind, DEFAULT_MEMBER_CAP,
    DEFAULT_VIDEO_CAP, DEFAULT_VOICE_CAP, KEY_LENGTH, PUBLIC_RELAYS, PUBLIC_RELAYS_KEYWORD,
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

/// Which microphone, speaker and camera to use, by `deviceId` (`None`: the system default).
/// Held in RAM only, like `AudioSettings`: it resets on reload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceChoice {
    pub mic: Option<String>,
    pub speaker: Option<String>,
    pub camera: Option<String>,
}

/// One entry of `navigator.mediaDevices.enumerateDevices()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceEntry {
    /// `audioinput`, `audiooutput` or `videoinput`.
    pub kind: String,
    pub device_id: String,
    /// Empty until the page may use a device (microphone or camera permission).
    pub label: String,
    pub group_id: String,
}

pub const MIC_KIND: &str = "audioinput";
pub const SPEAKER_KIND: &str = "audiooutput";
pub const CAMERA_KIND: &str = "videoinput";

/// The devices of `kind` a picker can offer, in the browser's order: real ids only (before
/// permission some browsers hide them as ""), each once, and without Chrome's `default` and
/// `communications` aliases (the picker's own "System default" stands for those).
pub fn selectable_devices(devices: &[DeviceEntry], kind: &str) -> Vec<DeviceEntry> {
    let mut seen = std::collections::HashSet::new();
    devices
        .iter()
        .filter(|d| d.kind == kind)
        .filter(|d| !d.device_id.is_empty() && d.device_id != "default" && d.device_id != "communications")
        .filter(|d| seen.insert(d.device_id.clone()))
        .cloned()
        .collect()
}

/// The camera after `current` in `cameras`, wrapping around (the first one when `current`
/// isn't listed). `None` when there is no other camera to move to.
pub fn next_device(cameras: &[DeviceEntry], current: Option<&str>) -> Option<String> {
    let position = current.and_then(|c| cameras.iter().position(|d| d.device_id == c));
    let next = match position {
        Some(i) => cameras.get((i + 1) % cameras.len())?,
        None => cameras.first()?,
    };
    (Some(next.device_id.as_str()) != current).then(|| next.device_id.clone())
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

/// Remote control as the UI shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlUi {
    /// Members whose shared screen accepts control requests, and who holds what there.
    pub offers: std::collections::HashMap<String, ControlOfferUi>,
    /// What this tab may do (or asked for) on each sharer's computer.
    pub mine: std::collections::HashMap<String, MyControlUi>,
    /// As the sharer: requests waiting for an answer, oldest first.
    pub prompts: Vec<ControlPromptUi>,
    /// As the sharer: others can ask to control this computer right now.
    pub hosting: bool,
    pub host_mouse_keyboard: Option<String>,
    pub host_pads: Vec<Option<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlOfferUi {
    /// Virtual controllers the sharer's computer can take (0 = none).
    pub controllers: u8,
    pub mouse_keyboard: Option<String>,
    pub pads: Vec<Option<String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MyControlUi {
    pub requested: bool,
    pub mouse_keyboard: bool,
    pub pad: Option<u8>,
    pub mode: protocol::PointerMode,
}

impl MyControlUi {
    pub fn granted(&self) -> bool {
        self.mouse_keyboard || self.pad.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControlPromptUi {
    pub member: String,
    pub mouse_keyboard: bool,
    pub controller: bool,
    /// Who currently holds mouse and keyboard, if granting moves it.
    pub takes_over: Option<String>,
}

/// Room settings chosen at creation: caps (`None` = unlimited), history for late joiners and
/// the `&relays=` value (`None` = the public relays).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomCaps {
    pub members: Option<usize>,
    pub voice: Option<usize>,
    pub video: Option<usize>,
    pub history: bool,
    /// Connect only through TURN, hiding members' IP addresses from each other.
    pub hide_ip: bool,
    /// The room key also needs a password: the link gets a fresh salt (`pw`).
    pub password: bool,
    pub relays: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeerDownloadProgress {
    pub peer: String,
    pub progress: u8,
    pub speed_kb: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeerQueuedInfo {
    pub peer: String,
    pub position: usize,
}

/// A file card's state, as seen by this tab.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileTransferStatus {
    /// Someone else's offer we may download.
    Offered,
    Queued { position: usize },
    Downloading { progress: u8, speed_kb: u64 },
    /// Received in full and waiting in RAM for a tap on Save (iOS: the share sheet needs
    /// one, and handing the file over unasked would take the person out of dchat).
    ReadyToSave,
    Completed,
    Declined,
    Cancelled,
    Withdrawn,
    SenderLeft,
    /// The direct link dropped mid-transfer.
    Interrupted,
    /// Our own offer: uploads running, waiting, finished.
    Sharing {
        active: usize,
        waiting: usize,
        done: usize,
        active_peers: Vec<PeerDownloadProgress>,
        queued_peers: Vec<PeerQueuedInfo>,
    },
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    /// `(emoji, reactor pubkeys)` in display order.
    pub reactions: Vec<(String, Vec<String>)>,
    pub edited: bool,
    pub mentions_me: bool,
    /// Bumped on every in-place change, so the row re-renders.
    pub rev: u32,
}

/// One line of a private conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmUi {
    pub id: String,
    pub from_me: bool,
    pub text: String,
    pub time: String,
    /// "The other member left" marker (text holds their name).
    pub notice: bool,
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

/// Open the room of a pasted or launched link (`FragmentParams::from_link`) in this window:
/// the fragment is rewritten in place and the page reloads, so the key never travels in a
/// request. Reloading leaves any current room, as a reload always does.
pub fn join_link(params: &FragmentParams) {
    replace_fragment(params);
    if let Some(win) = window() {
        let _ = win.location().reload();
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
    if caps.password {
        params.set("pw", &generate_password_salt());
    } else {
        params.remove("pw");
    }
    for (key, on) in [("hist", caps.history), ("hideip", caps.hide_ip)] {
        if on {
            params.set(key, "1");
        } else {
            params.remove(key);
        }
    }
    match &caps.relays {
        Some(relays) => params.set("relays", relays),
        None => params.remove("relays"),
    }
    replace_fragment(&params);
    Ok(())
}

/// Where people get the dchat-host app: `DCHAT_HOST_DOWNLOAD_URL` at build time, otherwise
/// the Releases page of the GitHub repository that built this site (GitHub Actions sets
/// `GITHUB_REPOSITORY`), otherwise unknown.
pub fn host_download_url() -> Option<String> {
    let configured = option_env!("DCHAT_HOST_DOWNLOAD_URL").map(str::trim).filter(|u| !u.is_empty()).map(str::to_string);
    let releases = option_env!("GITHUB_REPOSITORY")
        .filter(|repo| !repo.is_empty())
        .map(|repo| format!("https://github.com/{repo}/releases/latest"));
    configured.or(releases).filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// The app's own address without the fragment, e.g. `https://user.github.io/dchat/`.
/// Use this instead of `/`, which is the root of the whole site when the app is hosted
/// under a path.
pub fn page_base_url() -> String {
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

/// The relays this room uses. With `&relays=` in the link: exactly those (`nostr` stands for
/// the public relays). Without it: the public relays; on a local or LAN dev server, its own
/// relay at `/nostr` instead (localhost) or as well (LAN).
pub fn get_default_relays() -> Vec<String> {
    if let Some(chosen) = current_fragment()
        .get("relays")
        .map(parse_relay_list)
        .filter(|list| !list.is_empty())
    {
        return chosen;
    }
    let public = || PUBLIC_RELAYS.iter().map(|r| r.to_string()).collect::<Vec<_>>();
    let Some(location) = window().map(|w| w.location()) else {
        return public();
    };
    let hostname = location.hostname().unwrap_or_default();
    let is_local = hostname == "localhost" || hostname == "127.0.0.1";
    let is_lan = hostname.starts_with("192.168.")
        || hostname.starts_with("10.")
        || hostname.ends_with(".local")
        || (hostname.starts_with("172.")
            && hostname
                .split('.')
                .nth(1)
                .and_then(|p| p.parse::<u8>().ok())
                .is_some_and(|b| (16..=31).contains(&b)));
    if !(is_local || is_lan) {
        return public();
    }
    let ws_proto = if location.protocol().as_deref() == Ok("https:") { "wss:" } else { "ws:" };
    let local_relay = format!("{}//{}/nostr", ws_proto, location.host().unwrap_or_default());
    if is_local {
        // The isolated dev / test setup: no outside traffic.
        vec![local_relay]
    } else {
        std::iter::once(local_relay).chain(public()).collect()
    }
}

/// How the creator picks the room's relays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayMode {
    /// The public Nostr relays (no `&relays=`).
    Public,
    /// Only the creator's relay(s).
    Custom,
    /// The creator's relay(s), with the public relays as backup.
    CustomWithPublic,
}

/// The relay choice already in the link (e.g. a `#relays=…` opened before creating a room):
/// the mode and the custom relay addresses, for pre-filling the create form.
pub fn fragment_relay_choice() -> Option<(RelayMode, String)> {
    let value = current_fragment().get("relays")?.to_string();
    let entries: Vec<&str> = value.split(',').map(str::trim).filter(|e| !e.is_empty()).collect();
    let with_public = entries.iter().any(|e| e.eq_ignore_ascii_case(PUBLIC_RELAYS_KEYWORD));
    let custom: Vec<&str> = entries.into_iter().filter(|e| is_relay_url(e)).collect();
    if custom.is_empty() {
        return None;
    }
    let mode = if with_public { RelayMode::CustomWithPublic } else { RelayMode::Custom };
    Some((mode, custom.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(kind: &str, id: &str, label: &str) -> DeviceEntry {
        DeviceEntry { kind: kind.into(), device_id: id.into(), label: label.into(), group_id: String::new() }
    }

    #[test]
    fn pickers_offer_real_devices_of_one_kind_without_aliases() {
        let listed = [
            device(MIC_KIND, "default", "Default - Headset"),
            device(MIC_KIND, "communications", "Communications - Headset"),
            device(MIC_KIND, "a1", "Headset"),
            device(MIC_KIND, "a2", ""),
            device(MIC_KIND, "a1", "Headset"),
            device(CAMERA_KIND, "v1", "Webcam"),
            device(SPEAKER_KIND, "o1", "Speakers"),
        ];
        let ids = |kind| selectable_devices(&listed, kind).into_iter().map(|d| d.device_id).collect::<Vec<_>>();
        assert_eq!(ids(MIC_KIND), ["a1", "a2"]);
        assert_eq!(ids(CAMERA_KIND), ["v1"]);
        assert_eq!(ids(SPEAKER_KIND), ["o1"]);
        // Before permission some browsers list devices without ids: nothing to pick.
        assert!(selectable_devices(&[device(MIC_KIND, "", "")], MIC_KIND).is_empty());
    }

    #[test]
    fn next_camera_wraps_and_needs_another_camera() {
        let cameras = [device(CAMERA_KIND, "v1", "Front"), device(CAMERA_KIND, "v2", "Back"), device(CAMERA_KIND, "v3", "USB")];
        assert_eq!(next_device(&cameras, Some("v1")).as_deref(), Some("v2"));
        assert_eq!(next_device(&cameras, Some("v3")).as_deref(), Some("v1"));
        // A camera that is gone (or none chosen): start from the first one.
        assert_eq!(next_device(&cameras, Some("unplugged")).as_deref(), Some("v1"));
        assert_eq!(next_device(&cameras, None).as_deref(), Some("v1"));
        assert_eq!(next_device(&cameras[..1], Some("v1")), None);
        assert_eq!(next_device(&[], Some("v1")), None);
    }
}
