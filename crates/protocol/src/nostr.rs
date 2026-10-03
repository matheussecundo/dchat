use k256::schnorr::signature::Signer;
use k256::schnorr::signature::Verifier;
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
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
        let signing_key = SigningKey::from_bytes(&secret)
            .map_err(|e| NostrError::Crypto(format!("Invalid private key: {e}")))?;
        let verifying_key = signing_key.verifying_key();
        let pubkey_bytes = verifying_key.to_bytes();
        let pubkey_hex = hex::encode(pubkey_bytes);
        Ok(Self {
            signing_key,
            pubkey_hex,
        })
    }

    pub fn pubkey(&self) -> &str {
        &self.pubkey_hex
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

        // Sign the 32-byte digest using BIP-340 Schnorr
        let signature: Signature = self.signing_key.sign(&id_bytes);
        let sig_hex = hex::encode(signature.to_bytes());

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
    let signature = match Signature::try_from(sig_bytes.as_slice()) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };

    Ok(verifying_key.verify(&id_bytes, &signature).is_ok())
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
