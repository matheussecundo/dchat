use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use k256::ProjectivePoint;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Ephemeral kind for dchat WebRTC signaling payloads (NIP-16: 20000 <= kind < 30000).
/// Compliant Nostr relays drop these immediately and never store them to disk.
pub const KIND_EPHEMERAL_SIGNAL: u64 = 20001;

#[derive(Error, Debug)]
pub enum NostrError {
    #[error("Failed to parse hex string: {0}")]
    HexDecode(#[from] hex::FromHexError),
    #[error("Failed to serialize/deserialize JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid key or signature: {0}")]
    Crypto(String),
}

/// A Nostr Event conforming to NIP-01.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// Filter for subscribing to Nostr events (NIP-01).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct NostrFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u64>>,
    #[serde(rename = "#d", skip_serializing_if = "Option::is_none")]
    pub d_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

impl NostrFilter {
    pub fn matches(&self, event: &NostrEvent) -> bool {
        if let Some(ref kinds) = self.kinds {
            if !kinds.contains(&event.kind) {
                return false;
            }
        }
        if let Some(ref authors) = self.authors {
            if !authors.contains(&event.pubkey) {
                return false;
            }
        }
        if let Some(ref ids) = self.ids {
            if !ids.contains(&event.id) {
                return false;
            }
        }
        if let Some(ref d_tags) = self.d_tags {
            let has_matching_d = event.tags.iter().any(|tag| {
                tag.len() >= 2 && tag[0] == "d" && d_tags.contains(&tag[1])
            });
            if !has_matching_d {
                return false;
            }
        }
        if let Some(since) = self.since {
            if event.created_at < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if event.created_at > until {
                return false;
            }
        }
        true
    }
}

/// Messages sent from Client to Nostr Relay (NIP-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRelayMessage {
    Event(NostrEvent),
    Req {
        sub_id: String,
        filters: Vec<NostrFilter>,
    },
    Close(String),
}

impl ClientRelayMessage {
    pub fn to_json(&self) -> Result<String, NostrError> {
        match self {
            ClientRelayMessage::Event(event) => {
                let val = serde_json::json!(["EVENT", event]);
                Ok(serde_json::to_string(&val)?)
            }
            ClientRelayMessage::Req { sub_id, filters } => {
                let mut arr = vec![serde_json::json!("REQ"), serde_json::json!(sub_id)];
                for f in filters {
                    arr.push(serde_json::to_value(f)?);
                }
                Ok(serde_json::to_string(&arr)?)
            }
            ClientRelayMessage::Close(sub_id) => {
                let val = serde_json::json!(["CLOSE", sub_id]);
                Ok(serde_json::to_string(&val)?)
            }
        }
    }
}

/// Messages received from Nostr Relay to Client (NIP-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayClientMessage {
    Event {
        sub_id: String,
        event: NostrEvent,
    },
    Ok {
        event_id: String,
        accepted: bool,
        message: String,
    },
    Eose(String),
    Notice(String),
}

impl RelayClientMessage {
    pub fn from_json(s: &str) -> Result<Option<Self>, NostrError> {
        let val: serde_json::Value = serde_json::from_str(s)?;
        let arr = match val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => return Ok(None),
        };

        let msg_type = match arr[0].as_str() {
            Some(t) => t,
            None => return Ok(None),
        };

        match msg_type {
            "EVENT" if arr.len() >= 3 => {
                let sub_id = arr[1].as_str().unwrap_or("").to_string();
                let event: NostrEvent = serde_json::from_value(arr[2].clone())?;
                Ok(Some(RelayClientMessage::Event { sub_id, event }))
            }
            "OK" if arr.len() >= 3 => {
                let event_id = arr[1].as_str().unwrap_or("").to_string();
                let accepted = arr[2].as_bool().unwrap_or(false);
                let message = arr.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(Some(RelayClientMessage::Ok {
                    event_id,
                    accepted,
                    message,
                }))
            }
            "EOSE" if arr.len() >= 2 => {
                let sub_id = arr[1].as_str().unwrap_or("").to_string();
                Ok(Some(RelayClientMessage::Eose(sub_id)))
            }
            "NOTICE" if arr.len() >= 2 => {
                let notice = arr[1].as_str().unwrap_or("").to_string();
                Ok(Some(RelayClientMessage::Notice(notice)))
            }
            _ => Ok(None),
        }
    }
}

/// Compute SHA-256 hash of room ID for the Nostr topic tag.
/// Invariant: raw room ID is never exposed in cleartext to relay filters.
pub fn hash_room_topic(room_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"dchat:room:");
    hasher.update(room_id.as_bytes());
    hex::encode(hasher.finalize())
}

/// Serialize event fields to NIP-01 canonical JSON string:
/// `[0, <pubkey>, <created_at>, <kind>, <tags>, <content>]`
pub fn serialize_for_id(
    pubkey: &str,
    created_at: u64,
    kind: u64,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    serde_json::json!([0, pubkey, created_at, kind, tags, content]).to_string()
}

/// Ephemeral secp256k1 burner keypair living purely in RAM.
pub struct NostrBurnerKey {
    signing_key: SigningKey,
    pubkey_hex: String,
}

impl NostrBurnerKey {
    /// Generate a fresh random secp256k1 keypair.
    pub fn generate() -> Result<Self, NostrError> {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        Self::from_secret_bytes(&secret)
    }

    /// Restore a keypair from its 32-byte secret in hex (e.g. the admin key in a URL fragment).
    pub fn from_secret_hex(secret_hex: &str) -> Result<Self, NostrError> {
        Self::from_secret_bytes(&hex::decode(secret_hex)?)
    }

    fn from_secret_bytes(secret: &[u8]) -> Result<Self, NostrError> {
        let signing_key = SigningKey::from_bytes(secret)
            .map_err(|e| NostrError::Crypto(format!("Invalid private key: {e}")))?;
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());
        Ok(Self {
            signing_key,
            pubkey_hex,
        })
    }

    pub fn pubkey(&self) -> &str {
        &self.pubkey_hex
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.signing_key.to_bytes())
    }

    /// 32-byte key shared with the holder of `peer_pubkey_hex` (x-only): ECDH on secp256k1
    /// (both sides use their even-Y BIP-340 keys, so either side derives the same point),
    /// then SHA-256 over a domain tag, the shared X and both pubkeys in sorted order.
    pub fn shared_key(&self, peer_pubkey_hex: &str) -> Result<[u8; 32], NostrError> {
        let peer = VerifyingKey::from_bytes(&hex::decode(peer_pubkey_hex)?)
            .map_err(|e| NostrError::Crypto(format!("Invalid peer key: {e}")))?;
        let shared = (ProjectivePoint::from(*peer.as_affine()) * **self.signing_key.as_nonzero_scalar()).to_affine();
        let encoded = shared.to_encoded_point(false);
        let x = encoded.x().ok_or_else(|| NostrError::Crypto("Shared point at infinity".into()))?;
        let (a, b) = if self.pubkey_hex.as_str() <= peer_pubkey_hex {
            (self.pubkey_hex.as_str(), peer_pubkey_hex)
        } else {
            (peer_pubkey_hex, self.pubkey_hex.as_str())
        };
        let mut hasher = Sha256::new();
        hasher.update(b"dchat:ecdh:");
        hasher.update(x);
        hasher.update(a.as_bytes());
        hasher.update(b.as_bytes());
        Ok(hasher.finalize().into())
    }

    /// BIP-340 sign an arbitrary dchat message (domain-separated, see `message_digest`).
    pub fn sign_message(&self, msg: &[u8]) -> Result<String, NostrError> {
        Ok(hex::encode(self.sign_raw_32(&message_digest(msg))?.to_bytes()))
    }

    /// Sign and construct a NostrEvent with BIP-340 Schnorr signature.
    pub fn create_event(
        &self,
        kind: u64,
        tags: Vec<Vec<String>>,
        content: String,
        created_at: u64,
    ) -> Result<NostrEvent, NostrError> {
        let serialized = serialize_for_id(&self.pubkey_hex, created_at, kind, &tags, &content);
        let mut hasher = Sha256::new();
        hasher.update(serialized.as_bytes());
        let id_bytes = hasher.finalize();
        let id_hex = hex::encode(id_bytes);

        let sig_hex = hex::encode(self.sign_raw_32(&id_bytes)?.to_bytes());

        Ok(NostrEvent {
            id: id_hex,
            pubkey: self.pubkey_hex.clone(),
            created_at,
            kind,
            tags,
            content,
            sig: sig_hex,
        })
    }

    /// BIP-340 sign a 32-byte message as-is (no extra hashing), with fresh aux randomness.
    /// NIP-01 requires the signature to cover the raw event id, so the `Signer` trait
    /// (which SHA-256 hashes its input first) must not be used here.
    fn sign_raw_32(&self, msg: &[u8]) -> Result<Signature, NostrError> {
        let mut aux_rand = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut aux_rand);
        self.signing_key
            .sign_raw(msg, &aux_rand)
            .map_err(|e| NostrError::Crypto(format!("Signing failed: {e}")))
    }
}

/// Digest signed by `sign_message`; the prefix keeps dchat message signatures from ever
/// being valid as signatures over a Nostr event id (or vice versa).
fn message_digest(msg: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"dchat:msg:");
    hasher.update(msg);
    hasher.finalize().into()
}

/// Parse a 64-byte BIP-340 signature. The length is checked first because k256's
/// `Signature::try_from` panics on short input instead of returning an error.
fn parse_signature(sig_bytes: &[u8]) -> Option<Signature> {
    if sig_bytes.len() != 64 {
        return None;
    }
    Signature::try_from(sig_bytes).ok()
}

/// Verify a `NostrBurnerKey::sign_message` signature against an x-only pubkey in hex.
pub fn verify_message(pubkey_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    let (Ok(pubkey_bytes), Ok(sig_bytes)) = (hex::decode(pubkey_hex), hex::decode(sig_hex)) else {
        return false;
    };
    let (Ok(verifying_key), Some(signature)) = (
        VerifyingKey::from_bytes(&pubkey_bytes),
        parse_signature(&sig_bytes),
    ) else {
        return false;
    };
    verifying_key
        .verify_raw(&message_digest(msg), &signature)
        .is_ok()
}

/// Verify a NostrEvent signature according to NIP-01 and BIP-340.
pub fn verify_event(event: &NostrEvent) -> Result<bool, NostrError> {
    let serialized = serialize_for_id(
        &event.pubkey,
        event.created_at,
        event.kind,
        &event.tags,
        &event.content,
    );
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    let id_bytes = hasher.finalize();
    let expected_id = hex::encode(id_bytes);

    if expected_id != event.id {
        return Ok(false);
    }

    let pubkey_bytes = hex::decode(&event.pubkey)?;
    let verifying_key = match VerifyingKey::from_bytes(&pubkey_bytes) {
        Ok(vk) => vk,
        Err(_) => return Ok(false),
    };

    let sig_bytes = hex::decode(&event.sig)?;
    let signature = match parse_signature(&sig_bytes) {
        Some(s) => s,
        None => return Ok(false),
    };

    Ok(verifying_key.verify_raw(&id_bytes, &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_burner_key_generation_and_event_signing() {
        let key = NostrBurnerKey::generate().expect("Generate burner key");
        assert_eq!(key.pubkey().len(), 64);

        let tags = vec![vec!["d".to_string(), "test-topic".to_string()]];
        let content = "Encrypted signal content".to_string();
        let created_at = 1700000000;

        let event = key
            .create_event(KIND_EPHEMERAL_SIGNAL, tags, content, created_at)
            .expect("Create event");

        assert_eq!(event.pubkey, key.pubkey());
        assert_eq!(event.kind, KIND_EPHEMERAL_SIGNAL);
        assert_eq!(event.id.len(), 64);
        assert_eq!(event.sig.len(), 128);

        let is_valid = verify_event(&event).expect("Verify event");
        assert!(is_valid, "Signature verification must succeed");
    }

    #[test]
    fn test_bip340_vectors() {
        // Official BIP-340 test vectors 0 and 1 (secret key, aux_rand, message, signature).
        let vectors = [
            (
                "0000000000000000000000000000000000000000000000000000000000000003",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "E907831F80848D1069A5371B402410364BDF1C5F8307B0084C55F1CE2DCA821525F66A4A85EA8B71E482A74F382D2CE5EBEEE8FDB2172F477DF4900D310536C0",
            ),
            (
                "B7E151628AED2A6ABF7158809CF4F3C762E7160F38B4DA56A784D9045190CFEF",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "243F6A8885A308D313198A2E03707344A4093822299F31D0082EFA98EC4E6C89",
                "6896BD60EEAE296DB48A229FF71DFE071BDE413E6D43F917DC8DCF8C78DE33418906D11AC976ABCCB20B091292BFF4EA897EFCB639EA871CFA95F6DE339E4B0A",
            ),
        ];
        for (sk, aux, msg, sig) in vectors {
            let key = SigningKey::from_bytes(&hex::decode(sk).unwrap()).unwrap();
            let aux: [u8; 32] = hex::decode(aux).unwrap().try_into().unwrap();
            let msg = hex::decode(msg).unwrap();
            let signature = key.sign_raw(&msg, &aux).unwrap();
            assert_eq!(hex::encode_upper(signature.to_bytes()), sig);
            assert!(key.verifying_key().verify_raw(&msg, &signature).is_ok());
        }
    }

    #[test]
    fn test_event_signature_covers_raw_id() {
        // NIP-01: sig is a BIP-340 signature over the 32-byte id itself, not sha256(id).
        use k256::schnorr::signature::Verifier;

        let key = NostrBurnerKey::generate().expect("Generate burner key");
        let event = key
            .create_event(KIND_EPHEMERAL_SIGNAL, vec![], "hi".into(), 1700000000)
            .expect("Create event");

        let id_bytes = hex::decode(&event.id).unwrap();
        let vk = VerifyingKey::from_bytes(&hex::decode(&event.pubkey).unwrap()).unwrap();
        let sig = Signature::try_from(hex::decode(&event.sig).unwrap().as_slice()).unwrap();

        assert!(vk.verify_raw(&id_bytes, &sig).is_ok(), "sig must cover the raw id");
        assert!(vk.verify(&id_bytes, &sig).is_err(), "sig must not cover sha256(id)");
    }

    #[test]
    fn test_message_signing_roundtrip() {
        let key = NostrBurnerKey::generate().expect("Generate key");
        let sig = key.sign_message(b"hello room").expect("Sign");
        assert!(verify_message(key.pubkey(), b"hello room", &sig));
        assert!(!verify_message(key.pubkey(), b"hello rooM", &sig));

        let other = NostrBurnerKey::generate().expect("Generate key");
        assert!(!verify_message(other.pubkey(), b"hello room", &sig));
        assert!(!verify_message("zz", b"hello room", &sig));
        assert!(!verify_message(key.pubkey(), b"hello room", "00"));
    }

    #[test]
    fn test_shared_key_is_symmetric_and_pairwise() {
        let a = NostrBurnerKey::generate().unwrap();
        let b = NostrBurnerKey::generate().unwrap();
        let c = NostrBurnerKey::generate().unwrap();
        let ab = a.shared_key(b.pubkey()).unwrap();
        assert_eq!(ab, b.shared_key(a.pubkey()).unwrap());
        assert_ne!(ab, a.shared_key(c.pubkey()).unwrap());
        assert_ne!(ab, c.shared_key(b.pubkey()).unwrap());
        assert!(a.shared_key("zz").is_err());
        // Sealing to oneself also works (an admin includes its own grant).
        assert_eq!(a.shared_key(a.pubkey()).unwrap(), a.shared_key(a.pubkey()).unwrap());
    }

    #[test]
    fn test_secret_hex_roundtrip() {
        let key = NostrBurnerKey::generate().expect("Generate key");
        let restored = NostrBurnerKey::from_secret_hex(&key.secret_hex()).expect("Restore");
        assert_eq!(restored.pubkey(), key.pubkey());
        assert!(NostrBurnerKey::from_secret_hex("not-hex").is_err());
    }

    #[test]
    fn test_tampered_event_fails_verification() {
        let key = NostrBurnerKey::generate().expect("Generate burner key");
        let mut event = key
            .create_event(
                KIND_EPHEMERAL_SIGNAL,
                vec![],
                "Original content".into(),
                1700000000,
            )
            .expect("Create event");

        // Tamper content
        event.content = "Tampered content".into();
        let is_valid = verify_event(&event).expect("Verify");
        assert!(!is_valid, "Tampered event must fail verification");
    }

    #[test]
    fn test_malformed_signature_rejected_without_panic() {
        let key = NostrBurnerKey::generate().expect("Generate burner key");
        let mut event = key
            .create_event(KIND_EPHEMERAL_SIGNAL, vec![], "x".into(), 1700000000)
            .expect("Create event");
        for bad in ["00", "", &"ab".repeat(63), &"ab".repeat(65)] {
            event.sig = bad.to_string();
            assert!(!verify_event(&event).unwrap_or(false));
        }
    }

    #[test]
    fn test_client_relay_messages_json() {
        let req = ClientRelayMessage::Req {
            sub_id: "sub-123".into(),
            filters: vec![NostrFilter {
                kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
                d_tags: Some(vec!["topic1".into()]),
                ..Default::default()
            }],
        };

        let json = req.to_json().expect("to json");
        assert!(json.contains("REQ"));
        assert!(json.contains("sub-123"));
        assert!(json.contains("#d"));
        assert!(json.contains("topic1"));

        let close = ClientRelayMessage::Close("sub-123".into());
        let json = close.to_json().expect("to json");
        assert_eq!(json, r#"["CLOSE","sub-123"]"#);
    }

    #[test]
    fn test_relay_client_messages_parse() {
        let event_json = r#"["EVENT", "sub-1", {"id": "abc", "pubkey": "def", "created_at": 100, "kind": 20001, "tags": [], "content": "hello", "sig": "123"}]"#;
        let msg = RelayClientMessage::from_json(event_json).expect("parse");
        match msg {
            Some(RelayClientMessage::Event { sub_id, event }) => {
                assert_eq!(sub_id, "sub-1");
                assert_eq!(event.id, "abc");
                assert_eq!(event.content, "hello");
            }
            other => panic!("Unexpected msg: {:?}", other),
        }

        let eose_json = r#"["EOSE", "sub-1"]"#;
        let msg = RelayClientMessage::from_json(eose_json).expect("parse");
        assert_eq!(msg, Some(RelayClientMessage::Eose("sub-1".into())));

        let ok_json = r#"["OK", "evt-123", true, "pow: 20"]"#;
        let msg = RelayClientMessage::from_json(ok_json).expect("parse");
        assert_eq!(
            msg,
            Some(RelayClientMessage::Ok {
                event_id: "evt-123".into(),
                accepted: true,
                message: "pow: 20".into(),
            })
        );
    }

    #[test]
    fn test_hash_room_topic() {
        let topic1 = hash_room_topic("room-abc");
        let topic2 = hash_room_topic("room-abc");
        let topic3 = hash_room_topic("room-xyz");
        assert_eq!(topic1, topic2);
        assert_ne!(topic1, topic3);
        assert_eq!(topic1.len(), 64);
    }

    #[test]
    fn test_filter_matches() {
        let event = NostrEvent {
            id: "event-123".into(),
            pubkey: "author-456".into(),
            created_at: 1000,
            kind: KIND_EPHEMERAL_SIGNAL,
            tags: vec![vec!["d".into(), "topic-abc".into()]],
            content: "hello".into(),
            sig: "sig".into(),
        };

        let f_match = NostrFilter {
            kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
            d_tags: Some(vec!["topic-abc".into()]),
            ..Default::default()
        };
        assert!(f_match.matches(&event));

        let f_no_match_kind = NostrFilter {
            kinds: Some(vec![1]),
            d_tags: Some(vec!["topic-abc".into()]),
            ..Default::default()
        };
        assert!(!f_no_match_kind.matches(&event));

        let f_no_match_tag = NostrFilter {
            kinds: Some(vec![KIND_EPHEMERAL_SIGNAL]),
            d_tags: Some(vec!["topic-other".into()]),
            ..Default::default()
        };
        assert!(!f_no_match_tag.matches(&event));
    }
}
