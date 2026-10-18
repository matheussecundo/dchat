//! When this session is on the relays (`protocol::relay_role`): only the room's two
//! introducers stay; any other member joins them only while it has no open link, shakes
//! hands with someone the mesh can't reach, or answers an introducer's `Knock`. The pool is
//! opened and closed as that changes (every pool signs with a fresh relay key).
//!
//! It also holds what only members on the relays can see and the rest must hear over the
//! mesh: a newer protocol version (`NewerVersion`), and a newcomer that couldn't link with
//! the introducers (`Knock`). After a rekey, the old room's pool (kept for members who were
//! away, `admin::Forwarding`) follows our introducer role in the new room.

use super::RoomSession;
use crate::names::pubkey_tag;
use crate::nostr_pool::{protocol_version, NostrRelayPool, PoolEvents};
use leptos::prelude::*;
use protocol::relay_role::{self, BeaconRole, KNOCK_AGAIN_MS, KNOCK_WINDOW_MS};
use protocol::{RoomBody, RoomEnvelope};

/// Longest pubkey accepted in a `Knock`.
const MAX_PUBKEY_CHARS: usize = 128;

impl RoomSession {
    /// The room's introducers as we see it (present admins, then the lowest pubkeys).
    pub(super) fn introducers(&self) -> Vec<String> {
        let present = self.inner.present.borrow();
        relay_role::introducers(self.inner.roster.borrow().members(), &present)
    }

    /// Join or leave the relays as our role says, after `recompute` and on every tick.
    pub(super) fn update_relay_role(&self) {
        if self.inner.closed.get() {
            return;
        }
        let now = js_sys::Date::now();
        let introducer = self.introducers().contains(&self.inner.me);
        let wants = introducer || self.needs_relays(now);
        // Becoming an introducer in a linked room (the previous one left, or the room split):
        // announce ourselves at once, so a split room finds itself again.
        if introducer && !self.inner.introducer.get() && self.has_open_link() {
            self.inner.introducer_fast_until.set(now + relay_role::FAST_BEACON_FOR_MS as f64);
        }
        self.inner.introducer.set(introducer);
        let on = self.inner.relay_role.borrow_mut().update(wants, now as u64);
        match (on, self.pool().is_some()) {
            (true, false) => self.open_pool(true),
            (false, true) => {
                log::info!("Leaving the relays: the room's introducers let newcomers in");
                self.close_pool();
            }
            _ => {}
        }
        self.update_forwarders(introducer, now);
    }

    /// Reasons a member that isn't an introducer needs the relays.
    fn needs_relays(&self, now: f64) -> bool {
        let links = self.inner.links.borrow();
        let present = self.inner.present.borrow();
        let open = |pk: &str| links.get(pk).is_some_and(|l| l.is_open());
        let no_link = !links.values().any(|l| l.is_open());
        // A handshake the mesh can't carry: with someone it doesn't reach.
        let off_mesh = links.values().any(|l| !l.is_open() && !present.contains(&l.remote));
        let knocked = self.inner.knocks.borrow().iter().any(|(who, until)| *until > now && !open(who));
        no_link || off_mesh || knocked
    }

    /// The role we beacon with on the relays, and when its fast window ends.
    pub(super) fn beacon_role(&self) -> (BeaconRole, f64) {
        if !self.has_open_link() {
            (BeaconRole::Unlinked, self.inner.fast_beacon_until.get())
        } else if self.inner.introducer.get() {
            (BeaconRole::Introducer, self.inner.introducer_fast_until.get())
        } else {
            (BeaconRole::Visitor, 0.0)
        }
    }

    /// Connect to the room's relays with a fresh relay key. `announce`: say `Presence` on
    /// every connection (not for a pool kept only to hand a rekey to members who were away).
    pub(super) fn open_pool(&self, announce: bool) {
        let generation = self.inner.pool_generation.get().wrapping_add(1);
        self.inner.pool_generation.set(generation);
        let current = {
            let s = self.clone();
            move || s.inner.pool_generation.get() == generation
        };
        let events = PoolEvents {
            on_signal: {
                let s = self.clone();
                let current = current.clone();
                Box::new(move |from, signal| {
                    if current() {
                        s.on_relay_signal(from, signal);
                    }
                })
            },
            on_other_version: {
                let s = self.clone();
                let current = current.clone();
                Box::new(move |from, version| {
                    if current() {
                        s.on_other_version(&from, version);
                    }
                })
            },
            on_relay_connected: {
                let s = self.clone();
                Box::new(move |count| {
                    // A pool kept open after a rekey (`forward_rekey`) must not touch the UI,
                    // which shows the new session; nor may one already replaced.
                    if s.inner.closed.get() || !current() {
                        return;
                    }
                    s.inner.relays_connected.set(count);
                    s.inner.signals.connected_relays.set(count);
                    s.refresh_status();
                })
            },
        };
        let inner = &self.inner;
        match NostrRelayPool::new(inner.topic.clone(), inner.key, inner.identity.clone(), inner.relay_urls.clone(), events, announce) {
            Ok(pool) => {
                *inner.pool.borrow_mut() = Some(pool);
                if !inner.closed.get() {
                    inner.signals.relay_standby.set(false);
                }
            }
            Err(err) => log::error!("Cannot open the relay pool: {err}"),
        }
    }

    /// Disconnect from the relays (queued events still go out first).
    pub(super) fn close_pool(&self) {
        let pool = self.inner.pool.borrow_mut().take();
        if let Some(pool) = pool {
            pool.close();
        }
        self.inner.pool_generation.set(self.inner.pool_generation.get().wrapping_add(1));
        self.inner.relays_connected.set(0);
        if !self.inner.closed.get() {
            self.inner.signals.connected_relays.set(0);
            self.inner.signals.relay_standby.set(true);
            self.refresh_status();
        }
    }

    /// As an introducer, `who` announced itself again after our link with it failed: ask the
    /// room's other members to try, through the relays (at most every `KNOCK_AGAIN_MS`).
    pub(super) fn knock_for(&self, who: &str, now: f64) {
        let mut knocked = self.inner.knocked.borrow_mut();
        if knocked.get(who).is_some_and(|at| now - at < KNOCK_AGAIN_MS as f64) {
            return;
        }
        knocked.insert(who.to_string(), now);
        drop(knocked);
        log::info!("{} can't link with us: asking the room to try", pubkey_tag(who));
        self.publish(RoomBody::Knock { who: who.to_string() });
    }

    /// An introducer knocked for `who`: join the relays a while, unless we are linked with it.
    pub(super) fn on_knock(&self, author: &str, who: &str) {
        if author == self.inner.me || who == self.inner.me || who.len() > MAX_PUBKEY_CHARS {
            return;
        }
        if !self.introducers().iter().any(|i| i == author) {
            log::warn!("Ignoring a knock from a member who isn't an introducer");
            return;
        }
        if self.link(who).is_some_and(|l| l.is_open()) {
            return;
        }
        let until = js_sys::Date::now() + KNOCK_WINDOW_MS as f64;
        self.inner.knocks.borrow_mut().insert(who.to_string(), until);
        self.update_relay_role();
    }

    /// Someone on the relays runs protocol `version`, newer than ours: tell the room once, so
    /// members who aren't on the relays show the reload banner too.
    pub(super) fn tell_newer_version(&self, version: u32) {
        let known = self.inner.newer_version.borrow().as_ref().map(newer_version_of);
        if known.is_some_and(|v| v >= version) {
            return;
        }
        self.publish(RoomBody::NewerVersion { v: version });
    }

    pub(super) fn on_newer_version(&self, envelope: &RoomEnvelope, version: u32) {
        if version <= protocol_version() {
            return;
        }
        let known = self.inner.newer_version.borrow().as_ref().map(newer_version_of);
        if known.is_none_or(|v| v < version) {
            *self.inner.newer_version.borrow_mut() = Some(envelope.clone());
        }
        self.inner.signals.update_required.set(true);
    }

    /// The old rooms' pools we keep for members who were away during a rekey: open while we
    /// are an introducer here (others will have closed theirs), closed otherwise, gone once
    /// those absences have expired. Left alone right after the move, while members come back.
    fn update_forwarders(&self, introducer: bool, now: f64) {
        if now < self.inner.forwarders_hold_until {
            self.inner.forwarders.borrow_mut().retain(|old| old.forwarding_active());
            return;
        }
        self.inner.forwarders.borrow_mut().retain(|old| old.set_forwarding(introducer));
    }

    /// Stop every pool kept for members away during an earlier rekey.
    pub(super) fn end_forwarders(&self) {
        for old in self.inner.forwarders.take() {
            old.end_forwarding();
        }
    }
}

fn newer_version_of(envelope: &RoomEnvelope) -> u32 {
    match envelope.body {
        RoomBody::NewerVersion { v } => v,
        _ => 0,
    }
}
