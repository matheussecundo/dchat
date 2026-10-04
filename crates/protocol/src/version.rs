//! Protocol version: members link only with members on the same version, so the
//! messages they exchange never need to stay compatible with older or newer clients.

use crate::messages::RelaySignal;
use serde::{Deserialize, Serialize};

/// Bump whenever members on the old and new code could misunderstand each other: the
/// encoding of any signal or room message (the `wire_format_matches_protocol_version`
/// test catches those), signed bytes, the file chunk layout, or a rule every member must
/// apply alike (caps, gossip, history, rekey).
///
/// Never change how a plain room's relay topic is derived (`hash_room_topic`): members on
/// different versions only notice each other (and show the reload banner) on a shared topic.
pub const PROTOCOL_VERSION: u32 = 4;

/// What travels through the relays (encrypted with the room key): the sender's protocol
/// version and its signal. Every version must keep `v` readable, whatever `payload` becomes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedSignal<T> {
    pub v: u32,
    pub payload: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedSignal {
    Signal(RelaySignal),
    /// From a member on another protocol version: its payload is not parsed.
    OtherVersion(u32),
    Invalid,
}

pub fn encode_signal(signal: &RelaySignal, version: u32) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&VersionedSignal { v: version, payload: signal })
}

/// Read the version first, and parse the payload only when it matches `version`.
pub fn decode_signal(json: &[u8], version: u32) -> DecodedSignal {
    #[derive(Deserialize)]
    struct VersionOnly {
        v: u32,
    }
    match serde_json::from_slice::<VersionOnly>(json) {
        Ok(VersionOnly { v }) if v != version => DecodedSignal::OtherVersion(v),
        Ok(_) => match serde_json::from_slice::<VersionedSignal<RelaySignal>>(json) {
            Ok(signal) => DecodedSignal::Signal(signal.payload),
            Err(_) => DecodedSignal::Invalid,
        },
        Err(_) => DecodedSignal::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{encrypt_chunk, EncryptedPayload, CHUNK_SIZE};
    use crate::messages::*;
    use crate::nostr::hash_room_topic;
    use crate::input::{encode_events, seal_input, InputEvent, InputLane};
    use crate::keycodes::DomCode;
    use crate::password::{password_room_key, password_room_topic, stretch_password};
    use sha2::{Digest, Sha256};

    /// The wire fingerprint recorded for the current version. When the test below fails,
    /// bump `PROTOCOL_VERSION` and record the new pair here.
    const RECORDED: (u32, &str) = (4, "f7d7d32502cfb3a8c69465d407abb838821b2930ddb72199ef685e78d2ef2dc3");

    fn sealed() -> EncryptedPayload {
        EncryptedPayload { nonce: "n".into(), ciphertext: "c".into() }
    }

    /// One sample of every relay signal. Adding a variant breaks the exhaustive match
    /// below; add its sample here too.
    fn relay_signals() -> Vec<RelaySignal> {
        vec![
            RelaySignal::Presence,
            RelaySignal::PeerLeft,
            RelaySignal::Sealed { to: "b".into(), sealed: sealed() },
        ]
    }

    fn relay_signal_kind(signal: &RelaySignal) -> &'static str {
        match signal {
            RelaySignal::Presence => "Presence",
            RelaySignal::PeerLeft => "PeerLeft",
            RelaySignal::Sealed { .. } => "Sealed",
        }
    }

    /// One sample of every signal (sealed inside `RelaySignal::Sealed`, or sent over a
    /// link). Adding a variant breaks the exhaustive match below; add its sample here too.
    fn signals() -> Vec<SignalPayload> {
        vec![
            SignalPayload::Presence,
            SignalPayload::Offer { to: "b".into(), sdp: "v=0".into() },
            SignalPayload::Answer { to: "b".into(), sdp: "v=0".into() },
            SignalPayload::IceBatch {
                to: "b".into(),
                candidates: vec![IceCandidateData {
                    candidate: "candidate:1".into(),
                    sdp_mid: Some("0".into()),
                    sdp_m_line_index: Some(0),
                }],
            },
            SignalPayload::PeerLeft,
        ]
    }

    fn signal_kind(signal: &SignalPayload) -> &'static str {
        match signal {
            SignalPayload::Presence => "Presence",
            SignalPayload::Offer { .. } => "Offer",
            SignalPayload::Answer { .. } => "Answer",
            SignalPayload::IceBatch { .. } => "IceBatch",
            SignalPayload::PeerLeft => "PeerLeft",
        }
    }

    /// One sample of every room message. Adding a variant breaks the exhaustive match
    /// below; add its sample here too.
    fn bodies() -> Vec<RoomBody> {
        let envelope = RoomEnvelope {
            id: "id".into(),
            author: "a".into(),
            ts: 1,
            body: RoomBody::Chat { text: "hi".into(), shareable: true },
            sig: "sig".into(),
        };
        vec![
            RoomBody::Hello { name: "Ana".into(), join_ts: 1, admin_proof: Some("p".into()) },
            RoomBody::LinkState { seq: 1, direct: vec!["b".into()] },
            RoomBody::Chat { text: "hi".into(), shareable: true },
            RoomBody::VoiceState {
                seq: 1,
                in_voice: true,
                voice_ts: 2,
                mic_muted: false,
                video: VideoKind::Camera,
                video_ts: 3,
            },
            RoomBody::FileOffer {
                file_id: "f".into(),
                name: "a.txt".into(),
                size: 4,
                mime_type: "text/plain".into(),
                caption: Some("c".into()),
            },
            RoomBody::FileRequest { to: "b".into(), file_id: "f".into() },
            RoomBody::FileCancel { to: Some("b".into()), file_id: "f".into() },
            RoomBody::FileQueued { to: "b".into(), file_id: "f".into(), position: 1 },
            RoomBody::HistoryRequest { to: "b".into() },
            RoomBody::HistoryChunk { to: "b".into(), envelopes: vec![envelope] },
            RoomBody::Typing,
            RoomBody::Reaction { target: "m".into(), emoji: "👍".into(), on: true },
            RoomBody::Edit { target: "m".into(), text: "hello".into() },
            RoomBody::Delete { target: "m".into() },
            RoomBody::Dm { sealed: sealed() },
            RoomBody::LinkSignal { to: "b".into(), signal: SignalPayload::Offer { to: "b".into(), sdp: "v=0".into() } },
            RoomBody::AdminRekey {
                kicked: Some("c".into()),
                grants: vec![SealedGrant { to: "b".into(), payload: sealed() }],
            },
            RoomBody::ControlStatus { seq: 1, available: true, controllers: 2, mouse_keyboard: Some("b".into()), pads: vec![None, Some("c".into())] },
            RoomBody::ControlRequest { to: "s".into(), mouse_keyboard: true, controller: true },
            RoomBody::ControlGrant { to: "b".into(), mouse_keyboard: false, pad: Some(1), reason: Some(ControlEnd::TakenOver) },
            RoomBody::ControlRelease { to: "s".into() },
            RoomBody::Leave,
        ]
    }

    fn body_kind(body: &RoomBody) -> &'static str {
        match body {
            RoomBody::Hello { .. } => "Hello",
            RoomBody::LinkState { .. } => "LinkState",
            RoomBody::Chat { .. } => "Chat",
            RoomBody::VoiceState { .. } => "VoiceState",
            RoomBody::FileOffer { .. } => "FileOffer",
            RoomBody::FileRequest { .. } => "FileRequest",
            RoomBody::FileCancel { .. } => "FileCancel",
            RoomBody::FileQueued { .. } => "FileQueued",
            RoomBody::HistoryRequest { .. } => "HistoryRequest",
            RoomBody::HistoryChunk { .. } => "HistoryChunk",
            RoomBody::Typing => "Typing",
            RoomBody::Reaction { .. } => "Reaction",
            RoomBody::Edit { .. } => "Edit",
            RoomBody::Delete { .. } => "Delete",
            RoomBody::Dm { .. } => "Dm",
            RoomBody::LinkSignal { .. } => "LinkSignal",
            RoomBody::AdminRekey { .. } => "AdminRekey",
            RoomBody::ControlStatus { .. } => "ControlStatus",
            RoomBody::ControlRequest { .. } => "ControlRequest",
            RoomBody::ControlGrant { .. } => "ControlGrant",
            RoomBody::ControlRelease { .. } => "ControlRelease",
            RoomBody::Leave => "Leave",
        }
    }

    /// A hash over the encoding of everything members exchange.
    fn wire_fingerprint() -> String {
        let mut wire: Vec<String> = Vec::new();
        for signal in relay_signals() {
            wire.push(String::from_utf8(encode_signal(&signal, 0).unwrap()).unwrap());
        }
        for signal in signals() {
            wire.push(serde_json::to_string(&signal).unwrap());
        }
        for body in bodies() {
            wire.push(String::from_utf8(RoomEnvelope::signed_bytes("id", "a", 1, &body).unwrap()).unwrap());
        }
        for video in [VideoKind::None, VideoKind::Camera, VideoKind::Screen] {
            wire.push(serde_json::to_string(&video).unwrap());
        }
        wire.push(serde_json::to_string(&DmContent { text: "psst".into() }).unwrap());
        wire.push(serde_json::to_string(&RoomGrant { room: "r".into(), key: "k".into() }).unwrap());
        wire.push(String::from_utf8(admin_proof_message("r", "s")).unwrap());
        wire.push(hash_room_topic("r"));
        // Remote-control input: every event's binary encoding, the packet header, the keys.
        wire.push(hex::encode(encode_events(&crate::input::tests::samples()).unwrap()));
        let packet = seal_input(&[7; 32], "a", "b", InputLane::Events, 9, &[InputEvent::Alive]).unwrap();
        wire.push(format!("{:?} {}", &packet[..5], packet.len()));
        wire.push(DomCode::ALL.iter().map(|k| format!("{}={}", k.as_code(), k.hid())).collect::<Vec<_>>().join(","));
        wire.push(password_room_topic("r", &password_room_key(&[1; 32], &stretch_password("pw", &[2; 16]).unwrap())));
        // File chunks: the header layout (the rest is a random nonce and ciphertext).
        let packet = encrypt_chunk(&[7; 32], &[1; 16], 2, 3, b"data").unwrap();
        wire.push(format!("{:?} {} {}", &packet[..24], packet.len(), CHUNK_SIZE));
        hex::encode(Sha256::digest(wire.join("\n").as_bytes()))
    }

    #[test]
    fn test_samples_cover_every_variant_once() {
        let kinds: std::collections::HashSet<_> = relay_signals().iter().map(relay_signal_kind).collect();
        assert_eq!(kinds.len(), relay_signals().len());
        let kinds: std::collections::HashSet<_> = signals().iter().map(signal_kind).collect();
        assert_eq!(kinds.len(), signals().len());
        let kinds: std::collections::HashSet<_> = bodies().iter().map(body_kind).collect();
        assert_eq!(kinds.len(), bodies().len());
    }

    #[test]
    fn wire_format_matches_protocol_version() {
        let current = wire_fingerprint();
        assert_eq!(
            RECORDED.0, PROTOCOL_VERSION,
            "PROTOCOL_VERSION changed: record its fingerprint, RECORDED = ({PROTOCOL_VERSION}, \"{current}\")"
        );
        assert_eq!(
            RECORDED.1, current,
            "The wire format changed: members on the previous build would misread it. Bump PROTOCOL_VERSION \
             and set RECORDED = (<new version>, \"{current}\")"
        );
    }

    #[test]
    fn test_same_version_signals_roundtrip() {
        for signal in relay_signals() {
            let json = encode_signal(&signal, 4).unwrap();
            assert_eq!(decode_signal(&json, 4), DecodedSignal::Signal(signal));
        }
    }

    #[test]
    fn test_other_versions_are_reported_without_parsing_their_payload() {
        // A future version may send a payload this one cannot parse; the version still reads.
        let future = br#"{"v":9,"payload":{"kind":"Teleport","content":{"to":"b"}}}"#;
        assert_eq!(decode_signal(future, 4), DecodedSignal::OtherVersion(9));
        let older = encode_signal(&RelaySignal::Presence, 3).unwrap();
        assert_eq!(decode_signal(&older, 4), DecodedSignal::OtherVersion(3));
        let unknown_payload = br#"{"v":4,"payload":{"kind":"Teleport"}}"#;
        assert_eq!(decode_signal(unknown_payload, 4), DecodedSignal::Invalid);
        // Unversioned signals (bare payloads, before version 1) are not mistaken for anything.
        let bare = serde_json::to_vec(&RelaySignal::Presence).unwrap();
        assert_eq!(decode_signal(&bare, 4), DecodedSignal::Invalid);
    }
}
