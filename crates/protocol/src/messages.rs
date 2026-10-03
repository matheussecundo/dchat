use crate::crypto::EncryptedPayload;
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
    Leave,
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

    fn signed_bytes(id: &str, author: &str, ts: u64, body: &RoomBody) -> Result<Vec<u8>, NostrError> {
        Ok(serde_json::to_vec(&("dchat:envelope:v1", id, author, ts, body))?)
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
        let env = RoomEnvelope::sign(&key, 42, RoomBody::Chat { text: "hi".into() }).unwrap();
        assert!(env.verify());

        let json = serde_json::to_string(&env).unwrap();
        let parsed: RoomEnvelope = serde_json::from_str(&json).unwrap();
        assert!(parsed.verify());

        let mut forged = env.clone();
        forged.body = RoomBody::Chat { text: "bye".into() };
        assert!(!forged.verify());

        let mut reattributed = env.clone();
        reattributed.author = NostrBurnerKey::generate().unwrap().pubkey().to_string();
        assert!(!reattributed.verify());

        let mut retimed = env;
        retimed.ts = 43;
        assert!(!retimed.verify());
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
