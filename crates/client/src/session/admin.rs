//! Admin moderation: kick a member or rotate the invite link. Both "rekey" the room:
//! a new room ID and key are sealed to each remaining member's session key (ECDH) in one
//! gossiped, admin-signed envelope; every recipient moves to the new room, the kicked
//! member (and anyone holding only the old link) stays behind.

use super::RoomSession;
use crate::state::{current_fragment, replace_fragment, RekeyTarget};
use leptos::*;
use protocol::{generate_key, generate_room_id, key_from_base64, key_to_base64, RoomBody, RoomGrant, SealedGrant};

/// Give the rekey envelope time to reach (and be relayed to) everyone before the links
/// to the old room close.
const REKEY_DELAY_MS: u64 = 1500;

impl RoomSession {
    pub fn is_admin(&self) -> bool {
        self.inner.roster.borrow().get(&self.inner.me).is_some_and(|m| m.is_admin)
    }

    /// Remove `target` from the room: everyone else moves to a new room link.
    pub fn kick(&self, target: &str) {
        let target_is_admin = self.inner.roster.borrow().get(target).is_some_and(|m| m.is_admin);
        if target == self.inner.me || target_is_admin {
            return;
        }
        self.rekey(Some(target.to_string()));
    }

    /// Move everyone present to a new room link; the old invite stops working.
    pub fn rotate_link(&self) {
        self.rekey(None);
    }

    fn rekey(&self, kicked: Option<String>) {
        if !self.is_admin() || self.inner.closed.get() || self.inner.rekeying.get() {
            return;
        }
        let grant = RoomGrant {
            room: generate_room_id(),
            key: key_to_base64(&generate_key()),
        };
        let recipients: Vec<String> = self
            .inner
            .present
            .borrow()
            .iter()
            .filter(|pk| Some(*pk) != kicked.as_ref())
            .cloned()
            .collect();
        // Our own grant is included, so the admin moves through the same path as everyone.
        let grants = recipients
            .iter()
            .filter_map(|pk| SealedGrant::seal(&self.inner.identity, pk, &grant).ok())
            .collect();
        self.publish(RoomBody::AdminRekey { kicked, grants });
    }

    pub(super) fn on_admin_rekey(&self, author: &str, kicked: Option<&str>, grants: &[SealedGrant]) {
        let by_admin = self.inner.roster.borrow().get(author).is_some_and(|m| m.is_admin);
        if !by_admin {
            log::warn!("Ignoring rekey from a non-admin member");
            return;
        }
        if self.inner.rekeying.get() {
            return;
        }
        if kicked == Some(self.inner.me.as_str()) {
            self.inner.signals.removed.set(true);
            self.leave();
            return;
        }
        let Some(grant) = grants
            .iter()
            .find(|g| g.to == self.inner.me)
            .and_then(|g| g.open(&self.inner.identity, author))
        else {
            return;
        };
        let Ok(key) = key_from_base64(&grant.key) else {
            return;
        };
        self.inner.rekeying.set(true);
        let rejoin_voice = self.my_voice().in_voice;
        let s = self.clone();
        set_timeout(
            move || {
                s.leave();
                let mut params = current_fragment();
                params.set("room", &grant.room);
                params.set("key", &grant.key);
                replace_fragment(&params);
                s.inner.signals.rekey.set(Some(RekeyTarget {
                    room: grant.room,
                    key,
                    rejoin_voice,
                }));
            },
            std::time::Duration::from_millis(REKEY_DELAY_MS),
        );
    }
}
