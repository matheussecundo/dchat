use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
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
    rand::thread_rng().fill_bytes(&mut key);
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
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LENGTH];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

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
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes = URL_SAFE_NO_PAD.decode(&payload.nonce)?;
    if nonce_bytes.len() != NONCE_LENGTH {
        return Err(CryptoError::DecryptionFailed);
    }
    let nonce = Nonce::from_slice(&nonce_bytes);
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
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
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
}
