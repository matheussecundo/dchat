use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const KEY_LENGTH: usize = 32;
pub const NONCE_LENGTH: usize = 12;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Failed to decode base64: {0}")]
    Base64Decode(#[from] base64::DecodeError),
    #[error("Invalid key length: expected {expected}, got {got}")]
    InvalidKeyLength { expected: usize, got: usize },
    #[error("Encryption failure")]
    EncryptionFailed,
    #[error("Decryption failure (authentication tag mismatch or corrupted data)")]
    DecryptionFailed,
    #[error("Serialization failure: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Encrypted payload with URL-safe base64 encoded nonce and ciphertext.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedPayload {
    pub nonce: String,
    pub ciphertext: String,
}

/// Generate a cryptographically secure 256-bit symmetric key.
pub fn generate_key() -> [u8; KEY_LENGTH] {
    let mut key = [0u8; KEY_LENGTH];
    rand::rng().fill_bytes(&mut key);
    key
}

/// Encode 32-byte key to a URL-safe Base64 string suitable for URL hash fragment.
pub fn key_to_base64(key: &[u8; KEY_LENGTH]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

/// Decode 32-byte key from a URL-safe Base64 string.
pub fn key_from_base64(s: &str) -> Result<[u8; KEY_LENGTH], CryptoError> {
    let bytes = URL_SAFE_NO_PAD.decode(s)?;
    if bytes.len() != KEY_LENGTH {
        return Err(CryptoError::InvalidKeyLength {
            expected: KEY_LENGTH,
            got: bytes.len(),
        });
    }
    let mut key = [0u8; KEY_LENGTH];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Encrypt raw bytes with ChaCha20-Poly1305.
pub fn encrypt_bytes(key: &[u8; KEY_LENGTH], plaintext: &[u8]) -> Result<EncryptedPayload, CryptoError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LENGTH];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce: &Nonce = (&nonce_bytes).into();

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| CryptoError::EncryptionFailed)?;

    Ok(EncryptedPayload {
        nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

/// Decrypt an EncryptedPayload with ChaCha20-Poly1305.
pub fn decrypt_bytes(key: &[u8; KEY_LENGTH], payload: &EncryptedPayload) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce_bytes = URL_SAFE_NO_PAD.decode(&payload.nonce)?;
    let nonce = <&Nonce>::try_from(nonce_bytes.as_slice()).map_err(|_| CryptoError::DecryptionFailed)?;
    let ciphertext = URL_SAFE_NO_PAD.decode(&payload.ciphertext)?;

    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| CryptoError::DecryptionFailed)
}

/// Convenience method to serialize and encrypt any type implementing `Serialize`.
pub fn encrypt_json<T: Serialize>(key: &[u8; KEY_LENGTH], val: &T) -> Result<EncryptedPayload, CryptoError> {
    let json_bytes = serde_json::to_vec(val)?;
    encrypt_bytes(key, &json_bytes)
}

/// Convenience method to decrypt and deserialize any type implementing `DeserializeOwned`.
pub fn decrypt_json<T: serde::de::DeserializeOwned>(
    key: &[u8; KEY_LENGTH],
    payload: &EncryptedPayload,
) -> Result<T, CryptoError> {
    let bytes = decrypt_bytes(key, payload)?;
    let val = serde_json::from_slice(&bytes)?;
    Ok(val)
}

/// Generate a short random alphanumeric room identifier.
pub fn generate_room_id() -> String {
    use rand::RngExt;
    const CHARSET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..8)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

pub const CHUNK_SIZE: usize = 64 * 1024; // 64 KB optimal for SCTP data channel
pub const CHUNK_HEADER_SIZE: usize = 16 + 4 + 4 + NONCE_LENGTH; // 36 bytes

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHeader {
    pub file_id: [u8; 16],
    pub chunk_index: u32,
    pub total_chunks: u32,
}

/// Encrypt a chunk of a file for P2P binary transfer over RTCDataChannel.
/// Packet format:
/// [16-byte file_id][4-byte chunk_index][4-byte total_chunks][12-byte nonce][ciphertext + 16-byte Poly1305 tag]
/// The first 24 bytes (file_id + chunk_index + total_chunks) are also passed as AEAD AAD (Associated Data).
pub fn encrypt_chunk(
    key: &[u8; KEY_LENGTH],
    file_id: &[u8; 16],
    chunk_index: u32,
    total_chunks: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LENGTH];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce: &Nonce = (&nonce_bytes).into();

    let mut aad = [0u8; 24];
    aad[0..16].copy_from_slice(file_id);
    aad[16..20].copy_from_slice(&chunk_index.to_be_bytes());
    aad[20..24].copy_from_slice(&total_chunks.to_be_bytes());

    let payload = chacha20poly1305::aead::Payload {
        msg: plaintext,
        aad: &aad,
    };

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|_| CryptoError::EncryptionFailed)?;

    let mut packet = Vec::with_capacity(CHUNK_HEADER_SIZE + ciphertext.len());
    packet.extend_from_slice(file_id);
    packet.extend_from_slice(&chunk_index.to_be_bytes());
    packet.extend_from_slice(&total_chunks.to_be_bytes());
    packet.extend_from_slice(&nonce_bytes);
    packet.extend_from_slice(&ciphertext);

    Ok(packet)
}

/// Decrypt a binary chunk packet received over RTCDataChannel.
/// Validates chunk header, AEAD integrity (header + ciphertext), and decrypts payload.
pub fn decrypt_chunk(
    key: &[u8; KEY_LENGTH],
    packet: &[u8],
) -> Result<(ChunkHeader, Vec<u8>), CryptoError> {
    if packet.len() < CHUNK_HEADER_SIZE + 16 {
        return Err(CryptoError::DecryptionFailed);
    }

    let mut file_id = [0u8; 16];
    file_id.copy_from_slice(&packet[0..16]);
    let chunk_index = u32::from_be_bytes(packet[16..20].try_into().unwrap());
    let total_chunks = u32::from_be_bytes(packet[20..24].try_into().unwrap());

    let nonce = <&Nonce>::try_from(&packet[24..36]).map_err(|_| CryptoError::DecryptionFailed)?;
    let ciphertext = &packet[36..];

    let aad = &packet[0..24];
    let payload = chacha20poly1305::aead::Payload {
        msg: ciphertext,
        aad,
    };

    let cipher = ChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(nonce, payload)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    Ok((
        ChunkHeader {
            file_id,
            chunk_index,
            total_chunks,
        },
        plaintext,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_generation_and_base64() {
        let key = generate_key();
        let encoded = key_to_base64(&key);
        let decoded = key_from_base64(&encoded).expect("Decoded key");
        assert_eq!(key, decoded);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = generate_key();
        let plaintext = b"Hello, ephemeral world!";
        let payload = encrypt_bytes(&key, plaintext).expect("Encryption");
        let decrypted = decrypt_bytes(&key, &payload).expect("Decryption");
        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_tampered_payload_fails() {
        let key = generate_key();
        let plaintext = b"Secret message";
        let mut payload = encrypt_bytes(&key, plaintext).expect("Encryption");
        // Tamper with ciphertext
        payload.ciphertext.push('a');
        let res = decrypt_bytes(&key, &payload);
        assert!(res.is_err());
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let plaintext = b"Top secret";
        let payload = encrypt_bytes(&key1, plaintext).expect("Encryption");
        let res = decrypt_bytes(&key2, &payload);
        assert!(res.is_err());
    }

    #[test]
    fn test_json_roundtrip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct TestData {
            msg: String,
            code: u32,
        }

        let key = generate_key();
        let data = TestData {
            msg: "test payload".into(),
            code: 42,
        };
        let encrypted = encrypt_json(&key, &data).expect("Encrypt json");
        let decrypted: TestData = decrypt_json(&key, &encrypted).expect("Decrypt json");
        assert_eq!(data, decrypted);
    }

    #[test]
    fn test_chunk_encryption_roundtrip() {
        let key = generate_key();
        let file_id = [42u8; 16];
        let chunk_index = 3;
        let total_chunks = 10;
        let plaintext = b"This is a chunk of a shared file over WebRTC RTCDataChannel";

        let packet = encrypt_chunk(&key, &file_id, chunk_index, total_chunks, plaintext)
            .expect("Encrypt chunk");
        assert_eq!(packet.len(), CHUNK_HEADER_SIZE + plaintext.len() + 16);

        let (header, decrypted) = decrypt_chunk(&key, &packet).expect("Decrypt chunk");
        assert_eq!(header.file_id, file_id);
        assert_eq!(header.chunk_index, chunk_index);
        assert_eq!(header.total_chunks, total_chunks);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_tampered_chunk_payload_fails() {
        let key = generate_key();
        let file_id = [7u8; 16];
        let mut packet = encrypt_chunk(&key, &file_id, 0, 1, b"Valid data").expect("Encrypt");
        // Tamper last byte of ciphertext
        let last_idx = packet.len() - 1;
        packet[last_idx] ^= 0xFF;
        let res = decrypt_chunk(&key, &packet);
        assert!(res.is_err());
    }

    #[test]
    fn test_tampered_chunk_header_fails_aead() {
        let key = generate_key();
        let file_id = [9u8; 16];
        let mut packet = encrypt_chunk(&key, &file_id, 0, 5, b"Chunk data").expect("Encrypt");
        // Tamper chunk_index in header (bytes 16..20)
        packet[19] = 1; // changed chunk_index from 0 to 1
        let res = decrypt_chunk(&key, &packet);
        assert!(res.is_err(), "Header tampering must be detected by AEAD AAD");
    }

    #[test]
    fn test_wrong_key_chunk_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let file_id = [1u8; 16];
        let packet = encrypt_chunk(&key1, &file_id, 0, 1, b"Secret chunk").expect("Encrypt");
        let res = decrypt_chunk(&key2, &packet);
        assert!(res.is_err());
    }
}
