//! Remote-control input: the events a viewer sends to the member whose screen they control.
//!
//! Over WebRTC they travel as small binary packets on two data channels per link: the
//! reliable `input-events` lane (keys, clicks, wheel: losing one would leave a key stuck)
//! and the unordered, unreliable `input-state` lane (pointer and controller positions: only
//! the newest value matters). Every packet is sealed with the room key, and the AAD binds it
//! to its lane, sequence number, sender and recipient. From the sharer's tab to the local
//! `dchat-host` app the same events travel as JSON.

use crate::crypto::{KEY_LENGTH, NONCE_LENGTH};
use crate::keycodes::DomCode;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_EVENTS_PER_PACKET: usize = 32;
const MAX_PLAINTEXT: usize = 512;
const HEADER_LEN: usize = 1 + 4 + NONCE_LENGTH;
const TAG_LEN: usize = 16;
const AAD_DOMAIN: &[u8] = b"dchat:input:v1";

/// Data channel labels.
pub const INPUT_EVENTS_LABEL: &str = "input-events";
pub const INPUT_STATE_LABEL: &str = "input-state";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputLane {
    /// Reliable and ordered.
    Events = 0,
    /// Unordered, never retransmitted: newest value wins.
    State = 1,
}

impl InputLane {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Events),
            1 => Some(Self::State),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PointerMode {
    /// Absolute: the host pointer goes where the viewer points (desktop use).
    #[default]
    Desktop,
    /// Relative movement with pointer lock (games).
    Game,
}

/// Numbered like `MouseEvent.button`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    Back = 3,
    Forward = 4,
}

impl MouseButton {
    pub fn from_dom(button: i16) -> Option<Self> {
        match button {
            0 => Some(Self::Left),
            1 => Some(Self::Middle),
            2 => Some(Self::Right),
            3 => Some(Self::Back),
            4 => Some(Self::Forward),
            _ => None,
        }
    }
}

/// A controller in the W3C "standard" layout. `buttons` has bit `i` set while standard
/// button `i` is pressed (6 and 7, the analog triggers, are in `triggers` instead).
/// `axes` are left X, left Y, right X, right Y with Y positive downwards, as browsers report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct PadState {
    pub buttons: u32,
    pub axes: [i16; 4],
    pub triggers: [u8; 2],
}

/// Standard buttons, excluding the analog triggers 6 and 7.
pub const PAD_BUTTON_MASK: u32 = 0x1_FF3F;
/// Stick positions closer to the centre than this count as resting.
const PAD_AXIS_REST: i16 = 2048;

impl PadState {
    pub const NEUTRAL: PadState = PadState { buttons: 0, axes: [0; 4], triggers: [0; 2] };

    pub fn is_resting(&self) -> bool {
        self.buttons & PAD_BUTTON_MASK == 0
            && self.triggers == [0, 0]
            && self.axes.iter().all(|a| a.unsigned_abs() < PAD_AXIS_REST as u16)
    }
}

/// Positions are normalized over the shared screen: 0 is the left/top edge, 65535 the
/// right/bottom edge. Wheel deltas are in 1/120 of a notch, positive = right/down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k")]
pub enum InputEvent {
    PointerAbs { x: u16, y: u16 },
    PointerRel { dx: i16, dy: i16 },
    Button { button: MouseButton, down: bool, at: Option<(u16, u16)> },
    Wheel { dx: i16, dy: i16, at: Option<(u16, u16)> },
    Key { code: DomCode, down: bool, repeat: bool },
    /// `index` is the viewer's own controller number; the sharer maps it to the granted slot.
    Pad { index: u8, state: PadState },
    PadGone { index: u8 },
    /// Sent regularly while controlling, so the host can release held input if it stops.
    Alive,
    /// Release everything this viewer holds (focus lost, control ended).
    ReleaseAll,
    Mode { mode: PointerMode },
}

impl InputEvent {
    pub fn lane(&self) -> InputLane {
        match self {
            InputEvent::PointerAbs { .. } | InputEvent::PointerRel { .. } | InputEvent::Pad { .. } => InputLane::State,
            _ => InputLane::Events,
        }
    }

    /// Mouse and keyboard events, as opposed to controller ones.
    pub fn is_mouse_keyboard(&self) -> bool {
        matches!(
            self,
            InputEvent::PointerAbs { .. }
                | InputEvent::PointerRel { .. }
                | InputEvent::Button { .. }
                | InputEvent::Wheel { .. }
                | InputEvent::Key { .. }
                | InputEvent::Mode { .. }
        )
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum InputError {
    #[error("malformed input packet")]
    Malformed,
    #[error("input packet too large")]
    TooLarge,
    #[error("input event on the wrong lane")]
    WrongLane,
    #[error("input packet does not open with this key, sender and recipient")]
    Unauthentic,
}

// ---- Binary codec ----------------------------------------------------------------------

const TAG_POINTER_ABS: u8 = 1;
const TAG_POINTER_REL: u8 = 2;
const TAG_BUTTON: u8 = 3;
const TAG_WHEEL: u8 = 4;
const TAG_KEY: u8 = 5;
const TAG_PAD: u8 = 6;
const TAG_PAD_GONE: u8 = 7;
const TAG_ALIVE: u8 = 8;
const TAG_RELEASE_ALL: u8 = 9;
const TAG_MODE: u8 = 10;

fn put_at(out: &mut Vec<u8>, at: Option<(u16, u16)>) {
    let (x, y) = at.unwrap_or((0, 0));
    out.push(at.is_some() as u8);
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&y.to_be_bytes());
}

pub fn encode_event(ev: &InputEvent, out: &mut Vec<u8>) {
    match *ev {
        InputEvent::PointerAbs { x, y } => {
            out.push(TAG_POINTER_ABS);
            out.extend_from_slice(&x.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
        }
        InputEvent::PointerRel { dx, dy } => {
            out.push(TAG_POINTER_REL);
            out.extend_from_slice(&dx.to_be_bytes());
            out.extend_from_slice(&dy.to_be_bytes());
        }
        InputEvent::Button { button, down, at } => {
            out.extend_from_slice(&[TAG_BUTTON, button as u8, down as u8]);
            put_at(out, at);
        }
        InputEvent::Wheel { dx, dy, at } => {
            out.push(TAG_WHEEL);
            out.extend_from_slice(&dx.to_be_bytes());
            out.extend_from_slice(&dy.to_be_bytes());
            put_at(out, at);
        }
        InputEvent::Key { code, down, repeat } => {
            out.push(TAG_KEY);
            out.extend_from_slice(&code.hid().to_be_bytes());
            out.push(down as u8 | (repeat as u8) << 1);
        }
        InputEvent::Pad { index, state } => {
            out.extend_from_slice(&[TAG_PAD, index]);
            out.extend_from_slice(&state.buttons.to_be_bytes());
            for axis in state.axes {
                out.extend_from_slice(&axis.to_be_bytes());
            }
            out.extend_from_slice(&state.triggers);
        }
        InputEvent::PadGone { index } => out.extend_from_slice(&[TAG_PAD_GONE, index]),
        InputEvent::Alive => out.push(TAG_ALIVE),
        InputEvent::ReleaseAll => out.push(TAG_RELEASE_ALL),
        InputEvent::Mode { mode } => out.extend_from_slice(&[TAG_MODE, mode as u8]),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], InputError> {
        let end = self.pos.checked_add(N).ok_or(InputError::Malformed)?;
        let slice = self.bytes.get(self.pos..end).ok_or(InputError::Malformed)?;
        self.pos = end;
        Ok(slice.try_into().expect("length checked"))
    }
    fn u8(&mut self) -> Result<u8, InputError> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, InputError> {
        Ok(u16::from_be_bytes(self.take()?))
    }
    fn i16(&mut self) -> Result<i16, InputError> {
        Ok(i16::from_be_bytes(self.take()?))
    }
    fn bool(&mut self) -> Result<bool, InputError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(InputError::Malformed),
        }
    }
    fn at(&mut self) -> Result<Option<(u16, u16)>, InputError> {
        let present = self.bool()?;
        let (x, y) = (self.u16()?, self.u16()?);
        Ok(present.then_some((x, y)))
    }
}

fn decode_event(r: &mut Reader) -> Result<InputEvent, InputError> {
    Ok(match r.u8()? {
        TAG_POINTER_ABS => InputEvent::PointerAbs { x: r.u16()?, y: r.u16()? },
        TAG_POINTER_REL => InputEvent::PointerRel { dx: r.i16()?, dy: r.i16()? },
        TAG_BUTTON => {
            let button = MouseButton::from_dom(r.u8()? as i16).ok_or(InputError::Malformed)?;
            InputEvent::Button { button, down: r.bool()?, at: r.at()? }
        }
        TAG_WHEEL => InputEvent::Wheel { dx: r.i16()?, dy: r.i16()?, at: r.at()? },
        TAG_KEY => {
            let code = DomCode::from_hid(r.u16()?).ok_or(InputError::Malformed)?;
            let flags = r.u8()?;
            if flags > 3 {
                return Err(InputError::Malformed);
            }
            InputEvent::Key { code, down: flags & 1 != 0, repeat: flags & 2 != 0 }
        }
        TAG_PAD => {
            let index = r.u8()?;
            let buttons = u32::from_be_bytes(r.take()?);
            let axes = [r.i16()?, r.i16()?, r.i16()?, r.i16()?];
            let triggers = r.take::<2>()?;
            if buttons & !PAD_BUTTON_MASK != 0 {
                return Err(InputError::Malformed);
            }
            InputEvent::Pad { index, state: PadState { buttons, axes, triggers } }
        }
        TAG_PAD_GONE => InputEvent::PadGone { index: r.u8()? },
        TAG_ALIVE => InputEvent::Alive,
        TAG_RELEASE_ALL => InputEvent::ReleaseAll,
        TAG_MODE => InputEvent::Mode {
            mode: match r.u8()? {
                0 => PointerMode::Desktop,
                1 => PointerMode::Game,
                _ => return Err(InputError::Malformed),
            },
        },
        _ => return Err(InputError::Malformed),
    })
}

/// `[count][event...]`, at most `MAX_EVENTS_PER_PACKET` events and 512 bytes.
pub fn encode_events(events: &[InputEvent]) -> Result<Vec<u8>, InputError> {
    if events.len() > MAX_EVENTS_PER_PACKET {
        return Err(InputError::TooLarge);
    }
    let mut out = vec![events.len() as u8];
    for ev in events {
        encode_event(ev, &mut out);
    }
    if out.len() > MAX_PLAINTEXT {
        return Err(InputError::TooLarge);
    }
    Ok(out)
}

pub fn decode_events(bytes: &[u8]) -> Result<Vec<InputEvent>, InputError> {
    if bytes.len() > MAX_PLAINTEXT {
        return Err(InputError::TooLarge);
    }
    let mut r = Reader { bytes, pos: 0 };
    let count = r.u8()? as usize;
    if count > MAX_EVENTS_PER_PACKET {
        return Err(InputError::TooLarge);
    }
    let events = (0..count).map(|_| decode_event(&mut r)).collect::<Result<Vec<_>, _>>()?;
    if r.pos != bytes.len() {
        return Err(InputError::Malformed);
    }
    Ok(events)
}

// ---- Packets ---------------------------------------------------------------------------

fn packet_aad(lane: InputLane, seq: u32, from: &str, to: &str) -> Vec<u8> {
    let mut aad = AAD_DOMAIN.to_vec();
    aad.push(lane as u8);
    aad.extend_from_slice(&seq.to_be_bytes());
    for id in [from, to] {
        aad.push(id.len().min(255) as u8);
        aad.extend_from_slice(id.as_bytes());
    }
    aad
}

/// `[lane][seq u32][nonce 12][ciphertext + tag]`: `events` from `from` to `to`, all on `lane`.
pub fn seal_input(
    key: &[u8; KEY_LENGTH],
    from: &str,
    to: &str,
    lane: InputLane,
    seq: u32,
    events: &[InputEvent],
) -> Result<Vec<u8>, InputError> {
    if events.iter().any(|ev| ev.lane() != lane) {
        return Err(InputError::WrongLane);
    }
    let plaintext = encode_events(events)?;
    let mut nonce = [0u8; NONCE_LENGTH];
    rand::thread_rng().fill_bytes(&mut nonce);
    let aad = packet_aad(lane, seq, from, to);
    let ciphertext = ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plaintext, aad: &aad })
        .map_err(|_| InputError::Malformed)?;
    let mut packet = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    packet.push(lane as u8);
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&nonce);
    packet.extend_from_slice(&ciphertext);
    Ok(packet)
}

/// Open a packet that `from` sent to `to`; every event must belong to the packet's lane.
pub fn open_input(
    key: &[u8; KEY_LENGTH],
    from: &str,
    to: &str,
    packet: &[u8],
) -> Result<(InputLane, u32, Vec<InputEvent>), InputError> {
    if packet.len() < HEADER_LEN + TAG_LEN + 1 {
        return Err(InputError::Malformed);
    }
    if packet.len() > HEADER_LEN + MAX_PLAINTEXT + TAG_LEN {
        return Err(InputError::TooLarge);
    }
    let lane = InputLane::from_byte(packet[0]).ok_or(InputError::Malformed)?;
    let seq = u32::from_be_bytes(packet[1..5].try_into().expect("length checked"));
    let nonce = &packet[5..HEADER_LEN];
    let aad = packet_aad(lane, seq, from, to);
    let plaintext = ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), Payload { msg: &packet[HEADER_LEN..], aad: &aad })
        .map_err(|_| InputError::Unauthentic)?;
    let events = decode_events(&plaintext)?;
    if events.iter().any(|ev| ev.lane() != lane) {
        return Err(InputError::WrongLane);
    }
    Ok((lane, seq, events))
}

// ---- Helpers for capturing and receiving input -----------------------------------------

/// Newest-wins filter for the unordered state lane.
#[derive(Debug, Default)]
pub struct InputSequencer {
    last: Option<u32>,
}

impl InputSequencer {
    pub fn accept(&mut self, seq: u32) -> bool {
        let newer = self.last.map_or(true, |last| (seq.wrapping_sub(last) as i32) > 0);
        if newer {
            self.last = Some(seq);
        }
        newer
    }
}

/// Sums fractional pointer-lock movement and hands out whole pixels.
#[derive(Debug, Default)]
pub struct RelAccumulator {
    x: f64,
    y: f64,
}

impl RelAccumulator {
    pub fn add(&mut self, dx: f64, dy: f64) {
        if dx.is_finite() && dy.is_finite() {
            self.x += dx;
            self.y += dy;
        }
    }

    pub fn take(&mut self) -> Option<(i16, i16)> {
        let (dx, dy) = (self.x.trunc(), self.y.trunc());
        if dx == 0.0 && dy == 0.0 {
            return None;
        }
        let clamp = |v: f64| v.clamp(i16::MIN as f64, i16::MAX as f64);
        let (sx, sy) = (clamp(dx), clamp(dy));
        self.x -= sx;
        self.y -= sy;
        Some((sx as i16, sy as i16))
    }
}

/// Converts `WheelEvent` deltas to 1/120-notch units: a line is 40 units (3 lines per
/// notch), 100 pixels one notch, a page three notches.
#[derive(Debug, Default)]
pub struct WheelAccumulator {
    inner: RelAccumulator,
}

impl WheelAccumulator {
    pub const UNITS_PER_LINE: f64 = 40.0;
    pub const UNITS_PER_PIXEL: f64 = 1.2;
    pub const UNITS_PER_PAGE: f64 = 360.0;

    /// `delta_mode` as in `WheelEvent.deltaMode`: 0 pixels, 1 lines, 2 pages.
    pub fn push(&mut self, dx: f64, dy: f64, delta_mode: u32) {
        let scale = match delta_mode {
            1 => Self::UNITS_PER_LINE,
            2 => Self::UNITS_PER_PAGE,
            _ => Self::UNITS_PER_PIXEL,
        };
        self.inner.add(dx * scale, dy * scale);
    }

    pub fn take(&mut self) -> Option<(i16, i16)> {
        self.inner.take()
    }
}

/// Decides when to send a controller snapshot: on change at most 120 times a second, plus
/// a heartbeat every 100 ms while it is not resting (and for a second after), so a lost
/// packet on the unreliable lane is corrected quickly and the host never sees a stale stick.
#[derive(Debug, Default)]
pub struct PadSampler {
    last: Option<PadState>,
    last_sent_ms: f64,
    active_until_ms: f64,
}

impl PadSampler {
    const MIN_INTERVAL_MS: f64 = 1000.0 / 120.0;
    const HEARTBEAT_MS: f64 = 100.0;
    const LINGER_MS: f64 = 1000.0;

    pub fn sample(&mut self, state: PadState, now_ms: f64) -> Option<PadState> {
        if !state.is_resting() {
            self.active_until_ms = now_ms + Self::LINGER_MS;
        }
        let since = now_ms - self.last_sent_ms;
        let changed = self.last != Some(state);
        let due = (changed && since >= Self::MIN_INTERVAL_MS)
            || (now_ms < self.active_until_ms && since >= Self::HEARTBEAT_MS);
        if due {
            self.last = Some(state);
            self.last_sent_ms = now_ms;
        }
        due.then_some(state)
    }
}

/// Token bucket limiting how many events a member may send.
#[derive(Debug)]
pub struct InputBudget {
    tokens: f64,
    last_ms: f64,
}

impl InputBudget {
    const PER_SECOND: f64 = 1000.0;
    const BURST: f64 = 2000.0;

    pub fn new(now_ms: f64) -> Self {
        Self { tokens: Self::BURST, last_ms: now_ms }
    }

    pub fn take(&mut self, events: usize, now_ms: f64) -> bool {
        let elapsed = (now_ms - self.last_ms).max(0.0) / 1000.0;
        self.tokens = (self.tokens + elapsed * Self::PER_SECOND).min(Self::BURST);
        self.last_ms = now_ms;
        if self.tokens >= events as f64 {
            self.tokens -= events as f64;
            true
        } else {
            false
        }
    }
}

/// A browser gamepad in the "standard" layout as a `PadState`: `buttons` as (pressed,
/// value) by standard index (6 and 7, the triggers, use their analog value) and `axes` as
/// -1.0..=1.0 (left X, left Y, right X, right Y).
pub fn pad_state_from(buttons: &[(bool, f64)], axes: &[f64]) -> PadState {
    let mut state = PadState::NEUTRAL;
    let analog = |v: f64| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
    for (i, (pressed, value)) in buttons.iter().enumerate().take(17) {
        match i {
            6 => state.triggers[0] = analog(*value),
            7 => state.triggers[1] = analog(*value),
            _ if *pressed => state.buttons |= 1 << i,
            _ => {}
        }
    }
    for (i, v) in axes.iter().enumerate().take(4) {
        state.axes[i] = if v.is_finite() { (v.clamp(-1.0, 1.0) * 32767.0).round() as i16 } else { 0 };
    }
    state
}

/// Ctrl+Alt+Shift+Q: the viewer stops controlling, whatever is focused.
pub fn is_release_chord(ctrl: bool, alt: bool, shift: bool, code: &str) -> bool {
    ctrl && alt && shift && code == "KeyQ"
}

/// Map a position inside the shared picture (fractions 0..=1) to the wire range.
pub fn normalize_position(fx: f64, fy: f64) -> (u16, u16) {
    let scale = |f: f64| if f.is_finite() { (f.clamp(0.0, 1.0) * 65535.0).round() as u16 } else { 0 };
    (scale(fx), scale(fy))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One sample of every event (also part of the wire fingerprint in `version.rs`).
    pub(crate) fn samples() -> Vec<InputEvent> {
        vec![
            InputEvent::PointerAbs { x: 0, y: 65535 },
            InputEvent::PointerRel { dx: i16::MIN, dy: i16::MAX },
            InputEvent::Button { button: MouseButton::Right, down: true, at: Some((1, 2)) },
            InputEvent::Button { button: MouseButton::Forward, down: false, at: None },
            InputEvent::Wheel { dx: -120, dy: 240, at: Some((65535, 0)) },
            InputEvent::Key { code: DomCode::KeyW, down: true, repeat: true },
            InputEvent::Pad {
                index: 0,
                state: PadState { buttons: 1 | 1 << 16, axes: [i16::MIN, -1, 0, i16::MAX], triggers: [0, 255] },
            },
            InputEvent::PadGone { index: 1 },
            InputEvent::Alive,
            InputEvent::ReleaseAll,
            InputEvent::Mode { mode: PointerMode::Game },
        ]
    }

    #[test]
    fn test_every_event_round_trips_in_binary_and_json() {
        let events = samples();
        assert_eq!(decode_events(&encode_events(&events).unwrap()).unwrap(), events);
        for ev in &events {
            let json = serde_json::to_string(ev).unwrap();
            assert_eq!(&serde_json::from_str::<InputEvent>(&json).unwrap(), ev);
        }
        let key = serde_json::to_string(&InputEvent::Key { code: DomCode::KeyA, down: true, repeat: false }).unwrap();
        assert_eq!(key, r#"{"k":"Key","code":"KeyA","down":true,"repeat":false}"#);
    }

    #[test]
    fn test_malformed_and_oversized_input_is_rejected() {
        let good = encode_events(&[InputEvent::Key { code: DomCode::KeyA, down: true, repeat: false }]).unwrap();
        assert_eq!(decode_events(&good[..good.len() - 1]), Err(InputError::Malformed), "truncated");
        assert_eq!(decode_events(&[good.as_slice(), &[0]].concat()), Err(InputError::Malformed), "trailing");
        assert_eq!(decode_events(&[1, 99]), Err(InputError::Malformed), "unknown tag");
        assert_eq!(decode_events(&[1, TAG_KEY, 0x00, 0x32, 1]), Err(InputError::Malformed), "unknown key");
        assert_eq!(decode_events(&[1, TAG_BUTTON, 7, 1, 0, 0, 0, 0, 0]), Err(InputError::Malformed), "unknown button");
        assert_eq!(decode_events(&[1, TAG_MODE, 5]), Err(InputError::Malformed));
        assert_eq!(decode_events(&[]), Err(InputError::Malformed));
        assert_eq!(decode_events(&[33]), Err(InputError::TooLarge));
        assert_eq!(encode_events(&[InputEvent::Alive; 33]), Err(InputError::TooLarge));
        let pad_with_trigger_bits = [1, TAG_PAD, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_events(&pad_with_trigger_bits), Err(InputError::Malformed));
    }

    #[test]
    fn test_packets_open_only_for_their_link_direction_and_lane() {
        let key = [3u8; 32];
        let events = [InputEvent::Key { code: DomCode::Enter, down: true, repeat: false }, InputEvent::Alive];
        let packet = seal_input(&key, "bo", "ana", InputLane::Events, 7, &events).unwrap();
        assert_eq!(open_input(&key, "bo", "ana", &packet).unwrap(), (InputLane::Events, 7, events.to_vec()));

        assert_eq!(open_input(&[4u8; 32], "bo", "ana", &packet), Err(InputError::Unauthentic), "wrong key");
        assert_eq!(open_input(&key, "cy", "ana", &packet), Err(InputError::Unauthentic), "wrong sender");
        assert_eq!(open_input(&key, "ana", "bo", &packet), Err(InputError::Unauthentic), "reflected");
        for (i, value) in [(0usize, 1u8), (4, 8)] {
            let mut tampered = packet.clone();
            tampered[i] = value;
            assert_eq!(open_input(&key, "bo", "ana", &tampered), Err(InputError::Unauthentic), "header byte {i}");
        }
        assert_eq!(open_input(&key, "bo", "ana", &packet[..20]), Err(InputError::Malformed));

        let pointer = [InputEvent::PointerAbs { x: 1, y: 1 }];
        assert_eq!(seal_input(&key, "bo", "ana", InputLane::Events, 1, &pointer), Err(InputError::WrongLane));
        assert!(seal_input(&key, "bo", "ana", InputLane::State, 1, &pointer).is_ok());
    }

    #[test]
    fn test_sequencer_keeps_the_newest_across_wraparound() {
        let mut seq = InputSequencer::default();
        assert!(seq.accept(10));
        assert!(!seq.accept(10));
        assert!(!seq.accept(9));
        assert!(seq.accept(11));
        let mut wrapping = InputSequencer::default();
        assert!(wrapping.accept(u32::MAX));
        assert!(wrapping.accept(0));
        assert!(!wrapping.accept(u32::MAX - 1));
    }

    #[test]
    fn test_accumulators_carry_remainders_and_clamp() {
        let mut rel = RelAccumulator::default();
        rel.add(0.6, -0.6);
        assert_eq!(rel.take(), None);
        rel.add(0.6, -0.6);
        assert_eq!(rel.take(), Some((1, -1)));
        rel.add(f64::NAN, 1.0);
        rel.add(100_000.0, 0.0);
        assert_eq!(rel.take(), Some((i16::MAX, 0)));
        assert_eq!(rel.take(), Some((i16::MAX, 0)), "the rest follows");

        let mut wheel = WheelAccumulator::default();
        wheel.push(0.0, 3.0, 1);
        assert_eq!(wheel.take(), Some((0, 120)), "three lines are one notch");
        wheel.push(0.0, -100.0, 0);
        assert_eq!(wheel.take(), Some((0, -120)), "100 pixels are one notch");
    }

    #[test]
    fn test_pad_sampler_rate_and_heartbeat() {
        let mut sampler = PadSampler::default();
        let pressed = PadState { buttons: 1, ..PadState::NEUTRAL };
        assert_eq!(sampler.sample(pressed, 1000.0), Some(pressed));
        assert_eq!(sampler.sample(pressed, 1005.0), None, "unchanged");
        assert_eq!(sampler.sample(PadState::NEUTRAL, 1005.0), None, "changed, but within 1/120 s");
        assert_eq!(sampler.sample(PadState::NEUTRAL, 1010.0), Some(PadState::NEUTRAL));
        assert_eq!(sampler.sample(PadState::NEUTRAL, 1050.0), None);
        assert_eq!(sampler.sample(PadState::NEUTRAL, 1110.0), Some(PadState::NEUTRAL), "heartbeat after activity");
        assert_eq!(sampler.sample(PadState::NEUTRAL, 2500.0), None, "quiet once resting for a while");
        let drifting = PadState { axes: [900, -700, 0, 0], ..PadState::NEUTRAL };
        assert!(drifting.is_resting(), "small stick drift counts as resting");
    }

    #[test]
    fn test_browser_gamepads_become_pad_states() {
        let mut buttons = vec![(false, 0.0); 17];
        buttons[0] = (true, 1.0); // A
        buttons[7] = (true, 0.5); // right trigger, half
        buttons[16] = (true, 1.0); // guide
        let state = pad_state_from(&buttons, &[1.0, -1.0, 0.25, f64::NAN, 9.0]);
        assert_eq!(state.buttons, 1 | 1 << 16);
        assert_eq!(state.triggers, [0, 128]);
        assert_eq!(state.axes, [32767, -32767, 8192, 0]);
        assert_eq!(state.buttons & !PAD_BUTTON_MASK, 0, "no trigger bits");
        assert_eq!(pad_state_from(&[], &[]), PadState::NEUTRAL);
    }

    #[test]
    fn test_budget_and_helpers() {
        let mut budget = InputBudget::new(0.0);
        assert!(budget.take(2000, 0.0));
        assert!(!budget.take(1, 0.0));
        assert!(budget.take(500, 500.0));
        assert!(is_release_chord(true, true, true, "KeyQ"));
        assert!(!is_release_chord(true, false, true, "KeyQ"));
        assert_eq!(normalize_position(0.5, 2.0), (32768, 65535));
        assert_eq!(normalize_position(-1.0, f64::NAN), (0, 0));
    }
}
