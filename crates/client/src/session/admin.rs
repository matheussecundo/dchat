//! Admin moderation: kick a member or rotate the invite link. Both "rekey" the room:
//! a new room ID and key are sealed to each remaining member's session key (ECDH) in one
//! gossiped, admin-signed envelope; every recipient moves to the new room, the kicked
//! member (and anyone holding only the old link) stays behind.
//!
//! Members away at that moment (a phone that switched apps) get a grant too. They can't
//! receive the envelope, so members who move keep listening on the old room's relays while
//! those absences may last, and hand it to them when they announce themselves there
//! (`SignalPayload::RekeyForward`, sealed to them).

use super::RoomSession;
use crate::names::pubkey_tag;
use crate::state::{current_fragment, replace_fragment, RekeyTarget, SessionCarry};
use leptos::*;
use protocol::{
    generate_key, generate_room_id, key_from_base64, key_to_base64, RoomBody, RoomEnvelope, RoomGrant, SealedGrant,
    SignalPayload,
};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Give the rekey envelope time to reach (and be relayed to) everyone before the links
/// to the old room close.
const REKEY_DELAY_MS: u64 = 1500;
/// A member back on the old topic is answered after a random delay up to this long, so the
/// members who moved don't all answer at once (the first answer is enough).
const FORWARD_JITTER_MS: f64 = 1500.0;
/// Answer the same member again only after this long (an answer can be lost).
const FORWARD_AGAIN_MS: f64 = 10_000.0;

/// A rekey we followed, kept for the members who were away when it happened.
pub(super) struct Forwarding {
    envelope: Rc<RoomEnvelope>,
    /// Members away when we followed it: their grant's holders, and the kicked member.
    to: HashSet<String>,
    /// When we last answered each of them.
    answered: HashMap<String, f64>,
    /// Stop listening on the old topic then.
    until: f64,
}

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
        // Away members too: they get the envelope once back (`forward_rekey`).
        let away = self.inner.absences.borrow().away();
        let recipients: Vec<String> = self
            .inner
            .present
            .borrow()
            .iter()
            .chain(&away)
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

    pub(super) fn on_admin_rekey(&self, envelope: &RoomEnvelope, kicked: Option<&str>, grants: &[SealedGrant]) {
        let author = envelope.author.as_str();
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
        self.keep_for_away_members(envelope, kicked, grants);
        let rejoin_voice = self.my_voice().in_voice;
        let s = self.clone();
        set_timeout(
            move || {
                let carry = SessionCarry {
                    identity: s.inner.identity.clone(),
                    shared_files: s.shared_files(),
                    first_seen: s.inner.succession.borrow().seen.clone(),
                };
                s.leave();
                let mut params = current_fragment();
                params.set("room", &grant.room);
                params.set("key", &grant.key);
                replace_fragment(&params);
                s.inner.signals.rekey.set(Some(RekeyTarget {
                    room: grant.room,
                    key,
                    rejoin_voice,
                    carry,
                }));
            },
            std::time::Duration::from_millis(REKEY_DELAY_MS),
        );
    }

    /// We follow `envelope`: remember it for the members away now that it moves too (or
    /// names kicked), to hand it to them on the old topic (`forward_rekey`).
    fn keep_for_away_members(&self, envelope: &RoomEnvelope, kicked: Option<&str>, grants: &[SealedGrant]) {
        let absences = self.inner.absences.borrow();
        let to: HashSet<String> = grants
            .iter()
            .map(|g| g.to.as_str())
            .chain(kicked)
            .filter(|pk| absences.is_away(pk))
            .map(str::to_string)
            .collect();
        let until = absences.last_expiry_of(to.iter().map(String::as_str));
        drop(absences);
        let Some(until) = until else {
            return;
        };
        log::info!("{} member(s) away during the rekey: listening on the old room for them", to.len());
        *self.inner.forwarding.borrow_mut() =
            Some(Forwarding { envelope: Rc::new(envelope.clone()), to, answered: HashMap::new(), until: until as f64 });
    }

    /// The session left: close its relay connections, or keep them until the last member
    /// away during a rekey we followed could come back.
    pub(super) fn close_pool_after_forwarding(&self, pool: Rc<crate::nostr_pool::NostrRelayPool>) {
        let until = self.inner.forwarding.borrow().as_ref().map(|f| f.until);
        let Some(until) = until.filter(|until| *until > js_sys::Date::now()) else {
            pool.close();
            return;
        };
        let s = self.clone();
        set_timeout(
            move || {
                pool.close();
                s.inner.forwarding.borrow_mut().take();
            },
            std::time::Duration::from_millis((until - js_sys::Date::now()).max(0.0) as u64),
        );
    }

    /// `from` announced itself on the old topic after we moved: if it was away during the
    /// rekey, hand it the admin's envelope (sealed to it), once in a while.
    pub(super) fn forward_rekey(&self, from: &str) {
        let now = js_sys::Date::now();
        let envelope = {
            let mut forwarding = self.inner.forwarding.borrow_mut();
            let Some(forwarding) = forwarding.as_mut().filter(|f| f.to.contains(from) && now < f.until) else {
                return;
            };
            if forwarding.answered.get(from).is_some_and(|at| now - at < FORWARD_AGAIN_MS) {
                return;
            }
            forwarding.answered.insert(from.to_string(), now);
            forwarding.envelope.clone()
        };
        let Some(pool) = self.pool() else {
            return;
        };
        let signal = SignalPayload::RekeyForward { to: from.to_string(), envelope: Box::new((*envelope).clone()) };
        let from = from.to_string();
        set_timeout(
            move || {
                log::info!("Handing the rekey to {}, who was away", pubkey_tag(&from));
                pool.broadcast_signal(&signal);
            },
            std::time::Duration::from_millis((js_sys::Math::random() * FORWARD_JITTER_MS) as u64),
        );
    }

    /// A member who moved hands us the rekey we missed while away: the admin's signed
    /// envelope, checked as if it had come through the room.
    pub(super) fn on_rekey_forward(&self, envelope: &RoomEnvelope) {
        if self.inner.rekeying.get() || !envelope.verify(self.adm()) {
            return;
        }
        let RoomBody::AdminRekey { kicked, grants } = &envelope.body else {
            return;
        };
        if !self.inner.dedup.borrow_mut().insert(&envelope.id) {
            return;
        }
        log::info!("The room moved while we were away: following");
        self.on_admin_rekey(envelope, kicked.as_deref(), grants);
    }
}
