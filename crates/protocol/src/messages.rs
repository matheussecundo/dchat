use crate::crypto::EncryptedPayload;
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "content")]
pub enum SignalPayload {
    Presence,
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u16>,
    },
    IceBatch {
        candidates: Vec<IceCandidateData>,
    },
    PeerLeft,
}

/// Messages sent peer-to-peer over the WebRTC RTCDataChannel (also encrypted with ChaCha20-Poly1305).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum DataChannelMessage {
    Chat {
        id: String,
        sender: String,
        text: String,
        timestamp: u64,
    },
    FileOffer {
        id: String,
        sender: String,
        name: String,
        size: u64,
        mime_type: String,
        caption: Option<String>,
        timestamp: u64,
    },
    FileRequest {
        id: String,
    },
    FileCancel {
        id: String,
        reason: String,
    },
    FileComplete {
        id: String,
    },
    CallInvite,
    VideoCallInvite,
    ScreenShareInvite,
    CallAccepted,
    CallRejected,
    CallEnded,
    Ack {
        id: String,
    },
}
