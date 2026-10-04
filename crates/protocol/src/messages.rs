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

/// The inner signaling payload that is encrypted into an `EncryptedPayload` using
/// the secret key from the URL hash. The server/relays never see this in plaintext!
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

/// What a member is sending as video in the voice lounge (one source at a time).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum VideoKind {
    #[default]
    None,
    Camera,
    Screen,
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
    /// `shareable`: the author allows this message in the history shown to late joiners
    /// (set from the author's own room link, `&hist=1`).
    Chat {
        text: String,
        #[serde(default)]
        shareable: bool,
    },
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
    /// A late joiner asks a neighbor for the shareable recent messages it holds.
    HistoryRequest { to: String },
    /// Signed originals of recent shareable chat messages, oldest first.
    HistoryChunk { to: String, envelopes: Vec<RoomEnvelope> },
    /// The author is typing (sent at most every few seconds; expires on its own).
    Typing,
    /// Add (`on`) or remove the author's `emoji` reaction to message `target`.
    Reaction { target: String, emoji: String, on: bool },
    /// Replace the text of the author's own message `target`.
    Edit { target: String, text: String },
    /// Remove the author's own message `target` (best effort on honest clients).
    Delete { target: String },
    /// A private message to `to`, sealed with the two members' session keys (ECDH).
    /// Relayable like any message, but only `to` can open it and `to` never relays it.
    Dm { to: String, sealed: EncryptedPayload },
    /// WebRTC renegotiation (offer, answer, ICE) for the link between author and `to`, sent
    /// over that link itself once it is open, so Nostr relays only carry the first handshake.
    LinkSignal { to: String, signal: SignalPayload },
    /// Admin only: move the room to a new ID and key. Each remaining member gets its own
    /// grant sealed to its session key; `kicked` (if any) gets none.
    AdminRekey { kicked: Option<String>, grants: Vec<SealedGrant> },
    Leave,
}

impl RoomBody {
    /// The single member this message is for; such messages travel only over the direct
    /// link between the two and are never gossip-relayed.
    pub fn recipient(&self) -> Option<&str> {
        match self {
            RoomBody::FileRequest { to, .. }
            | RoomBody::FileQueued { to, .. }
            | RoomBody::HistoryRequest { to }
            | RoomBody::HistoryChunk { to, .. }
            | RoomBody::LinkSignal { to, .. } => Some(to),
            RoomBody::FileCancel { to, .. } => to.as_deref(),
            _ => None,
        }
    }
}

/// A room message signed by its author's session key, so it can be gossip-relayed
/// through other members without any of them being able to forge or alter it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomEnvelope {
    pub id: String,
    pub author: String,
    pub ts: u64,
    pub body: RoomBody,
    pub sig: String,
}

impl RoomEnvelope {
    pub fn sign(key: &NostrBurnerKey, ts: u64, body: RoomBody) -> Result<Self, NostrError> {
        let id = uuid::Uuid::new_v4().to_string();
        let author = key.pubkey().to_string();
        let sig = key.sign_message(&Self::signed_bytes(&id, &author, ts, &body)?)?;
        Ok(Self {
            id,
            author,
            ts,
            body,
            sig,
        })
    }

    pub fn verify(&self) -> bool {
        Self::signed_bytes(&self.id, &self.author, self.ts, &self.body)
            .map(|bytes| verify_message(&self.author, &bytes, &self.sig))
            .unwrap_or(false)
    }

    pub(crate) fn signed_bytes(id: &str, author: &str, ts: u64, body: &RoomBody) -> Result<Vec<u8>, NostrError> {
        Ok(serde_json::to_vec(&("dchat:envelope:v1", id, author, ts, body))?)
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
        let env = RoomEnvelope::sign(&key, 42, RoomBody::Chat { text: "hi".into(), shareable: false }).unwrap();
        assert!(env.verify());

        let json = serde_json::to_string(&env).unwrap();
        let parsed: RoomEnvelope = serde_json::from_str(&json).unwrap();
        assert!(parsed.verify());

        let mut forged = env.clone();
        forged.body = RoomBody::Chat { text: "bye".into(), shareable: false };
        assert!(!forged.verify());

        let mut reattributed = env.clone();
        reattributed.author = NostrBurnerKey::generate().unwrap().pubkey().to_string();
        assert!(!reattributed.verify());

        let mut retimed = env;
        retimed.ts = 43;
        assert!(!retimed.verify());
    }

    #[test]
    fn test_room_body_recipient() {
        let request = RoomBody::FileRequest { to: "a".into(), file_id: "f".into() };
        assert_eq!(request.recipient(), Some("a"));
        let withdraw = RoomBody::FileCancel { to: None, file_id: "f".into() };
        assert_eq!(withdraw.recipient(), None);
        let stop = RoomBody::FileCancel { to: Some("b".into()), file_id: "f".into() };
        assert_eq!(stop.recipient(), Some("b"));
        assert_eq!(RoomBody::Chat { text: "x".into(), shareable: true }.recipient(), None);
        assert_eq!(RoomBody::HistoryRequest { to: "c".into() }.recipient(), Some("c"));
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
        let env = RoomEnvelope::sign(&key, 1, body).unwrap();
        let parsed: RoomEnvelope = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert!(parsed.verify());
        assert_eq!(parsed.body, RoomBody::LinkSignal { to: "b".into(), signal: offer });
    }

    #[test]
    fn test_old_clients_cannot_verify_bodies_with_added_fields() {
        // One reason members link only on the same PROTOCOL_VERSION: a verifier that drops
        // an unknown field re-serializes different bytes, so the signature no longer matches.
        let key = NostrBurnerKey::generate().unwrap();
        let env = RoomEnvelope::sign(&key, 1, RoomBody::Chat { text: "hi".into(), shareable: true }).unwrap();
        let mut json: serde_json::Value = serde_json::to_value(&env).unwrap();
        json["body"]["data"].as_object_mut().unwrap().remove("shareable");
        let as_old_client_sees_it: RoomEnvelope = serde_json::from_value(json).unwrap();
        assert!(!as_old_client_sees_it.verify());
    }

    #[test]
    fn test_dm_seal_roundtrip() {
        let ana = NostrBurnerKey::generate().unwrap();
        let bo = NostrBurnerKey::generate().unwrap();
        let cy = NostrBurnerKey::generate().unwrap();
        let sealed = seal_json(&ana, bo.pubkey(), &DmContent { text: "psst".into() }).unwrap();
        assert_eq!(open_json::<DmContent>(&bo, ana.pubkey(), &sealed), Some(DmContent { text: "psst".into() }));
        assert_eq!(open_json::<DmContent>(&cy, ana.pubkey(), &sealed), None);
        assert_eq!(RoomBody::Dm { to: bo.pubkey().into(), sealed }.recipient(), None, "DMs are relayable");
    }

    #[test]
    fn test_chat_shareable_defaults_to_false() {
        let parsed: RoomBody = serde_json::from_str(r#"{"type":"Chat","data":{"text":"hi"}}"#).unwrap();
        assert_eq!(parsed, RoomBody::Chat { text: "hi".into(), shareable: false });
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
    fn test_admin_proof_binds_session() {
        let admin = NostrBurnerKey::generate().unwrap();
        let proof = admin.sign_message(&admin_proof_message("room1", "sessA")).unwrap();
        assert!(verify_message(admin.pubkey(), &admin_proof_message("room1", "sessA"), &proof));
        assert!(!verify_message(admin.pubkey(), &admin_proof_message("room1", "sessB"), &proof));
        assert!(!verify_message(admin.pubkey(), &admin_proof_message("room2", "sessA"), &proof));
    }
}
