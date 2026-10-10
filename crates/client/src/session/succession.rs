//! Admin succession (`protocol::succession`): while an admin is present, one of them keeps
//! the admin secret sealed with the room's longest-present other member (the heir). When no
//! admin has been in the room for a while, the heir takes over: the secret goes into its
//! link, as in the creator's admin link, and it signs an admin proof like any admin. Admins
//! can also make a member an admin at once (Make admin).

use super::RoomSession;
use crate::state::{current_fragment, replace_fragment};
use protocol::succession::GrantOutcome;
use protocol::{admin_proof_message, open_handover, seal_json, EncryptedPayload, HandoverContent, Member, NostrBurnerKey, RoomBody, RoomEnvelope};
use std::rc::Rc;

impl RoomSession {
    /// The members present, us included, as the roster holds them.
    fn present_members(&self) -> Vec<Member> {
        let present = self.inner.present.borrow();
        self.inner.roster.borrow().members().filter(|m| present.contains(&m.pubkey)).cloned().collect()
    }

    /// Check the heir once the current message is handled. `recompute` runs while we apply
    /// our own messages, before they are sent: a handover sent from there would overtake the
    /// Hello that makes us an admin, and members would ignore it.
    pub(super) fn schedule_heir_check(&self) {
        if self.inner.heir_check_queued.replace(true) {
            return;
        }
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            s.inner.heir_check_queued.set(false);
            s.maintain_heir();
        });
    }

    /// Hand the secret to the heir when we are the admin that keeps it and the heir changed.
    pub(super) fn maintain_heir(&self) {
        if self.inner.closed.get() || self.inner.rekeying.get() {
            return;
        }
        let present = self.present_members();
        let present: Vec<&Member> = present.iter().collect();
        let away = self.away_members();
        let away: Vec<&Member> = away.iter().collect();
        let heir = self.inner.succession.borrow_mut().update(&self.inner.me, &present, &away, js_sys::Date::now() as u64);
        if let Some(heir) = heir {
            if !self.hand_over(&heir, false) {
                self.inner.succession.borrow_mut().forget_handover();
            }
        }
    }

    /// Seal the admin secret, with our seniority order, to `to` and gossip it.
    fn hand_over(&self, to: &str, promote: bool) -> bool {
        let Some(admin) = self.inner.admin_key.borrow().clone() else {
            return false;
        };
        // Away members keep their rank: they may be back.
        let away = self.inner.absences.borrow().away();
        let around: Vec<String> =
            self.inner.present.borrow().iter().chain(&away).filter(|p| **p != self.inner.me).cloned().collect();
        let ranking = self.inner.succession.borrow().seen.order(around.iter().map(String::as_str));
        let content = HandoverContent { admsk: admin.secret_hex(), ranking };
        let Ok(sealed) = seal_json(&self.inner.identity, to, &content) else {
            return false;
        };
        self.publish(RoomBody::AdminHandover { promote, sealed }).is_some()
    }

    /// Ends the hold after a rekey, and lets the heir take over once no admin is left.
    pub(super) fn succession_tick(&self) {
        if self.inner.closed.get() || self.inner.rekeying.get() {
            return;
        }
        self.maintain_heir();
        if self.is_admin() {
            return;
        }
        let now = js_sys::Date::now() as u64;
        let secret = {
            let mut succession = self.inner.succession.borrow_mut();
            succession.due(now).then(|| succession.take_dormant()).flatten()
        };
        if let Some(secret) = secret {
            log::info!("No admin in the room for a while: taking over as admin");
            self.become_admin(&secret);
        }
    }

    pub(super) fn on_admin_handover(&self, envelope: &RoomEnvelope, promote: bool, sealed: &EncryptedPayload) {
        let author = envelope.author.as_str();
        if author == self.inner.me {
            return;
        }
        let by_admin = self.inner.roster.borrow().get(author).is_some_and(|m| m.is_admin);
        if !by_admin {
            log::warn!("Ignoring an admin handover from a non-admin member");
            return;
        }
        if self.is_admin() {
            return;
        }
        let content = open_handover(&self.inner.identity, author, sealed, self.adm());
        if promote {
            if let Some(content) = content {
                self.inner.succession.borrow_mut().seen.inherit(content.ranking);
                self.become_admin(&content.admsk);
            }
            return;
        }
        let (secret, ranking) = match content {
            Some(content) => (Some(content.admsk), content.ranking),
            None => (None, Vec::new()),
        };
        let mut succession = self.inner.succession.borrow_mut();
        if succession.on_heir_grant(envelope.ts, &envelope.id, secret) == GrantOutcome::Kept {
            succession.seen.inherit(ranking);
        }
    }

    /// Become an admin with the room's admin secret: it goes into our link (so a reload keeps
    /// it, and 🔑 Copy Admin Link shares it), and a new Hello carries our admin proof.
    fn become_admin(&self, secret: &str) {
        if self.inner.closed.get() || self.inner.rekeying.get() || self.is_admin() {
            return;
        }
        let Ok(admin) = NostrBurnerKey::from_secret_hex(secret) else {
            return;
        };
        if Some(admin.pubkey()) != self.inner.params.admin_pubkey.as_deref() {
            return;
        }
        let Ok(proof) = admin.sign_message(&admin_proof_message(&self.inner.room_id, &self.inner.me)) else {
            return;
        };
        let mut fragment = current_fragment();
        fragment.set("admsk", secret);
        replace_fragment(&fragment);
        *self.inner.admin_key.borrow_mut() = Some(Rc::new(admin));
        self.inner.succession.borrow_mut().clear_dormant();
        let hello = self.inner.hellos.borrow().get(&self.inner.me).map(|e| e.body.clone());
        if let Some(RoomBody::Hello { name, join_ts, .. }) = hello {
            self.publish(RoomBody::Hello { name, join_ts, admin_proof: Some(proof) });
        }
        self.toast("toast_now_admin");
    }

    /// Make `target` an admin now (admins only). It can't be undone: admins can't kick admins,
    /// and the secret can't be taken back.
    pub fn make_admin(&self, target: &str) {
        if !self.is_admin() || self.inner.rekeying.get() || target == self.inner.me {
            return;
        }
        let eligible = self.inner.present.borrow().contains(target)
            && self.inner.roster.borrow().get(target).is_some_and(|m| !m.is_admin);
        if eligible {
            self.hand_over(target, true);
        }
    }
}
