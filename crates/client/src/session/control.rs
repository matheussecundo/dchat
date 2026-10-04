//! Remote control of a member's computer while they share their screen.
//!
//! Sharer side: requests wait for an explicit click, `ControlState` decides who holds what,
//! `InputGate` drops anything a member has no right to, and what passes goes to the local
//! `dchat-host` app. Viewer side: requests, grants and sealed input packets over the two
//! input channels of the direct link. Nothing is relayed and nothing is stored.

use super::RoomSession;
use leptos::{SignalSet, SignalUpdate};
use crate::agent::{AgentEvent, AgentLink};
use crate::media::{self, ScreenInfo};
use crate::names::pubkey_tag;
use crate::state::{ControlOfferUi, ControlPromptUi, ControlUi, MyControlUi};
use protocol::{
    open_input, seal_input, ControlEnd, ControlRights, ControlState, ControlWants, InputBudget, InputEvent, InputGate,
    InputLane, PointerMode, RightsChange, RoomBody, RoomEnvelope, TabToAgent, VideoKind, MAX_EVENTS_PER_PACKET,
    MAX_PADS,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
pub(super) struct Control {
    // Sharer side.
    state: RefCell<ControlState>,
    gate: RefCell<InputGate>,
    budgets: RefCell<HashMap<String, InputBudget>>,
    screen: RefCell<Option<ScreenInfo>>,
    agent: RefCell<Option<Rc<AgentLink>>>,
    /// The sharer switched requests off.
    paused: Cell<bool>,
    status_seq: Cell<u64>,
    /// Small numbers the app knows members by (it never sees keys).
    who: RefCell<HashMap<String, u32>>,
    next_who: Cell<u32>,
    modes: RefCell<HashMap<String, PointerMode>>,
    // Everyone.
    /// Latest `ControlStatus` of each sharer, replayed to new neighbors.
    pub(super) envelopes: RefCell<HashMap<String, RoomEnvelope>>,
    offers: RefCell<HashMap<String, (u64, ControlOfferUi)>>,
    // Viewer side.
    mine: RefCell<HashMap<String, MyControlUi>>,
    seqs: RefCell<HashMap<(String, u8), u32>>,
}

impl RoomSession {
    // ---- Sharer side ---------------------------------------------------------------------

    /// Use (or stop using) the paired `dchat-host` app for remote control.
    pub fn attach_agent(&self, link: Option<Rc<AgentLink>>) {
        if let Some(link) = &link {
            let s = self.clone();
            link.set_listener(Some(Rc::new(move |event| match event {
                AgentEvent::Ready => s.on_agent_ready(),
                AgentEvent::Lost => s.refresh_control_availability(),
            })));
        }
        *self.inner.control.agent.borrow_mut() = link;
        self.refresh_control_availability();
    }

    fn agent(&self) -> Option<Rc<AgentLink>> {
        self.inner.control.agent.borrow().clone().filter(|a| a.is_paired())
    }

    pub(super) fn set_screen_info(&self, info: Option<ScreenInfo>) {
        *self.inner.control.screen.borrow_mut() = info;
    }

    /// Whether this tab shares a whole screen: the condition for offering control.
    pub fn shares_whole_screen(&self) -> Option<bool> {
        let mine = self.my_voice();
        (mine.in_voice && mine.video == VideoKind::Screen)
            .then(|| self.inner.control.screen.borrow().as_ref().is_some_and(ScreenInfo::is_monitor))
    }

    pub fn set_allow_control(&self, on: bool) {
        self.inner.control.paused.set(!on);
        self.refresh_control_availability();
    }

    fn on_agent_ready(&self) {
        self.refresh_control_availability();
        if !self.inner.control.state.borrow().available() {
            return;
        }
        // A resumed app session: tell it again what is shared and who holds what.
        self.send_agent_screen();
        let holders: Vec<String> = self.inner.control.state.borrow().holders().map(str::to_string).collect();
        for member in holders {
            let rights = self.inner.control.state.borrow().rights_of(&member);
            self.send_agent_control(&member, rights);
        }
    }

    /// Offer control only while the app is paired, this tab shares a whole screen and the
    /// sharer allows requests. Turning it off ends every grant.
    pub(super) fn refresh_control_availability(&self) {
        let control = &self.inner.control;
        let agent_ready = self.agent().is_some();
        let pad_slots = self.agent().and_then(|a| a.caps()).map_or(0, |caps| caps.pads);
        if control.state.borrow().pad_slots() != pad_slots.min(protocol::MAX_PADS as u8) {
            control.state.borrow_mut().set_pad_slots(pad_slots);
            if control.state.borrow().available() {
                self.publish_control_status();
            }
        }
        let available =
            !self.inner.closed.get() && agent_ready && !control.paused.get() && self.shares_whole_screen() == Some(true);
        let was = control.state.borrow().available();
        if available != was {
            let why = if agent_ready { ControlEnd::ShareEnded } else { ControlEnd::AgentLost };
            let changes = control.state.borrow_mut().set_available(available, why);
            self.apply_rights_changes(changes, None);
            if available {
                self.send_agent_screen();
            }
            self.publish_control_status();
        }
        self.refresh_control_ui();
    }

    fn send_agent_screen(&self) {
        let screen = self.inner.control.screen.borrow().clone();
        if let (Some(agent), Some(screen)) = (self.agent(), screen) {
            agent.send(&TabToAgent::Screen {
                surface: screen.surface,
                width: screen.width,
                height: screen.height,
                label: screen.label,
            });
        }
    }

    pub fn use_monitor(&self, id: u32) {
        if let Some(agent) = self.agent() {
            agent.send(&TabToAgent::UseMonitor { id });
        }
    }

    pub fn grant_control(&self, member: &str) {
        let changes = self.inner.control.state.borrow_mut().grant(member);
        self.apply_rights_changes(changes, None);
    }

    pub fn deny_control(&self, member: &str) {
        let changes = self.inner.control.state.borrow_mut().deny(member, js_sys::Date::now() as u64);
        self.apply_rights_changes(changes, None);
    }

    pub fn revoke_control(&self, member: &str) {
        let changes = self.inner.control.state.borrow_mut().revoke(member, ControlEnd::Revoked);
        self.apply_rights_changes(changes, None);
    }

    pub fn stop_all_control(&self) {
        let changes = self.inner.control.state.borrow_mut().revoke_all(ControlEnd::Revoked);
        self.apply_rights_changes(changes, None);
    }

    fn who_of(&self, member: &str) -> u32 {
        let control = &self.inner.control;
        *control.who.borrow_mut().entry(member.to_string()).or_insert_with(|| {
            let next = control.next_who.get() + 1;
            control.next_who.set(next);
            next
        })
    }

    fn label_of(&self, member: &str) -> String {
        let name = self.inner.names.borrow().get(member).cloned().unwrap_or_default();
        format!("{name} · {}", pubkey_tag(member))
    }

    fn send_agent_control(&self, member: &str, rights: ControlRights) {
        let Some(agent) = self.agent() else {
            return;
        };
        let mode = self.inner.control.modes.borrow().get(member).copied().unwrap_or_default();
        agent.send(&TabToAgent::Control {
            who: self.who_of(member),
            name: self.label_of(member),
            kbm: rights.mouse_keyboard.then_some(mode),
            pad: rights.pad,
        });
    }

    /// Tell each member (except `quiet`, who already knows) and the app what changed.
    fn apply_rights_changes(&self, changes: Vec<RightsChange>, quiet: Option<&str>) {
        if changes.is_empty() {
            return;
        }
        let control = &self.inner.control;
        for change in &changes {
            if quiet != Some(change.member.as_str()) {
                self.send_direct(
                    &change.member,
                    RoomBody::ControlGrant {
                        to: change.member.clone(),
                        mouse_keyboard: change.rights.mouse_keyboard,
                        pad: change.rights.pad,
                        reason: change.reason,
                    },
                );
            }
            if !change.rights.mouse_keyboard {
                control.modes.borrow_mut().remove(&change.member);
            }
            self.send_agent_control(&change.member, change.rights);
            if !change.rights.any() {
                control.gate.borrow_mut().forget(&change.member);
                control.budgets.borrow_mut().remove(&change.member);
            }
        }
        self.publish_control_status();
        self.refresh_control_ui();
    }

    fn publish_control_status(&self) {
        let control = &self.inner.control;
        let (available, controllers, mouse_keyboard, pads) = {
            let state = control.state.borrow();
            (state.available(), state.pad_slots(), state.mouse_keyboard_holder().map(str::to_string), state.pad_holders())
        };
        // Nothing to announce for a tab that never offered control.
        if !available && control.status_seq.get() == 0 {
            return;
        }
        let seq = control.status_seq.get() + 1;
        control.status_seq.set(seq);
        self.publish(RoomBody::ControlStatus { seq, available, controllers, mouse_keyboard, pads });
    }

    pub(super) fn on_control_request(&self, author: &str, wants: ControlWants) {
        let now = js_sys::Date::now() as u64;
        let available = self.inner.control.state.borrow().available();
        if !available {
            self.send_direct(
                author,
                RoomBody::ControlGrant { to: author.to_string(), mouse_keyboard: false, pad: None, reason: Some(ControlEnd::ShareEnded) },
            );
            return;
        }
        let prompted = self.inner.control.state.borrow_mut().request(author, wants, now);
        if prompted {
            // Chime and "(n)" in the title, like a mention: someone is waiting for an answer.
            self.inner.signals.mention.update(|n| *n += 1);
            self.refresh_control_ui();
        }
    }

    pub(super) fn on_control_release(&self, author: &str) {
        let changes = self.inner.control.state.borrow_mut().revoke(author, ControlEnd::Revoked);
        self.apply_rights_changes(changes, Some(author));
    }

    /// A packet on one of a member's input channels: only for a sharer offering control,
    /// only what that member holds, then on to the app.
    pub(super) fn on_input_packet(&self, from: &str, lane: InputLane, packet: &[u8]) {
        let control = &self.inner.control;
        if !control.state.borrow().available() {
            return;
        }
        let Ok((packet_lane, seq, events)) = open_input(&self.inner.key, from, &self.inner.me, packet) else {
            return;
        };
        if packet_lane != lane {
            return;
        }
        let now = js_sys::Date::now();
        let within_budget = control
            .budgets
            .borrow_mut()
            .entry(from.to_string())
            .or_insert_with(|| InputBudget::new(now))
            .take(events.len(), now);
        if !within_budget {
            return;
        }
        let admitted = control.gate.borrow_mut().admit(from, lane, seq, events, &control.state.borrow());
        for ev in &admitted {
            if let InputEvent::Mode { mode } = ev {
                control.modes.borrow_mut().insert(from.to_string(), *mode);
                let rights = control.state.borrow().rights_of(from);
                self.send_agent_control(from, rights);
                if let Some(track) = self.local_track("video") {
                    let game = *mode == PointerMode::Game;
                    media::set_content_hint(&track, if game { "motion" } else { "detail" });
                    media::set_frame_rate(&track, if game { 60 } else { 30 });
                }
            }
        }
        if let Some(agent) = self.agent() {
            agent.send_input(self.who_of(from), admitted);
        }
    }

    /// A member's voice state changed: viewers who leave voice can't see the screen anymore.
    pub(super) fn on_control_voice_state(&self, author: &str, in_voice: bool) {
        if author == self.inner.me {
            if !in_voice {
                self.release_all_my_control();
            }
            self.refresh_control_availability();
        } else if !in_voice {
            let changes = self.inner.control.state.borrow_mut().revoke(author, ControlEnd::LeftVoice);
            self.apply_rights_changes(changes, None);
        }
    }

    /// The link to `pk` is gone (or they left).
    pub(super) fn on_control_peer_lost(&self, pk: &str) {
        let control = &self.inner.control;
        let changes = control.state.borrow_mut().revoke(pk, ControlEnd::LeftVoice);
        self.apply_rights_changes(changes, Some(pk));
        let had = control.mine.borrow_mut().remove(pk).is_some();
        control.offers.borrow_mut().remove(pk);
        control.envelopes.borrow_mut().remove(pk);
        if had {
            self.toast("control_end_share_ended");
        }
        self.refresh_control_ui();
    }

    pub(super) fn control_tick(&self) {
        let changes = self.inner.control.state.borrow_mut().expire(js_sys::Date::now() as u64);
        self.apply_rights_changes(changes, None);
    }

    /// Leaving the room: end every grant, give back what we hold, let go of the app.
    pub(super) fn end_control(&self) {
        let changes = self.inner.control.state.borrow_mut().set_available(false, ControlEnd::ShareEnded);
        self.apply_rights_changes(changes, None);
        self.release_all_my_control();
        if let Some(agent) = self.inner.control.agent.borrow_mut().take() {
            agent.set_listener(None);
        }
    }

    // ---- Viewer side -----------------------------------------------------------------------

    pub fn request_control(&self, sharer: &str, wants: ControlWants) {
        if !wants.any() {
            return;
        }
        self.inner.control.mine.borrow_mut().entry(sharer.to_string()).or_default().requested = true;
        self.send_direct(
            sharer,
            RoomBody::ControlRequest { to: sharer.to_string(), mouse_keyboard: wants.mouse_keyboard, controller: wants.controller },
        );
        self.refresh_control_ui();
    }

    /// Give back everything held on `sharer`'s computer (or cancel the request).
    pub fn release_control(&self, sharer: &str) {
        let had = self.inner.control.mine.borrow_mut().remove(sharer).is_some();
        if had {
            self.send_direct(sharer, RoomBody::ControlRelease { to: sharer.to_string() });
        }
        self.refresh_control_ui();
    }

    fn release_all_my_control(&self) {
        let sharers: Vec<String> = self.inner.control.mine.borrow().keys().cloned().collect();
        for sharer in sharers {
            self.release_control(&sharer);
        }
    }

    pub(super) fn on_control_grant(&self, author: &str, mouse_keyboard: bool, pad: Option<u8>, reason: Option<ControlEnd>) {
        let granted = mouse_keyboard || pad.is_some();
        let known = self.inner.control.mine.borrow().contains_key(author);
        if !known {
            if granted {
                // We never asked: give it straight back.
                self.send_direct(author, RoomBody::ControlRelease { to: author.to_string() });
            }
            return;
        }
        if granted {
            if let Some(mine) = self.inner.control.mine.borrow_mut().get_mut(author) {
                mine.requested = false;
                mine.mouse_keyboard = mouse_keyboard;
                mine.pad = pad.filter(|p| (*p as usize) < MAX_PADS);
            }
            if reason == Some(ControlEnd::NoSlot) {
                self.toast("control_end_no_slot");
            }
        } else {
            self.inner.control.mine.borrow_mut().remove(author);
            if let Some(key) = reason.map(end_toast) {
                self.toast(key);
            }
        }
        self.refresh_control_ui();
    }

    /// Desktop (absolute) or game (relative, pointer lock) mouse on `sharer`'s computer.
    pub fn set_control_mode(&self, sharer: &str, mode: PointerMode) {
        if let Some(mine) = self.inner.control.mine.borrow_mut().get_mut(sharer) {
            mine.mode = mode;
        }
        self.send_input(sharer, vec![InputEvent::Mode { mode }]);
        self.refresh_control_ui();
    }

    /// Our controller's state, to every sharer who gave us a controller slot.
    pub fn send_pad_input(&self, events: Vec<InputEvent>) {
        let sharers: Vec<String> =
            self.inner.control.mine.borrow().iter().filter(|(_, m)| m.pad.is_some()).map(|(k, _)| k.clone()).collect();
        for sharer in sharers {
            self.send_input(&sharer, events.clone());
        }
    }

    /// Seal and send input for `sharer`, keeping only what we hold there.
    pub fn send_input(&self, sharer: &str, events: Vec<InputEvent>) {
        let Some(mine) = self.inner.control.mine.borrow().get(sharer).copied().filter(MyControlUi::granted) else {
            return;
        };
        let allowed = events.into_iter().filter(|ev| match ev {
            InputEvent::Pad { .. } | InputEvent::PadGone { .. } => mine.pad.is_some(),
            InputEvent::Alive | InputEvent::ReleaseAll => true,
            other => mine.mouse_keyboard && other.is_mouse_keyboard(),
        });
        self.seal_and_send_input(sharer, allowed.collect());
    }

    /// Test hook: send input without checking our own rights; the sharer must drop it.
    #[cfg(feature = "e2e-hooks")]
    pub(super) fn send_raw_input(&self, sharer: &str, events: Vec<InputEvent>) {
        self.seal_and_send_input(sharer, events);
    }

    fn seal_and_send_input(&self, sharer: &str, events: Vec<InputEvent>) {
        let Some(link) = self.link(sharer).filter(|l| l.is_open()) else {
            return;
        };
        let (events_lane, state_lane): (Vec<_>, Vec<_>) = events.into_iter().partition(|ev| ev.lane() == InputLane::Events);
        for (lane, batch) in [(InputLane::Events, events_lane), (InputLane::State, state_lane)] {
            for chunk in batch.chunks(MAX_EVENTS_PER_PACKET) {
                let seq = {
                    let mut seqs = self.inner.control.seqs.borrow_mut();
                    let seq = seqs.entry((sharer.to_string(), lane as u8)).or_insert(0);
                    *seq = seq.wrapping_add(1);
                    *seq
                };
                if let Ok(packet) = seal_input(&self.inner.key, &self.inner.me, sharer, lane, seq, chunk) {
                    link.send_input(lane, &packet);
                }
            }
        }
    }

    pub(super) fn on_control_status(&self, envelope: &RoomEnvelope) {
        let RoomBody::ControlStatus { seq, available, controllers, mouse_keyboard, pads } = &envelope.body else {
            return;
        };
        let author = envelope.author.as_str();
        if author == self.inner.me || pads.len() > MAX_PADS {
            return;
        }
        let control = &self.inner.control;
        if control.offers.borrow().get(author).is_some_and(|(known, _)| *known >= *seq) {
            return;
        }
        control.envelopes.borrow_mut().insert(author.to_string(), envelope.clone());
        if *available {
            control
                .offers
                .borrow_mut()
                .insert(
                    author.to_string(),
                    (*seq, ControlOfferUi { controllers: (*controllers).min(MAX_PADS as u8), mouse_keyboard: mouse_keyboard.clone(), pads: pads.clone() }),
                );
        } else {
            control.offers.borrow_mut().remove(author);
            if control.mine.borrow_mut().remove(author).is_some() {
                self.toast("control_end_share_ended");
            }
        }
        self.refresh_control_ui();
    }

    fn refresh_control_ui(&self) {
        let control = &self.inner.control;
        let state = control.state.borrow();
        let holder = state.mouse_keyboard_holder().map(str::to_string);
        let prompts = state
            .pending()
            .map(|(member, wants)| ControlPromptUi {
                member: member.to_string(),
                mouse_keyboard: wants.mouse_keyboard,
                controller: wants.controller,
                takes_over: holder.clone().filter(|h| wants.mouse_keyboard && h != member),
            })
            .collect();
        let ui = ControlUi {
            offers: control.offers.borrow().iter().map(|(k, (_, offer))| (k.clone(), offer.clone())).collect(),
            mine: control.mine.borrow().clone(),
            prompts,
            hosting: state.available(),
            host_mouse_keyboard: holder,
            host_pads: state.pad_holders(),
        };
        self.inner.signals.control.set(ui);
    }
}

fn end_toast(reason: ControlEnd) -> &'static str {
    match reason {
        ControlEnd::Denied => "control_end_denied",
        ControlEnd::Revoked => "control_end_revoked",
        ControlEnd::TakenOver => "control_end_taken_over",
        ControlEnd::ShareEnded | ControlEnd::LeftVoice => "control_end_share_ended",
        ControlEnd::AgentLost => "control_end_agent_lost",
        ControlEnd::Expired => "control_end_expired",
        ControlEnd::NoSlot => "control_end_no_slot",
    }
}
