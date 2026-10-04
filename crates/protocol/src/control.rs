//! Remote-control permissions, kept by the member who shares their screen. Pure, so every
//! rule is unit-tested: one member at a time holds mouse and keyboard, up to `MAX_PADS`
//! members hold a controller slot each, and nothing is granted without the sharer's click.

use crate::input::{InputEvent, InputLane, InputSequencer};
use crate::messages::ControlEnd;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_PADS: usize = 4;
pub const MAX_PENDING: usize = 8;
pub const REQUEST_TTL_MS: u64 = 60_000;
pub const DENY_COOLDOWN_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ControlWants {
    pub mouse_keyboard: bool,
    pub controller: bool,
}

impl ControlWants {
    pub fn any(&self) -> bool {
        self.mouse_keyboard || self.controller
    }
}

/// What one member may do right now. `pad` is the controller slot (0 = P1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ControlRights {
    pub mouse_keyboard: bool,
    pub pad: Option<u8>,
}

impl ControlRights {
    pub fn any(&self) -> bool {
        self.mouse_keyboard || self.pad.is_some()
    }
}

/// A member whose rights changed: tell them (`ControlGrant`) and the host app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RightsChange {
    pub member: String,
    pub rights: ControlRights,
    pub reason: Option<ControlEnd>,
}

#[derive(Clone, Debug)]
struct Pending {
    member: String,
    wants: ControlWants,
    at_ms: u64,
}

#[derive(Debug, Default)]
pub struct ControlState {
    available: bool,
    /// Controller slots the host computer can create (0 until the app says).
    pad_slots: usize,
    pending: Vec<Pending>,
    mouse_keyboard: Option<String>,
    pads: [Option<String>; MAX_PADS],
    cooldown_until: HashMap<String, u64>,
}

impl ControlState {
    pub fn available(&self) -> bool {
        self.available
    }

    /// How many virtual controllers the host app can create (at most `MAX_PADS`).
    pub fn set_pad_slots(&mut self, slots: u8) {
        self.pad_slots = (slots as usize).min(MAX_PADS);
    }

    pub fn pad_slots(&self) -> u8 {
        self.pad_slots as u8
    }

    /// Control is offered only while the sharer allows it (host app paired, a whole screen
    /// shared). Turning it off ends every grant and request.
    pub fn set_available(&mut self, on: bool, why: ControlEnd) -> Vec<RightsChange> {
        self.available = on;
        if on {
            return Vec::new();
        }
        let mut changes = self.revoke_all(why);
        for p in std::mem::take(&mut self.pending) {
            if !changes.iter().any(|c| c.member == p.member) {
                changes.push(self.change(&p.member, Some(why)));
            }
        }
        changes
    }

    /// A viewer asks; returns whether the sharer should be prompted. A new request from the
    /// same member replaces the old one.
    pub fn request(&mut self, member: &str, wants: ControlWants, now_ms: u64) -> bool {
        if !self.available || !wants.any() {
            return false;
        }
        if self.cooldown_until.get(member).is_some_and(|until| now_ms < *until) {
            return false;
        }
        let rights = self.rights_of(member);
        let new = (wants.mouse_keyboard && !rights.mouse_keyboard) || (wants.controller && rights.pad.is_none());
        if !new {
            return false;
        }
        self.pending.retain(|p| p.member != member);
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.push(Pending { member: member.to_string(), wants, at_ms: now_ms });
        true
    }

    pub fn pending(&self) -> impl Iterator<Item = (&str, ControlWants)> {
        self.pending.iter().map(|p| (p.member.as_str(), p.wants))
    }

    pub fn is_pending(&self, member: &str) -> bool {
        self.pending.iter().any(|p| p.member == member)
    }

    /// Accept `member`'s request. Mouse and keyboard move to them (the previous holder gets
    /// `TakenOver`); a controller takes the lowest free slot, if any.
    pub fn grant(&mut self, member: &str) -> Vec<RightsChange> {
        let Some(index) = self.pending.iter().position(|p| p.member == member) else {
            return Vec::new();
        };
        let wants = self.pending.remove(index).wants;
        let mut changes = Vec::new();
        if wants.mouse_keyboard {
            if let Some(previous) = self.mouse_keyboard.replace(member.to_string()) {
                if previous != member {
                    changes.push(self.change(&previous, Some(ControlEnd::TakenOver)));
                }
            }
        }
        if wants.controller && self.slot_of(member).is_none() {
            if let Some(slot) = self.pads[..self.pad_slots].iter().position(Option::is_none) {
                self.pads[slot] = Some(member.to_string());
            }
        }
        let rights = self.rights_of(member);
        let reason = (!rights.any()).then_some(ControlEnd::NoSlot);
        changes.push(RightsChange { member: member.to_string(), rights, reason });
        changes
    }

    pub fn deny(&mut self, member: &str, now_ms: u64) -> Vec<RightsChange> {
        let before = self.pending.len();
        self.pending.retain(|p| p.member != member);
        if self.pending.len() == before {
            return Vec::new();
        }
        self.cooldown_until.insert(member.to_string(), now_ms + DENY_COOLDOWN_MS);
        vec![self.change(member, Some(ControlEnd::Denied))]
    }

    /// End everything `member` holds or asked for.
    pub fn revoke(&mut self, member: &str, why: ControlEnd) -> Vec<RightsChange> {
        let had = self.rights_of(member).any() || self.is_pending(member);
        self.pending.retain(|p| p.member != member);
        if self.mouse_keyboard.as_deref() == Some(member) {
            self.mouse_keyboard = None;
        }
        for slot in self.pads.iter_mut() {
            if slot.as_deref() == Some(member) {
                *slot = None;
            }
        }
        if had {
            vec![self.change(member, Some(why))]
        } else {
            Vec::new()
        }
    }

    pub fn revoke_all(&mut self, why: ControlEnd) -> Vec<RightsChange> {
        let mut holders: Vec<String> = self.mouse_keyboard.iter().chain(self.pads.iter().flatten()).cloned().collect();
        holders.sort();
        holders.dedup();
        holders.into_iter().flat_map(|m| self.revoke(&m, why)).collect()
    }

    /// Drop requests the sharer left unanswered for too long.
    pub fn expire(&mut self, now_ms: u64) -> Vec<RightsChange> {
        let (old, keep): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pending).into_iter().partition(|p| now_ms >= p.at_ms + REQUEST_TTL_MS);
        self.pending = keep;
        old.iter().map(|p| self.change(&p.member, Some(ControlEnd::Expired))).collect()
    }

    pub fn rights_of(&self, member: &str) -> ControlRights {
        ControlRights {
            mouse_keyboard: self.mouse_keyboard.as_deref() == Some(member),
            pad: self.slot_of(member),
        }
    }

    pub fn mouse_keyboard_holder(&self) -> Option<&str> {
        self.mouse_keyboard.as_deref()
    }

    pub fn pad_holders(&self) -> Vec<Option<String>> {
        self.pads.to_vec()
    }

    pub fn holders(&self) -> impl Iterator<Item = &str> {
        self.mouse_keyboard.iter().chain(self.pads.iter().flatten()).map(String::as_str)
    }

    fn slot_of(&self, member: &str) -> Option<u8> {
        self.pads.iter().position(|p| p.as_deref() == Some(member)).map(|i| i as u8)
    }

    fn change(&self, member: &str, reason: Option<ControlEnd>) -> RightsChange {
        RightsChange { member: member.to_string(), rights: self.rights_of(member), reason }
    }

    /// Whether `member` may send `ev`: mouse and keyboard events only from the holder,
    /// controller events only from a slot holder, and heartbeats or releases from anyone
    /// holding something.
    pub fn allows(&self, member: &str, ev: &InputEvent) -> bool {
        let rights = self.rights_of(member);
        match ev {
            InputEvent::Pad { .. } | InputEvent::PadGone { .. } => rights.pad.is_some(),
            InputEvent::Alive | InputEvent::ReleaseAll => rights.any(),
            _ => ev.is_mouse_keyboard() && rights.mouse_keyboard,
        }
    }
}

/// The sharer's filter between the WebRTC input lanes and the host app: drops events the
/// sender has no right to and stale pointer or controller states, and puts each controller
/// event in the sender's granted slot.
#[derive(Debug, Default)]
pub struct InputGate {
    state_lanes: HashMap<String, InputSequencer>,
}

impl InputGate {
    pub fn admit(
        &mut self,
        from: &str,
        lane: InputLane,
        seq: u32,
        events: Vec<InputEvent>,
        control: &ControlState,
    ) -> Vec<InputEvent> {
        let rights = control.rights_of(from);
        if !rights.any() {
            return Vec::new();
        }
        if lane == InputLane::State && !self.state_lanes.entry(from.to_string()).or_default().accept(seq) {
            return Vec::new();
        }
        events
            .into_iter()
            .filter(|ev| control.allows(from, ev))
            .map(|ev| match ev {
                InputEvent::Pad { state, .. } => InputEvent::Pad { index: rights.pad.unwrap_or(0), state },
                InputEvent::PadGone { .. } => InputEvent::PadGone { index: rights.pad.unwrap_or(0) },
                other => other,
            })
            .collect()
    }

    /// Forget a member's sequence numbers (their link or rights are gone).
    pub fn forget(&mut self, member: &str) {
        self.state_lanes.remove(member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PadState;
    use crate::keycodes::DomCode;

    const KBM: ControlWants = ControlWants { mouse_keyboard: true, controller: false };
    const PAD: ControlWants = ControlWants { mouse_keyboard: false, controller: true };

    fn open() -> ControlState {
        let mut state = ControlState::default();
        state.set_available(true, ControlEnd::ShareEnded);
        state.set_pad_slots(4);
        state
    }

    #[test]
    fn test_nothing_is_granted_without_a_request_and_a_click() {
        let mut state = ControlState::default();
        assert!(!state.request("bo", KBM, 0), "not available yet");
        let mut state_open = open();
        assert!(state_open.grant("bo").is_empty(), "no request, no grant");
        assert!(state_open.request("bo", KBM, 0));
        assert!(!state_open.rights_of("bo").any());
        let changes = state_open.grant("bo");
        assert_eq!(changes, vec![RightsChange {
            member: "bo".into(),
            rights: ControlRights { mouse_keyboard: true, pad: None },
            reason: None,
        }]);
        assert!(!state_open.request("bo", KBM, 1), "already holds it");
        state.set_available(false, ControlEnd::ShareEnded);
    }

    #[test]
    fn test_mouse_and_keyboard_have_one_holder() {
        let mut state = open();
        state.request("bo", KBM, 0);
        state.grant("bo");
        state.request("cy", KBM, 0);
        let changes = state.grant("cy");
        assert_eq!(changes[0].member, "bo");
        assert_eq!(changes[0].reason, Some(ControlEnd::TakenOver));
        assert!(!changes[0].rights.any());
        assert_eq!(state.mouse_keyboard_holder(), Some("cy"));
    }

    #[test]
    fn test_controllers_take_the_lowest_free_slot() {
        let mut state = open();
        for m in ["a", "b", "c", "d", "e"] {
            assert!(state.request(m, PAD, 0));
        }
        for (m, slot) in [("a", 0), ("b", 1), ("c", 2), ("d", 3)] {
            state.grant(m);
            assert_eq!(state.rights_of(m).pad, Some(slot));
        }
        let full = state.grant("e");
        assert_eq!(full[0].reason, Some(ControlEnd::NoSlot));
        state.revoke("b", ControlEnd::Revoked);
        state.request("e", PAD, 1);
        state.grant("e");
        assert_eq!(state.rights_of("e").pad, Some(1), "the freed slot is reused");
        // Mouse/keyboard and a controller can be held together.
        state.request("a", KBM, 2);
        state.grant("a");
        assert_eq!(state.rights_of("a"), ControlRights { mouse_keyboard: true, pad: Some(0) });
    }

    #[test]
    fn test_controller_slots_follow_what_the_host_can_create() {
        let mut state = open();
        state.set_pad_slots(1);
        state.request("a", PAD, 0);
        state.grant("a");
        state.request("b", PAD, 0);
        assert_eq!(state.grant("b")[0].reason, Some(ControlEnd::NoSlot), "only one virtual controller here");
        state.set_pad_slots(0);
        state.request("c", PAD, 0);
        assert_eq!(state.grant("c")[0].reason, Some(ControlEnd::NoSlot), "no controllers at all");
        state.set_pad_slots(9);
        assert_eq!(state.pad_slots(), MAX_PADS as u8);
    }

    #[test]
    fn test_deny_cooldown_ttl_and_queue_limit() {
        let mut state = open();
        state.request("bo", KBM, 0);
        assert_eq!(state.deny("bo", 1000)[0].reason, Some(ControlEnd::Denied));
        assert!(!state.request("bo", KBM, 5000), "cooling down");
        assert!(state.request("bo", KBM, 11_000));
        let expired = state.expire(11_000 + REQUEST_TTL_MS);
        assert_eq!(expired[0].reason, Some(ControlEnd::Expired));
        assert!(!state.is_pending("bo"));
        for i in 0..MAX_PENDING {
            assert!(state.request(&format!("m{i}"), KBM, 0));
        }
        assert!(!state.request("late", KBM, 0), "the queue is full");
    }

    #[test]
    fn test_turning_control_off_ends_everything() {
        let mut state = open();
        state.request("bo", KBM, 0);
        state.grant("bo");
        state.request("cy", PAD, 0);
        state.grant("cy");
        state.request("di", PAD, 0);
        let changes = state.set_available(false, ControlEnd::AgentLost);
        let mut members: Vec<_> = changes.iter().map(|c| c.member.as_str()).collect();
        members.sort();
        assert_eq!(members, vec!["bo", "cy", "di"]);
        assert!(changes.iter().all(|c| c.reason == Some(ControlEnd::AgentLost) && !c.rights.any()));
        assert_eq!(state.holders().count(), 0);
        assert!(!state.request("bo", KBM, 0));
        assert!(state.revoke("bo", ControlEnd::Revoked).is_empty(), "nothing left to revoke");
    }

    #[test]
    fn test_gate_admits_only_what_each_member_may_send() {
        let mut state = open();
        state.request("bo", KBM, 0);
        state.grant("bo");
        state.request("cy", PAD, 0);
        state.request("di", PAD, 0);
        state.grant("di");
        state.grant("cy");
        let mut gate = InputGate::default();
        let key = InputEvent::Key { code: DomCode::KeyA, down: true, repeat: false };
        let pad = InputEvent::Pad { index: 0, state: PadState { buttons: 1, ..PadState::NEUTRAL } };

        assert_eq!(gate.admit("bo", InputLane::Events, 1, vec![key, InputEvent::Alive], &state), vec![key, InputEvent::Alive]);
        assert!(gate.admit("cy", InputLane::Events, 1, vec![key], &state).is_empty(), "controller only");
        assert!(gate.admit("eve", InputLane::Events, 1, vec![InputEvent::Alive], &state).is_empty());
        assert!(gate.admit("bo", InputLane::State, 1, vec![pad], &state).is_empty(), "no controller slot");
        // Cy's pad goes to Cy's slot (P2), whatever index the viewer used.
        let admitted = gate.admit("cy", InputLane::State, 5, vec![pad], &state);
        assert_eq!(admitted, vec![InputEvent::Pad { index: 1, state: PadState { buttons: 1, ..PadState::NEUTRAL } }]);
        assert!(gate.admit("cy", InputLane::State, 4, vec![pad], &state).is_empty(), "stale state packet");
        assert_eq!(gate.admit("di", InputLane::Events, 1, vec![InputEvent::PadGone { index: 3 }], &state),
            vec![InputEvent::PadGone { index: 0 }]);
    }
}
