//! Remote-control input pacing, at both ends of the path.
//!
//! The viewer sends each pointer move as soon as it happens instead of waiting for a timer,
//! but at most one every [`MOVE_MIN_GAP_MS`] ([`MoveSpacer`]): 250 moves a second is as fluid
//! as any display, and the unreliable state lane loses less when it isn't flooded.
//!
//! The sharer's tab forwards input to `dchat-host` over one WebSocket, one
//! `TabToAgent::Input { who, ev }` message per frame (see `agent.rs`), at most 64 events per
//! frame (`max_events_per_frame` in the host's config). The host allows that connection 500
//! messages a second with a burst of 1000 and closes the session after 10 s over it
//! (`crates/host-agent/src/config.rs`, `serve` in `server.rs`). Forwarding every packet as it
//! comes would already be too much: a mouse at 62.5 Hz plus four controllers at 120 Hz is
//! about 542 frames a second. [`AgentPacer`] keeps it under that: positions and controller
//! states only need their newest value, so they are coalesced per member and sent within a
//! budget of [`AGENT_STATE_FRAMES_PER_SEC`]; clicks, keys and everything else never wait and,
//! on their own, use none of that budget, so the rest of the host's 500 is theirs.

use crate::input::{InputEvent, InputLane, PadState};
use std::collections::BTreeMap;

/// Shortest gap between two pointer moves a viewer sends (at most 250 a second).
pub const MOVE_MIN_GAP_MS: f64 = 4.0;

/// Viewer side: decides whether a pointer move goes out now or waits for the gap to end.
#[derive(Clone, Debug)]
pub struct MoveSpacer {
    min_gap_ms: f64,
    last_sent_ms: Option<f64>,
}

impl Default for MoveSpacer {
    fn default() -> Self {
        Self::new(MOVE_MIN_GAP_MS)
    }
}

impl MoveSpacer {
    pub fn new(min_gap_ms: f64) -> Self {
        Self { min_gap_ms, last_sent_ms: None }
    }

    /// `Ok(())`: send the move now (the time is recorded). `Err(wait_ms)`: hold it and flush
    /// the newest move after `wait_ms` (always > 0). A clock that went backwards counts as
    /// ready, so a move is never held for longer than one gap.
    pub fn try_send(&mut self, now_ms: f64) -> Result<(), f64> {
        if let Some(last) = self.last_sent_ms {
            let since = now_ms - last;
            if since >= 0.0 && since < self.min_gap_ms {
                return Err(self.min_gap_ms - since);
            }
        }
        self.last_sent_ms = Some(now_ms);
        Ok(())
    }

    /// Forget the last send, so the next move goes out at once.
    pub fn reset(&mut self) {
        self.last_sent_ms = None;
    }
}

/// Frames per second toward dchat-host that carry positions and controller states, across all
/// members. Frames of clicks, keys and the rest alone don't count, so this leaves them 100 of
/// the host's 500 messages a second (and its burst of 1000 for a while beyond that).
pub const AGENT_STATE_FRAMES_PER_SEC: f64 = 400.0;
/// Frames that may go out back to back before the rate applies.
pub const AGENT_STATE_FRAME_BURST: f64 = 40.0;
/// dchat-host drops frames with more events than this (`max_events_per_frame`).
pub const AGENT_MAX_EVENTS_PER_FRAME: usize = 64;

/// Slack for floating-point refills, so a poll scheduled by `next_poll_in` finds its token.
const TOKEN_EPSILON: f64 = 1e-9;

/// One member's state-lane events not sent yet: only what the host needs to end up where the
/// viewer is.
#[derive(Debug)]
struct Pending {
    abs: Option<(u16, u16)>,
    /// Summed exactly; split into as many `PointerRel` events as needed on the way out.
    rel: (i64, i64),
    /// The newest pointer event was relative, so it goes after the absolute one.
    rel_last: bool,
    pads: BTreeMap<u8, PadState>,
    /// When this member started waiting: the oldest goes first.
    since_ms: f64,
}

impl Pending {
    fn new(since_ms: f64) -> Self {
        Self { abs: None, rel: (0, 0), rel_last: false, pads: BTreeMap::new(), since_ms }
    }

    fn merge(&mut self, ev: &InputEvent) {
        match *ev {
            InputEvent::PointerAbs { x, y } => {
                self.abs = Some((x, y));
                self.rel_last = false;
            }
            InputEvent::PointerRel { dx, dy } => {
                self.rel = (self.rel.0.saturating_add(dx.into()), self.rel.1.saturating_add(dy.into()));
                self.rel_last = true;
            }
            InputEvent::Pad { index, state } => {
                self.pads.insert(index, state);
            }
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.abs.is_none() && self.rel == (0, 0) && self.pads.is_empty()
    }

    /// Pointer first, then controllers by index. Absolute and relative moves only mix around
    /// a mode switch (an event, which flushes first); if they do, the kind that came last
    /// goes last, so the pointer ends where the viewer left it.
    fn into_events(self) -> Vec<InputEvent> {
        let abs = self.abs.map(|(x, y)| InputEvent::PointerAbs { x, y });
        let rel = split_rel(self.rel);
        let mut out = Vec::with_capacity(1 + rel.len() + self.pads.len());
        if self.rel_last {
            out.extend(abs);
            out.extend(rel);
        } else {
            out.extend(rel);
            out.extend(abs);
        }
        out.extend(self.pads.into_iter().map(|(index, state)| InputEvent::Pad { index, state }));
        out
    }
}

/// A summed relative move as `PointerRel` events whose deltas add up to it exactly.
fn split_rel((mut dx, mut dy): (i64, i64)) -> Vec<InputEvent> {
    let mut out = Vec::new();
    let clamp = |v: i64| v.clamp(i16::MIN.into(), i16::MAX.into());
    while dx != 0 || dy != 0 {
        let (sx, sy) = (clamp(dx), clamp(dy));
        out.push(InputEvent::PointerRel { dx: sx as i16, dy: sy as i16 });
        dx -= sx;
        dy -= sy;
    }
    out
}

/// One `TabToAgent::Input` message: the member's `who` and its events.
pub type AgentFrame = (u32, Vec<InputEvent>);

/// Sharer side: paces what goes to dchat-host (see the module docs).
///
/// - State-lane events (`PointerAbs`, `PointerRel`, `Pad`) are coalesced per member: the
///   newest absolute position, the exact sum of relative moves, the newest state per pad.
///   They go out as one frame when the token bucket allows, oldest waiting member first.
/// - Everything else goes out at once, after that member's waiting state (so a click never
///   lands before the move that preceded it), and never waits for a token. A frame of events
///   alone uses none: one member's clicks, keys or wheel can't hold back anyone's positions or
///   controllers. When the frame carries waiting state, that state pays its token as it would
///   have on its own, going into debt if needed, so state frames stay within the budget.
/// - No frame is empty or longer than [`AGENT_MAX_EVENTS_PER_FRAME`].
///
/// One member moving at 250 Hz or less never waits, whatever events go with it.
#[derive(Debug)]
pub struct AgentPacer {
    per_sec: f64,
    burst: f64,
    tokens: f64,
    last_ms: Option<f64>,
    pending: BTreeMap<u32, Pending>,
}

impl Default for AgentPacer {
    fn default() -> Self {
        Self::with_budget(AGENT_STATE_FRAMES_PER_SEC, AGENT_STATE_FRAME_BURST)
    }
}

impl AgentPacer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A pacer with its own budget (at least one frame a second and a burst of one). The
    /// bucket starts full.
    pub fn with_budget(per_sec: f64, burst: f64) -> Self {
        let burst = burst.max(1.0);
        Self { per_sec: per_sec.max(1.0), burst, tokens: burst, last_ms: None, pending: BTreeMap::new() }
    }

    /// Events member `who` sent, in order. Returns the frames to send now, possibly for other
    /// members too when they have waited longer and the budget allows.
    pub fn push(&mut self, who: u32, events: Vec<InputEvent>, now_ms: f64) -> Vec<AgentFrame> {
        self.refill(now_ms);
        let mut out = Vec::new();
        let mut run = Vec::new();
        for ev in events {
            if ev.lane() == InputLane::State {
                if !run.is_empty() {
                    self.send_now(who, std::mem::take(&mut run), &mut out);
                }
                self.pending.entry(who).or_insert_with(|| Pending::new(now_ms)).merge(&ev);
            } else {
                run.push(ev);
            }
        }
        if !run.is_empty() {
            self.send_now(who, run, &mut out);
        }
        self.drain(&mut out);
        out
    }

    /// Frames the budget allows now, oldest waiting member first.
    pub fn poll(&mut self, now_ms: f64) -> Vec<AgentFrame> {
        self.refill(now_ms);
        let mut out = Vec::new();
        self.drain(&mut out);
        out
    }

    /// Milliseconds until `poll` can send more; `None` when nothing is waiting.
    pub fn next_poll_in(&self, now_ms: f64) -> Option<f64> {
        if self.pending.values().all(Pending::is_empty) {
            return None;
        }
        let tokens = self.tokens_at(now_ms);
        Some(if tokens + TOKEN_EPSILON >= 1.0 { 0.0 } else { (1.0 - tokens) / self.per_sec * 1000.0 })
    }

    /// Drop what member `who` has waiting (control ended, member left).
    pub fn forget(&mut self, who: u32) {
        self.pending.remove(&who);
    }

    fn tokens_at(&self, now_ms: f64) -> f64 {
        let elapsed = match self.last_ms {
            Some(last) if now_ms.is_finite() => (now_ms - last).max(0.0),
            _ => 0.0,
        };
        (self.tokens + elapsed / 1000.0 * self.per_sec).min(self.burst)
    }

    /// A clock that goes backwards refills nothing, and the new time becomes the reference.
    fn refill(&mut self, now_ms: f64) {
        self.tokens = self.tokens_at(now_ms);
        if now_ms.is_finite() {
            self.last_ms = Some(now_ms);
        }
    }

    /// `who`'s waiting state, then `events`, without waiting for a token.
    fn send_now(&mut self, who: u32, events: Vec<InputEvent>, out: &mut Vec<AgentFrame>) {
        let mut frame = self.pending.remove(&who).map(Pending::into_events).unwrap_or_default();
        let state_len = frame.len();
        frame.extend(events);
        self.emit(who, frame, state_len, out);
    }

    /// Waiting members, oldest first, one token per frame.
    fn drain(&mut self, out: &mut Vec<AgentFrame>) {
        self.pending.retain(|_, p| !p.is_empty());
        while self.tokens + TOKEN_EPSILON >= 1.0 {
            // `min_by` keeps the first of equals: the lowest `who` on a tie.
            let oldest = self.pending.iter().min_by(|a, b| a.1.since_ms.total_cmp(&b.1.since_ms)).map(|(&who, _)| who);
            let Some(who) = oldest else { break };
            if let Some(waiting) = self.pending.remove(&who) {
                let frame = waiting.into_events();
                let state_len = frame.len();
                self.emit(who, frame, state_len, out);
            }
        }
    }

    /// Split into frames the host accepts. Each frame holding some of the first `state_len`
    /// events (the state) uses a token, into debt if there is none (the events after it can't
    /// wait); frames of events alone are free. The debt is bounded so state that rode along
    /// with a burst of clicks can't hold other positions back for long.
    fn emit(&mut self, who: u32, events: Vec<InputEvent>, state_len: usize, out: &mut Vec<AgentFrame>) {
        for (i, chunk) in events.chunks(AGENT_MAX_EVENTS_PER_FRAME).enumerate() {
            if i * AGENT_MAX_EVENTS_PER_FRAME < state_len {
                self.tokens = (self.tokens - 1.0).max(-self.burst);
            }
            out.push((who, chunk.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::MouseButton;
    use crate::keycodes::DomCode;

    fn abs(x: u16) -> InputEvent {
        InputEvent::PointerAbs { x, y: x.wrapping_mul(3) }
    }

    fn pad(index: u8, axis: i16) -> InputEvent {
        InputEvent::Pad { index, state: PadState { buttons: 1, axes: [axis, -axis, 0, 0], triggers: [0, 0] } }
    }

    fn click() -> InputEvent {
        InputEvent::Button { button: MouseButton::Left, down: true, at: None }
    }

    /// A pacer whose bucket is empty at time 0.
    fn exhausted() -> AgentPacer {
        let mut pacer = AgentPacer::with_budget(400.0, 1.0);
        assert_eq!(pacer.push(9, vec![abs(0)], 0.0).len(), 1, "the one token");
        pacer
    }

    #[test]
    fn test_move_spacer() {
        let mut spacer = MoveSpacer::default();
        assert_eq!(spacer.try_send(100.0), Ok(()), "the first move goes at once");
        assert_eq!(spacer.try_send(101.0), Err(3.0));
        assert_eq!(spacer.try_send(103.5), Err(0.5));
        assert_eq!(spacer.try_send(104.0), Ok(()), "after the gap");
        assert_eq!(spacer.try_send(110.0), Ok(()));
        assert_eq!(spacer.try_send(50.0), Ok(()), "a clock that went backwards counts as ready");
        assert_eq!(spacer.try_send(52.0), Err(2.0), "and becomes the reference");
        spacer.reset();
        assert_eq!(spacer.try_send(52.5), Ok(()), "reset sends at once");
        let mut custom = MoveSpacer::new(10.0);
        assert_eq!(custom.try_send(0.0), Ok(()));
        assert_eq!(custom.try_send(4.0), Err(6.0));
    }

    #[test]
    fn test_one_member_at_250_hz_is_never_delayed() {
        let mut pacer = AgentPacer::default();
        for i in 0..500u16 {
            let t = i as f64 * 4.0;
            assert_eq!(pacer.push(1, vec![abs(i)], t), vec![(1, vec![abs(i)])], "move {i}");
            assert_eq!(pacer.next_poll_in(t), None);
        }
    }

    /// One desktop mouse and one game mouse at 250 Hz plus four controllers at 120 Hz for
    /// 10 s, polling whenever the pacer asks.
    #[test]
    fn test_busy_room_stays_within_budget_and_delivers_the_latest() {
        const END_MS: f64 = 10_000.0;
        const MOUSE: u32 = 1;
        const GAME: u32 = 2;
        let mut pacer = AgentPacer::default();
        let mut delivered: Vec<(f64, AgentFrame)> = Vec::new();
        let mut next_mouse = 0.0;
        let mut next_game = 1.0;
        let mut next_pads = [0.5, 2.0, 4.0, 6.0];
        let pad_period = 1000.0 / 120.0;
        let mut next_poll: Option<f64> = None;
        let (mut mouse_i, mut game_i, mut pad_i) = (0u16, 0i64, [0i16; 4]);
        let (mut last_abs, mut pushed_rel, mut last_pad) = (None, (0i64, 0i64), [None; 4]);

        for _ in 0..1_000_000 {
            let pushes = [next_mouse, next_game, next_pads[0], next_pads[1], next_pads[2], next_pads[3]];
            let now = pushes.into_iter().filter(|&t| t < END_MS).chain(next_poll).reduce(f64::min);
            let Some(now) = now else { break };
            let mut out = Vec::new();
            if next_mouse <= now && next_mouse < END_MS {
                mouse_i = mouse_i.wrapping_add(7);
                last_abs = Some(abs(mouse_i));
                out.extend(pacer.push(MOUSE, vec![abs(mouse_i)], now));
                next_mouse += 4.0;
            }
            if next_game <= now && next_game < END_MS {
                game_i += 1;
                let (dx, dy) = (20_000 + (game_i % 13) as i16, -15_000 - (game_i % 5) as i16);
                pushed_rel = (pushed_rel.0 + dx as i64, pushed_rel.1 + dy as i64);
                out.extend(pacer.push(GAME, vec![InputEvent::PointerRel { dx, dy }], now));
                next_game += 4.0;
            }
            for i in 0..4 {
                if next_pads[i] <= now && next_pads[i] < END_MS {
                    pad_i[i] = pad_i[i].wrapping_add(101);
                    last_pad[i] = Some(pad(0, pad_i[i]));
                    out.extend(pacer.push(10 + i as u32, vec![pad(0, pad_i[i])], now));
                    next_pads[i] += pad_period;
                }
            }
            if next_poll.is_some_and(|t| t <= now) {
                out.extend(pacer.poll(now));
            }
            delivered.extend(out.into_iter().map(|frame| (now, frame)));
            next_poll = pacer.next_poll_in(now).map(|wait| {
                assert!(wait > 0.0, "nothing left behind while tokens remain");
                now + wait
            });
        }
        assert_eq!(next_poll, None, "everything delivered");

        let (last_t, _) = delivered.last().unwrap();
        assert!(*last_t < END_MS + 50.0, "the backlog clears promptly, at {last_t}");
        let in_time = delivered.iter().filter(|(t, _)| *t <= END_MS).count();
        assert!(in_time <= 400 * 10 + 40, "{in_time} frames in 10 s");
        for (n, (t, _)) in delivered.iter().enumerate() {
            assert!((n + 1) as f64 <= 40.0 + 400.0 * t / 1000.0 + 1e-6, "frame {n} at {t} ms is over budget");
        }
        assert!(delivered.iter().all(|(_, (_, ev))| !ev.is_empty() && ev.len() <= AGENT_MAX_EVENTS_PER_FRAME));

        let of = |who: u32| delivered.iter().filter(move |(_, f)| f.0 == who).flat_map(|(_, f)| f.1.iter().copied());
        assert_eq!(of(MOUSE).last(), last_abs, "the last position arrives");
        let rel = of(GAME).fold((0i64, 0i64), |(sx, sy), ev| match ev {
            InputEvent::PointerRel { dx, dy } => (sx + dx as i64, sy + dy as i64),
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(rel, pushed_rel, "relative moves add up exactly");
        let split = delivered.iter().any(|(_, (who, ev))| *who == GAME && ev.len() > 1);
        assert!(split, "some coalesced sums exceeded i16::MAX and were split");
        for (i, last) in last_pad.into_iter().enumerate() {
            assert_eq!(of(10 + i as u32).last(), last, "pad member {i}");
        }
        for who in [MOUSE, GAME, 10, 11, 12, 13] {
            let frames = delivered.iter().filter(|(_, f)| f.0 == who).count();
            assert!(frames >= 500, "member {who} got a fair share: {frames} frames");
        }
    }

    #[test]
    fn test_a_click_takes_the_waiting_move_with_it_in_order() {
        let mut pacer = exhausted();
        assert_eq!(pacer.push(1, vec![abs(5)], 0.0), vec![], "no token: the move waits");
        assert_eq!(pacer.next_poll_in(0.0), Some(2.5));
        assert_eq!(pacer.push(1, vec![click()], 0.0), vec![(1, vec![abs(5), click()])], "events never wait");
        assert_eq!(pacer.next_poll_in(0.0), None);

        let key = InputEvent::Key { code: DomCode::KeyA, down: true, repeat: false };
        let mixed = pacer.push(1, vec![abs(6), key, abs(7), pad(0, 1)], 1.0);
        assert_eq!(mixed, vec![(1, vec![abs(6), key])], "a mixed batch is handled in order");
        assert_eq!(pacer.poll(100.0), vec![(1, vec![abs(7), pad(0, 1)])], "pointer before pads");
    }

    #[test]
    fn test_forget_drops_waiting_state() {
        let mut pacer = exhausted();
        pacer.push(1, vec![abs(5), pad(1, 9)], 0.0);
        pacer.push(2, vec![abs(8)], 0.0);
        pacer.forget(1);
        assert_eq!(pacer.poll(1000.0), vec![(2, vec![abs(8)])]);
        assert_eq!(pacer.next_poll_in(1000.0), None);
        pacer.forget(3);
    }

    #[test]
    fn test_coalescing_keeps_the_newest_and_exact_sums() {
        let mut pacer = exhausted();
        let rel = |dx: i16, dy: i16| InputEvent::PointerRel { dx, dy };
        pacer.push(1, vec![rel(30_000, -30_000), rel(30_000, -30_000), pad(1, 5), pad(0, 1)], 0.0);
        pacer.push(1, vec![rel(30_000, -30_000), pad(1, 6)], 0.5);
        let frames = pacer.poll(10.0);
        assert_eq!(
            frames,
            vec![(
                1,
                vec![rel(i16::MAX, i16::MIN), rel(i16::MAX, i16::MIN), rel(24_466, -24_464), pad(0, 1), pad(1, 6)]
            )],
            "90_000 and -90_000 in i16 steps, then each pad's newest state"
        );

        pacer.push(1, vec![rel(5, 0), rel(-5, 0)], 10.0);
        assert_eq!(pacer.next_poll_in(10.0), None, "a zero sum is nothing to send");
        assert_eq!(pacer.push(1, vec![], 10.0), vec![], "never an empty frame");
        let mut fresh = AgentPacer::default();
        assert_eq!(fresh.push(1, vec![rel(0, 0)], 0.0), vec![]);
        assert_eq!(fresh.push(1, vec![], 0.0), vec![]);
    }

    #[test]
    fn test_newest_pointer_kind_goes_last() {
        let rel = InputEvent::PointerRel { dx: 3, dy: 4 };
        let mut pacer = exhausted();
        pacer.push(1, vec![abs(1), rel], 0.0);
        assert_eq!(pacer.poll(10.0), vec![(1, vec![abs(1), rel])]);
        let mut pacer = exhausted();
        pacer.push(1, vec![rel, abs(1)], 0.0);
        assert_eq!(pacer.poll(10.0), vec![(1, vec![rel, abs(1)])]);
    }

    #[test]
    fn test_long_frames_are_split_in_order() {
        let mut pacer = exhausted();
        let pads: Vec<_> = (0..100).map(|i| pad(i, i as i16)).collect();
        pacer.push(1, pads.clone(), 0.0);
        let frames = pacer.push(1, vec![InputEvent::ReleaseAll], 0.0);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|(who, ev)| *who == 1 && ev.len() <= AGENT_MAX_EVENTS_PER_FRAME));
        let flat: Vec<_> = frames.into_iter().flat_map(|(_, ev)| ev).collect();
        assert_eq!(flat, [pads, vec![InputEvent::ReleaseAll]].concat());
    }

    #[test]
    fn test_waiting_members_are_served_oldest_first() {
        let mut pacer = exhausted();
        pacer.push(3, vec![abs(3)], 0.0);
        pacer.push(2, vec![abs(2)], 1.0);
        pacer.push(3, vec![abs(4)], 2.0);
        assert_eq!(pacer.poll(2.5), vec![(3, vec![abs(4)])], "member 3 has waited longest");
        assert_eq!(pacer.next_poll_in(2.5), Some(2.5));
        let newcomer = pacer.push(1, vec![abs(1)], 5.0);
        assert_eq!(newcomer, vec![(2, vec![abs(2)])], "a push serves whoever waited longest");
        assert_eq!(pacer.poll(7.5), vec![(1, vec![abs(1)])]);
    }

    /// Scrolling at 200 Hz while moving at 250 Hz: 450 frames a second, each sent at once.
    #[test]
    fn test_events_never_delay_one_members_moves() {
        let mut pacer = AgentPacer::default();
        let wheel = InputEvent::Wheel { dx: 0, dy: -3, at: None };
        for ms in 0..10_000u32 {
            let t = ms as f64;
            if ms % 4 == 0 {
                let i = (ms / 4) as u16;
                assert_eq!(pacer.push(1, vec![abs(i)], t), vec![(1, vec![abs(i)])], "move at {t} ms");
            }
            if ms % 5 == 0 {
                assert_eq!(pacer.push(1, vec![wheel], t), vec![(1, vec![wheel])], "wheel at {t} ms");
            }
            assert_eq!(pacer.next_poll_in(t), None, "nothing waits at {t} ms");
        }
    }

    /// One member's keys at the host's whole rate leave the budget to the others' controllers.
    #[test]
    fn test_one_members_events_leave_the_others_budget_alone() {
        let mut pacer = AgentPacer::default();
        let pad_period = 1000.0 / 120.0;
        let mut next_pads = [0.0, 1.0, 2.0];
        let mut pad_i = 0i16;
        for tick in 0..5_000u32 {
            let t = tick as f64 * 2.0;
            let key = InputEvent::Key { code: DomCode::KeyA, down: tick % 2 == 0, repeat: false };
            assert_eq!(pacer.push(1, vec![key], t), vec![(1, vec![key])]);
            for (m, next) in next_pads.iter_mut().enumerate() {
                if *next <= t {
                    pad_i = pad_i.wrapping_add(1);
                    let who = 10 + m as u32;
                    let frames = pacer.push(who, vec![pad(0, pad_i)], t);
                    assert_eq!(frames, vec![(who, vec![pad(0, pad_i)])], "controller {m} at {t} ms");
                    *next += pad_period;
                }
            }
        }
    }

    #[test]
    fn test_events_alone_are_free_and_waiting_state_pays() {
        let mut pacer = AgentPacer::with_budget(400.0, 2.0);
        for _ in 0..10 {
            assert_eq!(pacer.push(1, vec![InputEvent::Alive], 0.0), vec![(1, vec![InputEvent::Alive])]);
        }
        assert_eq!(pacer.push(1, vec![abs(1)], 0.0), vec![(1, vec![abs(1)])], "ten events used no token");
        assert_eq!(pacer.push(2, vec![abs(2)], 0.0), vec![(2, vec![abs(2)])], "the second token");
        for i in 0..5 {
            assert_eq!(pacer.push(1, vec![abs(10 + i)], 0.0), vec![], "no token: the move waits");
            assert_eq!(pacer.push(1, vec![click()], 0.0), vec![(1, vec![abs(10 + i), click()])], "events never wait");
        }
        pacer.push(2, vec![abs(3)], 0.0);
        assert_eq!(pacer.next_poll_in(0.0), Some(7.5), "the moves that rode along: two frames of debt, then one token");
        assert_eq!(pacer.poll(7.5), vec![(2, vec![abs(3)])]);

        // A split frame pays only for the parts that hold state.
        let mut split = AgentPacer::with_budget(400.0, 3.0);
        for who in 0..3 {
            assert_eq!(split.push(who, vec![abs(1)], 0.0).len(), 1);
        }
        let pads: Vec<_> = (0..64).map(|i| pad(i, 1)).collect();
        assert_eq!(split.push(1, pads.clone(), 0.0), vec![]);
        let frames = split.push(1, vec![InputEvent::ReleaseAll], 0.0);
        assert_eq!(frames, vec![(1, pads), (1, vec![InputEvent::ReleaseAll])]);
        split.push(2, vec![abs(2)], 0.0);
        assert_eq!(split.next_poll_in(0.0), Some(5.0), "one frame of debt, not two");

        let mut odd_clock = AgentPacer::with_budget(f64::NAN, 0.0);
        assert_eq!(odd_clock.push(1, vec![abs(1)], f64::NAN), vec![(1, vec![abs(1)])]);
        odd_clock.push(1, vec![abs(2)], 5.0);
        assert_eq!(odd_clock.next_poll_in(5.0), Some(1000.0), "sanitized to one frame a second");
    }
}
