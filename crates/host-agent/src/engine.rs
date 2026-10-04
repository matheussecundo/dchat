//! The only place that touches the OS input backend. One thread owns the injector and
//! everything currently held down, so every way control can end (revoke, the tab going
//! away, the user stopping it, a panic) releases exactly what is held.

use crate::geometry::{map_position, pick_monitor, Choice, Monitor};
use crate::inject::{Caps, Injector};
use crate::status::sanitize_name;
use protocol::{DomCode, InputEvent, MonitorInfo, MouseButton, PadState, PointerMode};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// More held keys than any real use needs; beyond this, presses are ignored.
const MAX_HELD_KEYS: usize = 16;
/// A pushed stick or held button with no update for this long goes back to neutral
/// (pad states travel on a lossy channel; viewers resend them while active).
const PAD_STALE: Duration = Duration::from_millis(500);

pub enum EngineCmd {
    /// Everything member `who` may do now (both `None`: their control ends).
    Control { who: u32, name: String, kbm: Option<PointerMode>, pad: Option<u8> },
    Input { who: u32, events: Vec<InputEvent> },
    Screen { surface: String, width: u32, height: u32 },
    UseMonitor(u32),
    /// The tab's connection dropped: release everything, keep grants for a resume.
    ReleaseAll,
    /// The session is over: release everything, unplug controllers, forget all grants.
    EndSession,
    Shutdown(mpsc::Sender<()>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Status(String),
    Warning(String),
    Monitors { list: Vec<MonitorInfo>, chosen: Option<u32> },
}

struct Member {
    name: String,
    kbm: Option<PointerMode>,
    pad: Option<u8>,
    keys: BTreeSet<DomCode>,
    buttons: BTreeSet<MouseButton>,
    pad_active: bool,
    last_pad: Instant,
    last_seen: Instant,
}

impl Member {
    fn holds_anything(&self) -> bool {
        !self.keys.is_empty() || !self.buttons.is_empty() || self.pad_active
    }
}

pub type MonitorSource = Box<dyn Fn() -> Vec<Monitor> + Send>;

pub struct Engine {
    injector: Box<dyn Injector>,
    caps: Caps,
    members: BTreeMap<u32, Member>,
    monitors: MonitorSource,
    known_monitors: Vec<Monitor>,
    monitor_override: Option<u32>,
    chosen: Option<u32>,
    /// The tab shares a whole monitor, so absolute positions can be mapped.
    absolute_ok: bool,
    watchdog: Duration,
    warned: BTreeSet<String>,
}

impl Engine {
    pub fn new(injector: Box<dyn Injector>, monitors: MonitorSource, monitor_override: Option<u32>, watchdog: Duration) -> Self {
        let caps = injector.caps();
        let known_monitors = monitors();
        Self {
            injector,
            caps,
            members: BTreeMap::new(),
            monitors,
            known_monitors,
            monitor_override,
            chosen: None,
            absolute_ok: false,
            watchdog,
            warned: BTreeSet::new(),
        }
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn handle(&mut self, cmd: EngineCmd, now: Instant) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        match cmd {
            EngineCmd::Control { who, name, kbm, pad } => self.control(who, &name, kbm, pad, now, &mut out),
            EngineCmd::Input { who, events } => self.input(who, events, now, &mut out),
            EngineCmd::Screen { surface, width, height } => self.screen(&surface, width, height, &mut out),
            EngineCmd::UseMonitor(id) => {
                if self.known_monitors.iter().any(|m| m.id == id) {
                    self.monitor_override = Some(id);
                    self.chosen = Some(id);
                    out.push(self.monitors_event());
                }
            }
            EngineCmd::ReleaseAll => self.release_all(),
            EngineCmd::EndSession => self.end_session(&mut out),
            EngineCmd::Shutdown(ack) => {
                self.end_session(&mut out);
                let _ = ack.send(());
            }
        }
        out
    }

    /// Watchdog: someone holds keys, buttons or a pushed stick but has gone quiet.
    pub fn tick(&mut self, now: Instant) -> Vec<EngineEvent> {
        // Controllers first: a stale pushed stick is neutralized quickly.
        for member in self.members.values_mut() {
            if let (Some(slot), true) = (member.pad, member.pad_active) {
                if now.duration_since(member.last_pad) > PAD_STALE {
                    let _ = self.injector.pad_update(slot, &PadState::NEUTRAL);
                    member.pad_active = false;
                    let _ = self.injector.flush();
                }
            }
        }
        let stale: Vec<u32> = self
            .members
            .iter()
            .filter(|(_, m)| m.holds_anything() && now.duration_since(m.last_seen) > self.watchdog)
            .map(|(who, _)| *who)
            .collect();
        stale
            .into_iter()
            .map(|who| {
                self.release(who);
                EngineEvent::Status(format!("{}: no input for a moment, released what was held", self.name_of(who)))
            })
            .collect()
    }

    fn name_of(&self, who: u32) -> String {
        self.members.get(&who).map(|m| m.name.clone()).unwrap_or_else(|| "someone".into())
    }

    fn control(&mut self, who: u32, name: &str, kbm: Option<PointerMode>, pad: Option<u8>, now: Instant, out: &mut Vec<EngineEvent>) {
        // Mouse and keyboard have one holder, whatever the tab says.
        if kbm.is_some() {
            let others: Vec<u32> = self.members.iter().filter(|(w, m)| **w != who && m.kbm.is_some()).map(|(w, _)| *w).collect();
            for other in others {
                self.release_mouse_keyboard(other);
                if let Some(m) = self.members.get_mut(&other) {
                    m.kbm = None;
                }
            }
        }
        // A controller slot has one holder too, and must exist here.
        let max_pads = *self.caps.pads.as_ref().unwrap_or(&0);
        let pad = pad.filter(|slot| {
            *slot < max_pads && !self.members.iter().any(|(w, m)| *w != who && m.pad == Some(*slot))
        });

        let member = self.members.entry(who).or_insert_with(|| Member {
            name: sanitize_name(name),
            kbm: None,
            pad: None,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            pad_active: false,
            last_pad: now,
            last_seen: now,
        });
        let (old_kbm, old_pad) = (member.kbm, member.pad);
        if old_kbm.is_some() && old_kbm != kbm {
            self.release_mouse_keyboard(who);
        }
        if old_pad.is_some() && old_pad != pad {
            if let Some(slot) = old_pad {
                let _ = self.injector.pad_update(slot, &PadState::NEUTRAL);
                self.injector.pad_unplug(slot);
            }
        }
        if let Some(slot) = pad.filter(|_| old_pad != pad) {
            if let Err(err) = self.injector.pad_plug(slot) {
                self.warn_once("pad", format!("Controller unavailable: {err}"), out);
            }
        }
        let _ = self.injector.flush();

        let member = self.members.get_mut(&who).expect("inserted above");
        member.kbm = kbm;
        member.pad = pad;
        member.pad_active = member.pad_active && pad.is_some();
        member.last_seen = now;
        let name = member.name.clone();
        if kbm.is_none() && pad.is_none() {
            self.members.remove(&who);
            if old_kbm.is_some() || old_pad.is_some() {
                out.push(EngineEvent::Status(format!("{name} stopped controlling")));
            }
            return;
        }
        if kbm != old_kbm {
            if let Some(mode) = kbm {
                let mode = if mode == PointerMode::Game { "game" } else { "desktop" };
                out.push(EngineEvent::Status(format!("{name} is controlling mouse and keyboard ({mode} mode)")));
            }
        }
        if pad != old_pad {
            if let Some(slot) = pad {
                out.push(EngineEvent::Status(format!("{name}: controller P{}", slot + 1)));
            }
        }
    }

    fn input(&mut self, who: u32, events: Vec<InputEvent>, now: Instant, out: &mut Vec<EngineEvent>) {
        let Some(member) = self.members.get_mut(&who) else {
            return;
        };
        member.last_seen = now;
        let mut failed = None;
        let mut release = false;
        for ev in events {
            let result = match ev {
                InputEvent::PointerAbs { x, y } => match member.kbm {
                    Some(PointerMode::Desktop) if self.absolute_ok => {
                        let (ax, ay) = map_position(&self.known_monitors, self.chosen, x, y);
                        self.injector.move_abs(ax, ay)
                    }
                    _ => Ok(()),
                },
                InputEvent::PointerRel { dx, dy } => match member.kbm {
                    Some(PointerMode::Game) => self.injector.move_rel(dx as i32, dy as i32),
                    _ => Ok(()),
                },
                InputEvent::Button { button, down, at } => match member.kbm {
                    Some(mode) => {
                        let mut result = Ok(());
                        if let (Some((x, y)), PointerMode::Desktop, true) = (at, mode, self.absolute_ok) {
                            let (ax, ay) = map_position(&self.known_monitors, self.chosen, x, y);
                            result = self.injector.move_abs(ax, ay);
                        }
                        let changed = if down { member.buttons.insert(button) } else { member.buttons.remove(&button) };
                        if changed {
                            result = result.and(self.injector.button(mode, button, down));
                        }
                        result
                    }
                    None => Ok(()),
                },
                InputEvent::Wheel { dx, dy, at } => match member.kbm {
                    Some(mode) => {
                        let mut result = Ok(());
                        if let (Some((x, y)), PointerMode::Desktop, true) = (at, mode, self.absolute_ok) {
                            let (ax, ay) = map_position(&self.known_monitors, self.chosen, x, y);
                            result = self.injector.move_abs(ax, ay);
                        }
                        result.and(self.injector.wheel(mode, dx as i32, dy as i32))
                    }
                    None => Ok(()),
                },
                InputEvent::Key { code, down, repeat } if member.kbm.is_some() => {
                    if !down {
                        if member.keys.remove(&code) {
                            self.injector.key(code, false)
                        } else {
                            Ok(())
                        }
                    } else if repeat || member.keys.contains(&code) {
                        // The OS repeats held keys itself, except Windows for injected ones.
                        if self.caps.software_repeat && member.keys.contains(&code) {
                            self.injector.key(code, true)
                        } else {
                            Ok(())
                        }
                    } else if member.keys.len() < MAX_HELD_KEYS {
                        member.keys.insert(code);
                        self.injector.key(code, true)
                    } else {
                        Ok(())
                    }
                }
                InputEvent::Key { .. } => Ok(()),
                InputEvent::Pad { index, state } if member.pad == Some(index) => {
                    member.pad_active = !state.is_resting();
                    member.last_pad = now;
                    self.injector.pad_update(index, &state)
                }
                InputEvent::PadGone { index } if member.pad == Some(index) => {
                    member.pad_active = false;
                    self.injector.pad_update(index, &PadState::NEUTRAL)
                }
                InputEvent::Pad { .. } | InputEvent::PadGone { .. } | InputEvent::Alive | InputEvent::Mode { .. } => Ok(()),
                InputEvent::ReleaseAll => {
                    release = true;
                    break;
                }
            };
            if let Err(err) = result {
                failed = Some(err);
            }
        }
        if let Err(err) = self.injector.flush() {
            failed = Some(err);
        }
        if release {
            self.release(who);
        }
        if let Some(err) = failed {
            self.warn_once("inject", format!("Injecting input failed: {err}"), out);
        }
    }

    fn screen(&mut self, surface: &str, width: u32, height: u32, out: &mut Vec<EngineEvent>) {
        self.absolute_ok = surface == "monitor";
        if !self.absolute_ok {
            self.warn_once(
                "surface",
                "Only a whole shared screen can be controlled with the mouse pointer; game mode, keyboard and controllers still work.".into(),
                out,
            );
        }
        self.known_monitors = (self.monitors)();
        self.chosen = match pick_monitor(&self.known_monitors, (width, height), self.monitor_override) {
            Choice::Chosen(id) => Some(id),
            Choice::Ambiguous(_) => None,
        };
        out.push(self.monitors_event());
    }

    fn monitors_event(&self) -> EngineEvent {
        EngineEvent::Monitors { list: self.known_monitors.iter().map(Monitor::info).collect(), chosen: self.chosen }
    }

    fn warn_once(&mut self, key: &str, message: String, out: &mut Vec<EngineEvent>) {
        if self.warned.insert(key.to_string()) {
            out.push(EngineEvent::Warning(message));
        }
    }

    fn release_mouse_keyboard(&mut self, who: u32) {
        let Some(member) = self.members.get_mut(&who) else {
            return;
        };
        let mode = member.kbm.unwrap_or_default();
        for code in std::mem::take(&mut member.keys) {
            let _ = self.injector.key(code, false);
        }
        for button in std::mem::take(&mut member.buttons) {
            let _ = self.injector.button(mode, button, false);
        }
        let _ = self.injector.flush();
    }

    /// Let go of everything `who` holds; their grants stay.
    fn release(&mut self, who: u32) {
        self.release_mouse_keyboard(who);
        if let Some(member) = self.members.get_mut(&who) {
            if let (Some(slot), true) = (member.pad, member.pad_active) {
                let _ = self.injector.pad_update(slot, &PadState::NEUTRAL);
                member.pad_active = false;
                let _ = self.injector.flush();
            }
        }
    }

    pub fn release_all(&mut self) {
        let all: Vec<u32> = self.members.keys().copied().collect();
        for who in all {
            self.release(who);
        }
    }

    fn end_session(&mut self, out: &mut Vec<EngineEvent>) {
        self.release_all();
        for member in std::mem::take(&mut self.members).into_values() {
            if let Some(slot) = member.pad {
                self.injector.pad_unplug(slot);
            }
            if member.kbm.is_some() || member.pad.is_some() {
                out.push(EngineEvent::Status(format!("{} stopped controlling", member.name)));
            }
        }
        let _ = self.injector.flush();
        self.absolute_ok = false;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let mut ignored = Vec::new();
        self.end_session(&mut ignored);
    }
}

/// The engine on its own thread. Commands arrive over a channel; events go back to the server.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineCmd>,
}

impl EngineHandle {
    pub fn send(&self, cmd: EngineCmd) {
        let _ = self.tx.send(cmd);
    }

    /// End the session and wait (at most 2 s) until everything held is released.
    pub fn shutdown(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(EngineCmd::Shutdown(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

pub fn spawn(mut engine: Engine, events: tokio::sync::mpsc::UnboundedSender<EngineEvent>) -> EngineHandle {
    let (tx, rx) = mpsc::channel::<EngineCmd>();
    std::thread::Builder::new()
        .name("dchat-host-engine".into())
        .spawn(move || loop {
            let produced = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(cmd) => engine.handle(cmd, Instant::now()),
                Err(mpsc::RecvTimeoutError::Timeout) => engine.tick(Instant::now()),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            for event in produced {
                let _ = events.send(event);
            }
        })
        .expect("start the input engine thread");
    EngineHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::inject::mock::{Injected, Recorder, Recording};

    fn engine(pads: u8, repeat: bool) -> (Engine, Recording) {
        let mut recorder = Recorder::new(pads);
        recorder.software_repeat = repeat;
        let log = recorder.log.clone();
        let monitors: MonitorSource = Box::new(|| {
            vec![
                Monitor { id: 0, name: "A".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 }, pixels: (1920, 1080), primary: true },
                Monitor { id: 1, name: "B".into(), rect: Rect { x: 1920, y: 0, w: 2560, h: 1440 }, pixels: (2560, 1440), primary: false },
            ]
        });
        (Engine::new(Box::new(recorder), monitors, None, Duration::from_millis(1500)), log)
    }

    fn taken(log: &Recording) -> Vec<Injected> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    fn grant(e: &mut Engine, who: u32, kbm: Option<PointerMode>, pad: Option<u8>, now: Instant) -> Vec<EngineEvent> {
        e.handle(EngineCmd::Control { who, name: format!("m{who}"), kbm, pad }, now)
    }

    fn key(code: DomCode, down: bool) -> InputEvent {
        InputEvent::Key { code, down, repeat: false }
    }

    #[test]
    fn test_only_granted_members_inject() {
        let now = Instant::now();
        let (mut e, log) = engine(0, false);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true)] }, now);
        assert!(taken(&log).is_empty(), "no grant, nothing happens");
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true), key(DomCode::KeyA, false)] }, now);
        assert_eq!(taken(&log), vec![
            Injected::Key { code: DomCode::KeyA, down: true },
            Injected::Key { code: DomCode::KeyA, down: false },
        ]);
    }

    #[test]
    fn test_revoking_releases_exactly_what_is_held() {
        let now = Instant::now();
        let (mut e, log) = engine(0, false);
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        e.handle(
            EngineCmd::Input {
                who: 1,
                events: vec![
                    key(DomCode::ShiftLeft, true),
                    key(DomCode::KeyW, true),
                    InputEvent::Button { button: MouseButton::Left, down: true, at: None },
                    key(DomCode::KeyW, false),
                ],
            },
            now,
        );
        taken(&log);
        let events = grant(&mut e, 1, None, None, now);
        assert_eq!(taken(&log), vec![
            Injected::Key { code: DomCode::ShiftLeft, down: false },
            Injected::Button { mode: PointerMode::Desktop, button: MouseButton::Left, down: false },
        ]);
        assert_eq!(events, vec![EngineEvent::Status("m1 stopped controlling".into())]);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true)] }, now);
        assert!(taken(&log).is_empty());
    }

    #[test]
    fn test_one_mouse_keyboard_holder() {
        let now = Instant::now();
        let (mut e, log) = engine(0, false);
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true)] }, now);
        taken(&log);
        grant(&mut e, 2, Some(PointerMode::Desktop), None, now);
        assert_eq!(taken(&log), vec![Injected::Key { code: DomCode::KeyA, down: false }], "the old holder is released");
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyB, true)] }, now);
        e.handle(EngineCmd::Input { who: 2, events: vec![key(DomCode::KeyC, true)] }, now);
        assert_eq!(taken(&log), vec![Injected::Key { code: DomCode::KeyC, down: true }]);
    }

    #[test]
    fn test_pointer_modes_and_monitor_mapping() {
        let now = Instant::now();
        let (mut e, log) = engine(0, false);
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        let abs = InputEvent::PointerAbs { x: 0, y: 0 };
        e.handle(EngineCmd::Input { who: 1, events: vec![abs] }, now);
        assert!(taken(&log).is_empty(), "no absolute pointer until a whole screen is shared");
        let events = e.handle(EngineCmd::Screen { surface: "monitor".into(), width: 2560, height: 1440 }, now);
        assert!(matches!(&events[0], EngineEvent::Monitors { chosen: Some(1), .. }));
        e.handle(EngineCmd::Input { who: 1, events: vec![abs, InputEvent::PointerRel { dx: 5, dy: 5 }] }, now);
        let moved = taken(&log);
        assert_eq!(moved.len(), 1, "relative moves are ignored in desktop mode");
        let Injected::MoveAbs { x, .. } = moved[0] else { panic!() };
        assert_eq!(x as u64 * 4480 / 65536, 1920, "the left edge of the second monitor");

        grant(&mut e, 1, Some(PointerMode::Game), None, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![abs, InputEvent::PointerRel { dx: 5, dy: -3 }] }, now);
        assert_eq!(taken(&log), vec![Injected::MoveRel { dx: 5, dy: -3 }]);

        e.handle(EngineCmd::Screen { surface: "window".into(), width: 800, height: 600 }, now);
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        taken(&log);
        e.handle(EngineCmd::Input { who: 1, events: vec![abs] }, now);
        assert!(taken(&log).is_empty(), "a shared window can't be mapped");
    }

    #[test]
    fn test_key_repeats_follow_the_os() {
        let now = Instant::now();
        for (software, expected) in [(false, 1), (true, 2)] {
            let (mut e, log) = engine(0, software);
            grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
            let repeat = InputEvent::Key { code: DomCode::KeyJ, down: true, repeat: true };
            e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyJ, true), repeat] }, now);
            assert_eq!(taken(&log).len(), expected);
        }
    }

    #[test]
    fn test_controllers_use_their_own_slot() {
        let now = Instant::now();
        let (mut e, log) = engine(4, false);
        grant(&mut e, 1, None, Some(0), now);
        grant(&mut e, 2, None, Some(0), now);
        assert_eq!(taken(&log), vec![Injected::PadPlug { slot: 0 }], "a slot has one holder");
        let pressed = PadState { buttons: 1, ..PadState::NEUTRAL };
        e.handle(EngineCmd::Input { who: 1, events: vec![InputEvent::Pad { index: 0, state: pressed }] }, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![InputEvent::Pad { index: 3, state: pressed }] }, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true)] }, now);
        assert_eq!(taken(&log), vec![Injected::PadUpdate { slot: 0, state: pressed }]);
        grant(&mut e, 1, None, None, now);
        assert_eq!(taken(&log), vec![Injected::PadUpdate { slot: 0, state: PadState::NEUTRAL }, Injected::PadUnplug { slot: 0 }]);
    }

    #[test]
    fn test_stale_controller_states_go_back_to_neutral() {
        let now = Instant::now();
        let (mut e, log) = engine(4, false);
        grant(&mut e, 1, None, Some(2), now);
        let pushed = PadState { axes: [20000, 0, 0, 0], ..PadState::NEUTRAL };
        e.handle(EngineCmd::Input { who: 1, events: vec![InputEvent::Pad { index: 2, state: pushed }] }, now);
        taken(&log);
        e.tick(now + Duration::from_millis(400));
        assert!(taken(&log).is_empty());
        e.tick(now + Duration::from_millis(600));
        assert_eq!(taken(&log), vec![Injected::PadUpdate { slot: 2, state: PadState::NEUTRAL }]);
        e.tick(now + Duration::from_millis(900));
        assert!(taken(&log).is_empty(), "only once");
    }

    #[test]
    fn test_watchdog_releases_quiet_members() {
        let now = Instant::now();
        let (mut e, log) = engine(0, false);
        grant(&mut e, 1, Some(PointerMode::Desktop), None, now);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyW, true)] }, now);
        taken(&log);
        assert!(e.tick(now + Duration::from_millis(1000)).is_empty());
        e.handle(EngineCmd::Input { who: 1, events: vec![InputEvent::Alive] }, now + Duration::from_millis(1000));
        assert!(e.tick(now + Duration::from_millis(2000)).is_empty(), "heartbeats keep it alive");
        let events = e.tick(now + Duration::from_millis(2600));
        assert_eq!(events.len(), 1);
        assert_eq!(taken(&log), vec![Injected::Key { code: DomCode::KeyW, down: false }]);
        assert!(e.tick(now + Duration::from_millis(5000)).is_empty(), "nothing held any more");
    }

    #[test]
    fn test_session_end_and_drop_release_everything() {
        let now = Instant::now();
        let (mut e, log) = engine(4, false);
        grant(&mut e, 1, Some(PointerMode::Desktop), Some(1), now);
        let pressed = PadState { buttons: 1, ..PadState::NEUTRAL };
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyA, true), InputEvent::Pad { index: 1, state: pressed }] }, now);
        taken(&log);
        e.handle(EngineCmd::ReleaseAll, now);
        assert_eq!(taken(&log), vec![
            Injected::Key { code: DomCode::KeyA, down: false },
            Injected::PadUpdate { slot: 1, state: PadState::NEUTRAL },
        ]);
        e.handle(EngineCmd::Input { who: 1, events: vec![key(DomCode::KeyB, true)] }, now);
        taken(&log);
        drop(e);
        assert_eq!(taken(&log), vec![Injected::Key { code: DomCode::KeyB, down: false }, Injected::PadUnplug { slot: 1 }]);
    }
}
