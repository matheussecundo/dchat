//! Optional room passwords. The room key is derived from the key in the link *and* a
//! password shared separately, so a link that leaks (chat apps, browser history or sync)
//! does not open the room on its own. The link carries only a random salt (`pw=`).

use crate::crypto::{CryptoError, KEY_LENGTH};
use crate::nostr::hash_room_topic;
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const PASSWORD_SALT_LENGTH: usize = 16;
/// Argon2id at the OWASP minimum (19 MiB, 2 passes, 1 lane): memory-hard, so guessing a
/// password costs every attempt the same, while one derivation takes well under a second.
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_PASSES: u32 = 2;

/// A fresh salt for a new password room, encoded as it goes in the link (`pw=`).
pub fn generate_password_salt() -> String {
    let mut salt = [0u8; PASSWORD_SALT_LENGTH];
    rand::thread_rng().fill_bytes(&mut salt);
    URL_SAFE_NO_PAD.encode(salt)
}

/// The slow part, done once per tab: Argon2id of the password (surrounding spaces ignored,
/// as phone keyboards add them). The result stays in RAM and survives a room rekey, which
/// only changes the link key.
pub fn stretch_password(password: &str, salt: &[u8]) -> Result<[u8; KEY_LENGTH], CryptoError> {
    let params = Params::new(ARGON2_MEMORY_KIB, ARGON2_PASSES, 1, Some(KEY_LENGTH))
        .map_err(|_| CryptoError::EncryptionFailed)?;
    let mut out = [0u8; KEY_LENGTH];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.trim().as_bytes(), salt, &mut out)
        .map_err(|_| CryptoError::EncryptionFailed)?;
    Ok(out)
}

/// The room key of a password room: the link key bound to the stretched password.
pub fn password_room_key(link_key: &[u8; KEY_LENGTH], stretched: &[u8; KEY_LENGTH]) -> [u8; KEY_LENGTH] {
    let mut hasher = Sha256::new();
    hasher.update(b"dchat:password-room-key:");
    hasher.update(link_key);
    hasher.update(stretched);
    hasher.finalize().into()
}

/// Relay topic of a password room. It depends on the derived key, so someone with the link
/// but not the password cannot even find the room's relay traffic.
pub fn password_room_topic(room_id: &str, room_key: &[u8; KEY_LENGTH]) -> String {
    let tag = hex::encode(Sha256::digest([b"dchat:password-topic:".as_slice(), room_key].concat()));
    hash_room_topic(&format!("{room_id}:{tag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_rooms_need_the_link_and_the_password() {
        let salt = [7u8; PASSWORD_SALT_LENGTH];
        let link_key = [1u8; KEY_LENGTH];
        let stretched = stretch_password("correct horse", &salt).unwrap();
        let key = password_room_key(&link_key, &stretched);

        // Same password (spaces a phone keyboard adds don't matter): same room.
        assert_eq!(password_room_key(&link_key, &stretch_password(" correct horse ", &salt).unwrap()), key);
        // A wrong password, another salt or another link key: another key and topic.
        let wrong = password_room_key(&link_key, &stretch_password("correct hors", &salt).unwrap());
        assert_ne!(wrong, key);
        assert_ne!(password_room_key(&link_key, &stretch_password("correct horse", &[8; 16]).unwrap()), key);
        assert_ne!(password_room_key(&[2; KEY_LENGTH], &stretched), key);
        assert_ne!(password_room_topic("room", &wrong), password_room_topic("room", &key));
        // Neither the link key nor the room ID alone leads to the password room's topic.
        assert_ne!(password_room_topic("room", &key), hash_room_topic("room"));
        assert_ne!(key, link_key);
    }

    #[test]
    fn test_generated_salts_parse_from_the_link() {
        let salt = generate_password_salt();
        let fragment = crate::fragment::FragmentParams::parse(&format!("#room=r&pw={salt}"));
        let parsed = crate::room::RoomParams::from_fragment(&fragment).password_salt.unwrap();
        assert_eq!(parsed.len(), PASSWORD_SALT_LENGTH);
        assert_ne!(generate_password_salt(), salt);
    }
}
