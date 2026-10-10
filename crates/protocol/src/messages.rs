use crate::crypto::EncryptedPayload;
use crate::crypto::{decrypt_json, encrypt_json};
use crate::nostr::{verify_message, NostrBurnerKey, NostrError};
use serde::{Deserialize, Serialize};

/// Messages sent from the WebAssembly client to the Axum WebSocket signaling server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    Join {
        room_id: String,
    },
    Signal {
        room_id: String,
        payload: EncryptedPayload,
    },
    Leave {
        room_id: String,
    },
    Ping,
}

/// Messages sent from the Axum WebSocket signaling server to the WebAssembly client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    Joined {
        room_id: String,
        peer_count: usize,
        is_initiator: bool,
    },
    Signal {
        payload: EncryptedPayload,
    },
    PeerJoined,
    PeerLeft,
    Pong,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IceCandidateData {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_m_line_index: Option<u16>,
}

/// A WebRTC signal between members. Through the relays it travels as a `RelaySignal`;
/// over an open link, inside a `RoomBody::LinkSignal`.
///
/// Every room member subscribes to the same topic, so SDP and ICE carry the
/// recipient's session pubkey in `to`; everyone else ignores them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "content")]
pub enum SignalPayload {
    /// "I am in the room": prompts every member without a link to us to connect.
    Presence,
    Offer {
        to: String,
        sdp: String,
    },
    Answer {
        to: String,
        sdp: String,
    },
    IceBatch {
        to: String,
        candidates: Vec<IceCandidateData>,
    },
    PeerLeft,
}

impl SignalPayload {
    /// Recipient of an addressed signal; `None` for room-wide broadcasts.
    pub fn recipient(&self) -> Option<&str> {
        match self {
            SignalPayload::Offer { to, .. }
            | SignalPayload::Answer { to, .. }
            | SignalPayload::IceBatch { to, .. } => Some(to),
            SignalPayload::Presence | SignalPayload::PeerLeft => None,
        }
    }
}

/// A signal as the relays carry it, inside the room-key encryption. Offers, answers and
/// ICE (SDP and IP addresses) are also sealed to the recipient's session key (ECDH), so
/// the room key alone does not open them: not for other members, and not for anyone who
/// gets the link later and kept relay traffic. Session keys live only in the tab's RAM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "content")]
pub enum RelaySignal {
    Presence,
    PeerLeft,
    Sealed { to: String, sealed: EncryptedPayload },
}

impl RelaySignal {
    /// Wrap `signal` from `sender` for the relays, sealing it when it is addressed.
    pub fn seal(sender: &NostrBurnerKey, signal: &SignalPayload) -> Result<Self, NostrError> {
        Ok(match signal {
            SignalPayload::Presence => Self::Presence,
            SignalPayload::PeerLeft => Self::PeerLeft,
            SignalPayload::Offer { to, .. } | SignalPayload::Answer { to, .. } | SignalPayload::IceBatch { to, .. } => {
                Self::Sealed {
                    to: to.clone(),
                    sealed: seal_json(sender, to, signal)?,
                }
            }
        })
    }

    pub fn recipient(&self) -> Option<&str> {
        match self {
            Self::Sealed { to, .. } => Some(to),
            Self::Presence | Self::PeerLeft => None,
        }
    }

    /// The signal from the member `sender`, when it is for `recipient` and opens with
    /// their two session keys; `None` otherwise.
    pub fn open(&self, recipient: &NostrBurnerKey, sender: &str) -> Option<SignalPayload> {
        match self {
            Self::Presence => Some(SignalPayload::Presence),
            Self::PeerLeft => Some(SignalPayload::PeerLeft),
            Self::Sealed { to, sealed } => {
                if to != recipient.pubkey() {
                    return None;
                }
                let signal: SignalPayload = open_json(recipient, sender, sealed)?;
                // Only addressed signals are sealed, and only to whoever they address.
                (signal.recipient() == Some(recipient.pubkey())).then_some(signal)
            }
        }
    }
}

/// What a member sends through the relays: its room identity (session pubkey), proof that
/// the key signing the relay event speaks for it, and the signal. Relay events are signed by
/// a key that is fresh every session, while the identity survives a rekey; so relays can't
/// tie a room to the one it was rotated from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayFrame {
    pub from: String,
    /// `from`'s signature over `relay_key_message(topic, relay event pubkey)`.
    pub cert: String,
    pub signal: RelaySignal,
}

/// Bytes a member's identity signs to vouch for the key that signs its relay events on
/// `topic`. Binding the relay key stops anyone from re-sending the frame under their own.
pub fn relay_key_message(topic: &str, relay_pubkey: &str) -> Vec<u8> {
    format!("dchat:relay-key:{topic}:{relay_pubkey}").into_bytes()
}

/// Why remote control ended or was not granted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ControlEnd {
    Denied,
    Revoked,
    TakenOver,
    ShareEnded,
    AgentLost,
    Expired,
    LeftVoice,
    NoSlot,
}

/// What a member is sending as video in the voice lounge (one source at a time).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum VideoKind {
    #[default]
    None,
    Camera,
    Screen,
}

/// A time window of a chat log: its start (ms since the epoch, aligned to its span), how
/// many entries it holds and the XOR of their fingerprints.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncWindow {
    pub start: u64,
    pub count: u32,
    pub hash: u64,
}

/// Content of a room message. Sent inside a `RoomEnvelope` over the per-pair
/// "chat" RTCDataChannel, encrypted with the room key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum RoomBody {
    /// Member announcement: display name, join time (ms) for cap ordering, and
    /// an optional admin-key signature over `admin_proof_message`.
    Hello {
        name: String,
        join_ts: u64,
        admin_proof: Option<String>,
    },
    /// The author's current direct WebRTC links; `seq` increases with each update.
    LinkState { seq: u64, direct: Vec<String> },
    Chat { text: String },
    /// The author's voice-lounge state. `voice_ts` / `video_ts` are when they joined
    /// voice / turned video on, ordering the voice and video caps like `join_ts` does
    /// for the member cap. `seq` increases with each update.
    VoiceState {
        seq: u64,
        in_voice: bool,
        voice_ts: u64,
        mic_muted: bool,
        video: VideoKind,
        video_ts: u64,
    },
    /// A file the author shares with the room. Each member who wants it pulls it from
    /// the author over their direct link; files are never relayed.
    FileOffer {
        file_id: String,
        name: String,
        size: u64,
        mime_type: String,
        caption: Option<String>,
    },
    /// Ask the author (`to`) to send their file over our direct link.
    FileRequest { to: String, file_id: String },
    /// The author withdraws the offer for everyone (`to: None`), or one side stops a
    /// single transfer (`to: Some(peer)`).
    FileCancel { to: Option<String>, file_id: String },
    /// The author tells a requester its 1-based place in the upload queue.
    FileQueued { to: String, file_id: String, position: usize },
    /// Offer, answer or ICE for extra file link `link` between the author and `to` (a second
    /// `RTCPeerConnection` that only carries file chunks), sent over their open main link.
    /// `from_dialer`: the author opened that link (otherwise `to` did); each side numbers
    /// the links it opens.
    FileLinkSignal { to: String, link: u32, from_dialer: bool, signal: SignalPayload },
    /// History sync, first message of a round: the sender's non-empty day windows of its
    /// chat log (`protocol::chat_log`). `horizon`: the time below which its log keeps nothing.
    SyncSummary { to: String, horizon: Option<u64>, windows: Vec<SyncWindow> },
    /// Describe these windows of `level` (0 day, 1 hour, 2 minute): they differ from ours.
    SyncAsk { to: String, horizon: Option<u64>, level: u8, starts: Vec<u64> },
    /// Answer to a summary or an ask: the ids held in small differing windows, and the
    /// non-empty `level` windows inside large ones. `last` ends the answer.
    SyncDiff { to: String, ids: Vec<String>, level: u8, windows: Vec<SyncWindow>, last: bool },
    /// Send these entries of your log.
    SyncWant { to: String, ids: Vec<String> },
    /// Signed originals from the sender's log (authors' Hellos first). `last` ends the
    /// answer to one `SyncWant`.
    SyncBatch { to: String, envelopes: Vec<RoomEnvelope>, last: bool },
    /// The author is typing (sent at most every few seconds; expires on its own).
    Typing,
    /// Add (`on`) or remove the author's `emoji` reaction to message `target`.
    Reaction { target: String, emoji: String, on: bool },
    /// Replace the text of the author's own message `target`.
    Edit { target: String, text: String },
    /// Remove the author's own message `target` (best effort on honest clients).
    Delete { target: String },
    /// A private message sealed with the author's and the recipient's session keys (ECDH).
    /// It does not name the recipient: members relaying it can't tell who it is for. Each
    /// member tries to open it; the one who can keeps it and relays it no further.
    Dm { sealed: EncryptedPayload },
    /// WebRTC renegotiation (offer, answer, ICE) for the link between author and `to`, sent
    /// over that link itself once it is open, so Nostr relays only carry the first handshake.
    LinkSignal { to: String, signal: SignalPayload },
    /// The author's remote-control state while they share their screen: whether they accept
    /// requests (host app paired, whole screen shared), how many controllers its computer
    /// can take, and who holds what. Gossiped, so everyone sees who controls the shared
    /// computer; `seq` increases with each update.
    ControlStatus { seq: u64, available: bool, controllers: u8, mouse_keyboard: Option<String>, pads: Vec<Option<String>> },
    /// Ask the sharer `to` for mouse and keyboard and/or a controller.
    ControlRequest { to: String, mouse_keyboard: bool, controller: bool },
    /// From the sharer: everything `to` may control now (all false = denied or ended, with why).
    ControlGrant { to: String, mouse_keyboard: bool, pad: Option<u8>, reason: Option<ControlEnd> },
    /// The viewer gives back everything it holds on `to`'s computer and drops its request.
    ControlRelease { to: String },
    /// Admin only: move the room to a new ID and key. Each remaining member gets its own
    /// grant sealed to its session key; `kicked` (if any) gets none.
    AdminRekey { kicked: Option<String>, grants: Vec<SealedGrant> },
    /// Admin only: the admin secret sealed to one member (`HandoverContent`, ECDH like a DM).
    /// It names no recipient: every member tries to open it. `promote`: the recipient becomes
    /// an admin now (Make admin); otherwise it is the heir, holding the secret until no admin
    /// is left (`protocol::succession`). Everyone relays it, so an older heir sees that
    /// someone else is the heir now and drops its copy.
    AdminHandover { promote: bool, sealed: EncryptedPayload },
    Leave,
}

impl RoomBody {
    /// The single member this message is for; such messages travel only over the direct
    /// link between the two and are never gossip-relayed.
    pub fn recipient(&self) -> Option<&str> {
        match self {
            RoomBody::FileRequest { to, .. }
            | RoomBody::FileQueued { to, .. }
            | RoomBody::FileLinkSignal { to, .. }
            | RoomBody::SyncSummary { to, .. }
            | RoomBody::SyncAsk { to, .. }
            | RoomBody::SyncDiff { to, .. }
            | RoomBody::SyncWant { to, .. }
            | RoomBody::SyncBatch { to, .. }
            | RoomBody::LinkSignal { to, .. }
            | RoomBody::ControlRequest { to, .. }
            | RoomBody::ControlGrant { to, .. }
            | RoomBody::ControlRelease { to } => Some(to),
            RoomBody::FileCancel { to, .. } => to.as_deref(),
            _ => None,
        }
    }
}

/// A room message signed by its author's session key, so it can be gossip-relayed
/// through other members without any of them being able to forge or alter it. The
/// signature also covers the room's admin key (`adm`, unchanged by rekeys), so a member of
/// two rooms can't replay one room's messages, or names, into the other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomEnvelope {
    pub id: String,
    pub author: String,
    pub ts: u64,
    pub body: RoomBody,
    pub sig: String,
}

impl RoomEnvelope {
    /// `adm`: the room's admin pubkey (`""` for a link without one).
    pub fn sign(key: &NostrBurnerKey, adm: &str, ts: u64, body: RoomBody) -> Result<Self, NostrError> {
        let id = uuid::Uuid::new_v4().to_string();
        let author = key.pubkey().to_string();
        let sig = key.sign_message(&Self::signed_bytes(adm, &id, &author, ts, &body)?)?;
        Ok(Self {
            id,
            author,
            ts,
            body,
            sig,
        })
    }

    /// Signed by `author` for the room whose admin pubkey is `adm`.
    pub fn verify(&self, adm: &str) -> bool {
        Self::signed_bytes(adm, &self.id, &self.author, self.ts, &self.body)
            .map(|bytes| verify_message(&self.author, &bytes, &self.sig))
            .unwrap_or(false)
    }

    pub(crate) fn signed_bytes(adm: &str, id: &str, author: &str, ts: u64, body: &RoomBody) -> Result<Vec<u8>, NostrError> {
        Ok(serde_json::to_vec(&("dchat:envelope:v2", adm, id, author, ts, body))?)
    }
}

/// Seal `value` from `sender` to the member whose session pubkey is `to` (ECDH + AEAD).
pub fn seal_json<T: Serialize>(sender: &NostrBurnerKey, to: &str, value: &T) -> Result<EncryptedPayload, NostrError> {
    let key = sender.shared_key(to)?;
    encrypt_json(&key, value).map_err(|e| NostrError::Crypto(e.to_string()))
}

/// Open a payload sealed to `recipient` by the member whose session pubkey is `sender`.
pub fn open_json<T: serde::de::DeserializeOwned>(
    recipient: &NostrBurnerKey,
    sender: &str,
    payload: &EncryptedPayload,
) -> Option<T> {
    let key = recipient.shared_key(sender).ok()?;
    decrypt_json(&key, payload).ok()
}

/// The plaintext inside a `RoomBody::Dm`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DmContent {
    pub text: String,
}

/// A rekey grant for one member, readable only with that member's session key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SealedGrant {
    pub to: String,
    pub payload: EncryptedPayload,
}

/// The new room a rekey moves members to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomGrant {
    pub room: String,
    /// Base64url room key, as in the URL fragment.
    pub key: String,
}

impl SealedGrant {
    /// Seal `grant` from `sender` to the member whose session pubkey is `to`.
    pub fn seal(sender: &NostrBurnerKey, to: &str, grant: &RoomGrant) -> Result<Self, NostrError> {
        Ok(Self {
            to: to.to_string(),
            payload: seal_json(sender, to, grant)?,
        })
    }

    /// Open a grant sealed to `recipient` by the member whose session pubkey is `sender`.
    pub fn open(&self, recipient: &NostrBurnerKey, sender: &str) -> Option<RoomGrant> {
        open_json(recipient, sender, &self.payload)
    }
}

/// At most this many members in a handover's ranking.
pub const MAX_RANKING: usize = 256;

/// The plaintext inside a `RoomBody::AdminHandover`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandoverContent {
    /// The admin secret key (hex), as in the admin link's `admsk`.
    pub admsk: String,
    /// The sender's seniority order of the members present, most senior first.
    pub ranking: Vec<String>,
}

impl std::fmt::Debug for HandoverContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverContent").field("admsk", &"<secret>").field("ranking", &self.ranking).finish()
    }
}

/// Open a handover sealed to `recipient` by `sender`; `None` unless it opens and its secret
/// is the room's admin key (`adm`).
pub fn open_handover(recipient: &NostrBurnerKey, sender: &str, sealed: &EncryptedPayload, adm: &str) -> Option<HandoverContent> {
    let mut content: HandoverContent = open_json(recipient, sender, sealed)?;
    let admin = NostrBurnerKey::from_secret_hex(&content.admsk).ok()?;
    if admin.pubkey() != adm {
        return None;
    }
    content.ranking.truncate(MAX_RANKING);
    Some(content)
}

/// Bytes an admin key signs to vouch that `session_pubkey` is an admin of `room_id`.
/// Binding the session key stops one member from replaying another's proof.
pub fn admin_proof_message(room_id: &str, session_pubkey: &str) -> Vec<u8> {
    format!("dchat:admin:{room_id}:{session_pubkey}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_sign_verify_and_tamper() {
        let key = NostrBurnerKey::generate().unwrap();
        let env = RoomEnvelope::sign(&key, "adm", 42, RoomBody::Chat { text: "hi".into() }).unwrap();
        assert!(env.verify("adm"));

        let json = serde_json::to_string(&env).unwrap();
        let parsed: RoomEnvelope = serde_json::from_str(&json).unwrap();
        assert!(parsed.verify("adm"));

        let mut forged = env.clone();
        forged.body = RoomBody::Chat { text: "bye".into() };
        assert!(!forged.verify("adm"));

        let mut reattributed = env.clone();
        reattributed.author = NostrBurnerKey::generate().unwrap().pubkey().to_string();
        assert!(!reattributed.verify("adm"));

        let mut retimed = env;
        retimed.ts = 43;
        assert!(!retimed.verify("adm"));
    }

    #[test]
    fn test_envelope_bound_to_admin_key() {
        // A member of two rooms can't replay one room's messages into the other.
        let key = NostrBurnerKey::generate().unwrap();
        let env = RoomEnvelope::sign(&key, "room-a-admin", 1, RoomBody::Chat { text: "hi".into() }).unwrap();
        assert!(env.verify("room-a-admin"));
        assert!(!env.verify("room-b-admin"));
        assert!(!env.verify(""));
    }

    #[test]
    fn test_room_body_recipient() {
        let request = RoomBody::FileRequest { to: "a".into(), file_id: "f".into() };
        assert_eq!(request.recipient(), Some("a"));
        let withdraw = RoomBody::FileCancel { to: None, file_id: "f".into() };
        assert_eq!(withdraw.recipient(), None);
        let stop = RoomBody::FileCancel { to: Some("b".into()), file_id: "f".into() };
        assert_eq!(stop.recipient(), Some("b"));
        assert_eq!(RoomBody::Chat { text: "x".into() }.recipient(), None);
        let sync_bodies = [
            RoomBody::SyncSummary { to: "c".into(), horizon: None, windows: vec![] },
            RoomBody::SyncAsk { to: "c".into(), horizon: Some(1), level: 1, starts: vec![0] },
            RoomBody::SyncDiff { to: "c".into(), ids: vec![], level: 1, windows: vec![], last: true },
            RoomBody::SyncWant { to: "c".into(), ids: vec!["m".into()] },
            RoomBody::SyncBatch { to: "c".into(), envelopes: vec![], last: true },
        ];
        for body in sync_bodies {
            assert_eq!(body.recipient(), Some("c"), "history sync is direct-only: {body:?}");
        }
        assert_eq!(RoomBody::ControlRequest { to: "s".into(), mouse_keyboard: true, controller: false }.recipient(), Some("s"));
        assert_eq!(RoomBody::ControlRelease { to: "s".into() }.recipient(), Some("s"));
        let status = RoomBody::ControlStatus { seq: 1, available: true, controllers: 4, mouse_keyboard: None, pads: vec![] };
        assert_eq!(status.recipient(), None, "everyone sees who controls what");
        let rekey = RoomBody::AdminRekey { kicked: Some("x".into()), grants: vec![] };
        assert_eq!(rekey.recipient(), None, "rekeys are gossiped so relayed members get theirs");
    }

    #[test]
    fn test_sealed_grant_only_opens_for_its_recipient() {
        let admin = NostrBurnerKey::generate().unwrap();
        let bo = NostrBurnerKey::generate().unwrap();
        let cy = NostrBurnerKey::generate().unwrap();
        let grant = RoomGrant { room: "newroom".into(), key: "k3y".into() };
        let sealed = SealedGrant::seal(&admin, bo.pubkey(), &grant).unwrap();
        assert_eq!(sealed.to, bo.pubkey());
        assert_eq!(sealed.open(&bo, admin.pubkey()), Some(grant));
        assert_eq!(sealed.open(&cy, admin.pubkey()), None, "another member cannot open it");
        assert_eq!(sealed.open(&bo, cy.pubkey()), None, "wrong sender key fails");
    }

    #[test]
    fn test_link_signals_are_direct_only() {
        let offer = SignalPayload::Offer { to: "b".into(), sdp: "v=0".into() };
        let body = RoomBody::LinkSignal { to: "b".into(), signal: offer.clone() };
        assert_eq!(body.recipient(), Some("b"));

        let key = NostrBurnerKey::generate().unwrap();
        let env = RoomEnvelope::sign(&key, "", 1, body).unwrap();
        let parsed: RoomEnvelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert!(parsed.verify(""));
        assert_eq!(parsed.body, RoomBody::LinkSignal { to: "b".into(), signal: offer });
    }

    #[test]
    fn test_old_clients_cannot_verify_bodies_with_added_fields() {
        // One reason members link only on the same PROTOCOL_VERSION: a verifier that drops
        // an unknown field re-serializes different bytes, so the signature no longer matches.
        let key = NostrBurnerKey::generate().unwrap();
        let offer = RoomBody::FileOffer {
            file_id: "f".into(),
            name: "a.txt".into(),
            size: 1,
            mime_type: "text/plain".into(),
            caption: Some("c".into()),
        };
        let env = RoomEnvelope::sign(&key, "adm", 1, offer).unwrap();
        let mut json: serde_json::Value = serde_json::to_value(&env).unwrap();
        json["body"]["data"].as_object_mut().unwrap().remove("caption");
        let as_old_client_sees_it: RoomEnvelope = serde_json::from_value(json).unwrap();
        assert!(!as_old_client_sees_it.verify("adm"));
    }

    #[test]
    fn test_dm_seal_roundtrip() {
        let ana = NostrBurnerKey::generate().unwrap();
        let bo = NostrBurnerKey::generate().unwrap();
        let cy = NostrBurnerKey::generate().unwrap();
        let sealed = seal_json(&ana, bo.pubkey(), &DmContent { text: "psst".into() }).unwrap();
        assert_eq!(open_json::<DmContent>(&bo, ana.pubkey(), &sealed), Some(DmContent { text: "psst".into() }));
        assert_eq!(open_json::<DmContent>(&cy, ana.pubkey(), &sealed), None);
        assert!(!serde_json::to_string(&RoomBody::Dm { sealed: sealed.clone() }).unwrap().contains(bo.pubkey()));
        assert_eq!(RoomBody::Dm { sealed }.recipient(), None, "DMs are relayable");
    }

    #[test]
    fn test_relay_key_cert_binds_topic_and_key() {
        let identity = NostrBurnerKey::generate().unwrap();
        let relay_key = NostrBurnerKey::generate().unwrap();
        let other_key = NostrBurnerKey::generate().unwrap();
        let cert = identity.sign_message(&relay_key_message("topic", relay_key.pubkey())).unwrap();
        assert!(verify_message(identity.pubkey(), &relay_key_message("topic", relay_key.pubkey()), &cert));
        assert!(!verify_message(identity.pubkey(), &relay_key_message("topic", other_key.pubkey()), &cert));
        assert!(!verify_message(identity.pubkey(), &relay_key_message("other", relay_key.pubkey()), &cert));
        assert!(!verify_message(other_key.pubkey(), &relay_key_message("topic", relay_key.pubkey()), &cert));
    }

    #[test]
    fn test_signal_recipient() {
        let offer = SignalPayload::Offer { to: "abc".into(), sdp: "v=0".into() };
        assert_eq!(offer.recipient(), Some("abc"));
        assert_eq!(SignalPayload::Presence.recipient(), None);

        let json = serde_json::to_string(&offer).unwrap();
        assert_eq!(serde_json::from_str::<SignalPayload>(&json).unwrap(), offer);
    }

    #[test]
    fn test_relay_signals_seal_handshakes_to_their_recipient() {
        let ana = NostrBurnerKey::generate().unwrap();
        let bo = NostrBurnerKey::generate().unwrap();
        let cy = NostrBurnerKey::generate().unwrap();
        let offer = SignalPayload::Offer { to: bo.pubkey().into(), sdp: "v=0 c=IN IP4 203.0.113.7".into() };

        let relayed = RelaySignal::seal(&ana, &offer).unwrap();
        assert_eq!(relayed.recipient(), Some(bo.pubkey()));
        // What the room key reveals: who it is for, not the SDP or its addresses.
        let with_room_key = serde_json::to_string(&relayed).unwrap();
        assert!(!with_room_key.contains("v=0") && !with_room_key.contains("203.0.113.7"));

        assert_eq!(relayed.open(&bo, ana.pubkey()), Some(offer.clone()));
        assert_eq!(relayed.open(&cy, ana.pubkey()), None, "another member cannot open it");
        assert_eq!(relayed.open(&bo, cy.pubkey()), None, "the sender's key is part of the seal");

        // A member sealing to Bo a signal addressed to someone else is refused.
        let misaddressed = RelaySignal::Sealed {
            to: bo.pubkey().into(),
            sealed: seal_json(&ana, bo.pubkey(), &SignalPayload::Answer { to: cy.pubkey().into(), sdp: "x".into() })
                .unwrap(),
        };
        assert_eq!(misaddressed.open(&bo, ana.pubkey()), None);

        // Room-wide signals carry nothing to seal.
        for signal in [SignalPayload::Presence, SignalPayload::PeerLeft] {
            let relayed = RelaySignal::seal(&ana, &signal).unwrap();
            assert_eq!(relayed.recipient(), None);
            assert_eq!(relayed.open(&cy, ana.pubkey()), Some(signal));
        }
    }

    #[test]
    fn test_handover_opens_only_for_its_recipient_and_the_room_admin_key() {
        let admin = NostrBurnerKey::generate().unwrap();
        let ana = NostrBurnerKey::generate().unwrap();
        let bo = NostrBurnerKey::generate().unwrap();
        let cy = NostrBurnerKey::generate().unwrap();
        let content = HandoverContent { admsk: admin.secret_hex(), ranking: vec![bo.pubkey().into(), cy.pubkey().into()] };
        let sealed = seal_json(&ana, bo.pubkey(), &content).unwrap();
        assert_eq!(open_handover(&bo, ana.pubkey(), &sealed, admin.pubkey()), Some(content.clone()));
        assert_eq!(open_handover(&cy, ana.pubkey(), &sealed, admin.pubkey()), None, "only the recipient opens it");
        assert_eq!(open_handover(&bo, cy.pubkey(), &sealed, admin.pubkey()), None, "the sender's key is part of the seal");
        assert_eq!(open_handover(&bo, ana.pubkey(), &sealed, cy.pubkey()), None, "another room's admin key");
        // On the wire: neither the secret nor the recipient.
        let body = RoomBody::AdminHandover { promote: false, sealed };
        let wire = serde_json::to_string(&body).unwrap();
        assert!(!wire.contains(&admin.secret_hex()) && !wire.contains(bo.pubkey()));
        assert_eq!(body.recipient(), None, "relayed by everyone");
        assert!(!format!("{content:?}").contains(&admin.secret_hex()));
    }

    #[test]
    fn test_admin_proof_binds_session() {
        let admin = NostrBurnerKey::generate().unwrap();
        let proof = admin.sign_message(&admin_proof_message("room1", "sessA")).unwrap();
        assert!(verify_message(admin.pubkey(), &admin_proof_message("room1", "sessA"), &proof));
        assert!(!verify_message(admin.pubkey(), &admin_proof_message("room1", "sessB"), &proof));
        assert!(!verify_message(admin.pubkey(), &admin_proof_message("room2", "sessA"), &proof));
    }
}
