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

/// The inner signaling payload that is encrypted into an `EncryptedPayload` using
/// the secret key from the URL hash. The server never sees this in plaintext!
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "content")]
pub enum SignalPayload {
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
    Ack {
        id: String,
    },
}
