//! A standard-layout controller state as an Xbox 360 pad: the XInput report ViGEm takes
//! on Windows, and the events of the Linux kernel's own Xbox 360 driver (xpad), which a
//! uinput copy of it must emit so games and SDL recognize it. Pure and unit-tested.

use protocol::PadState;

/// Standard layout button indices.
const A: u32 = 0;
const B: u32 = 1;
const X: u32 = 2;
const Y: u32 = 3;
const LB: u32 = 4;
const RB: u32 = 5;
const BACK: u32 = 8;
const START: u32 = 9;
const LS: u32 = 10;
const RS: u32 = 11;
const UP: u32 = 12;
const DOWN: u32 = 13;
const LEFT: u32 = 14;
const RIGHT: u32 = 15;
const GUIDE: u32 = 16;

fn pressed(state: &PadState, button: u32) -> bool {
    state.buttons & (1 << button) != 0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

/// XInput: Y axes point up (browsers: down), so they are flipped.
pub fn xinput(state: &PadState) -> XReport {
    const BITS: [(u32, u16); 15] = [
        (UP, 0x0001),
        (DOWN, 0x0002),
        (LEFT, 0x0004),
        (RIGHT, 0x0008),
        (START, 0x0010),
        (BACK, 0x0020),
        (LS, 0x0040),
        (RS, 0x0080),
        (LB, 0x0100),
        (RB, 0x0200),
        (GUIDE, 0x0400),
        (A, 0x1000),
        (B, 0x2000),
        (X, 0x4000),
        (Y, 0x8000),
    ];
    let flip = |v: i16| v.checked_neg().unwrap_or(i16::MAX);
    XReport {
        buttons: BITS.iter().filter(|(b, _)| pressed(state, *b)).fold(0, |acc, (_, bit)| acc | bit),
        left_trigger: state.triggers[0],
        right_trigger: state.triggers[1],
        lx: state.axes[0],
        ly: flip(state.axes[1]),
        rx: state.axes[2],
        ry: flip(state.axes[3]),
    }
}

pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

/// xpad's buttons: A, B, X, Y, LB, RB, Back, Start, Guide, left and right stick clicks.
pub const XPAD_KEYS: [(u32, u16); 11] = [
    (A, 0x130),
    (B, 0x131),
    (X, 0x133),
    (Y, 0x134),
    (LB, 0x136),
    (RB, 0x137),
    (BACK, 0x13a),
    (START, 0x13b),
    (GUIDE, 0x13c),
    (LS, 0x13d),
    (RS, 0x13e),
];

/// (type, code, value) for every button and axis; the kernel drops values that didn't
/// change. Y axes point down, as in browsers; the D-pad is a hat.
pub fn xpad_events(state: &PadState) -> Vec<(u16, u16, i32)> {
    let mut events: Vec<(u16, u16, i32)> =
        XPAD_KEYS.iter().map(|(button, code)| (EV_KEY, *code, pressed(state, *button) as i32)).collect();
    let axis = |b_neg: u32, b_pos: u32| pressed(state, b_pos) as i32 - pressed(state, b_neg) as i32;
    events.extend([
        (EV_ABS, ABS_X, state.axes[0] as i32),
        (EV_ABS, ABS_Y, state.axes[1] as i32),
        (EV_ABS, ABS_RX, state.axes[2] as i32),
        (EV_ABS, ABS_RY, state.axes[3] as i32),
        (EV_ABS, ABS_Z, state.triggers[0] as i32),
        (EV_ABS, ABS_RZ, state.triggers[1] as i32),
        (EV_ABS, ABS_HAT0X, axis(LEFT, RIGHT)),
        (EV_ABS, ABS_HAT0Y, axis(UP, DOWN)),
    ]);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(buttons: &[u32]) -> PadState {
        PadState { buttons: buttons.iter().fold(0, |acc, b| acc | 1 << b), ..PadState::NEUTRAL }
    }

    #[test]
    fn test_xinput_report() {
        assert_eq!(xinput(&PadState::NEUTRAL), XReport::default());
        assert_eq!(xinput(&with(&[A, Y, UP, GUIDE])).buttons, 0x1000 | 0x8000 | 0x0001 | 0x0400);
        let state = PadState { axes: [100, 200, -300, i16::MIN], triggers: [10, 255], ..PadState::NEUTRAL };
        let report = xinput(&state);
        assert_eq!((report.lx, report.ly, report.rx, report.ry), (100, -200, -300, i16::MAX), "Y flipped, no overflow");
        assert_eq!((report.left_trigger, report.right_trigger), (10, 255));
    }

    #[test]
    fn test_xpad_events() {
        let events = xpad_events(&with(&[A, X, LEFT, DOWN]));
        let value = |kind, code| events.iter().find(|(k, c, _)| *k == kind && *c == code).map(|e| e.2);
        assert_eq!(value(EV_KEY, 0x130), Some(1), "A");
        assert_eq!(value(EV_KEY, 0x133), Some(1), "X is xpad's BTN_X");
        assert_eq!(value(EV_KEY, 0x131), Some(0));
        assert_eq!(value(EV_ABS, ABS_HAT0X), Some(-1), "D-pad left");
        assert_eq!(value(EV_ABS, ABS_HAT0Y), Some(1), "D-pad down");
        let sticks = xpad_events(&PadState { axes: [1, 2, 3, 4], triggers: [5, 6], ..PadState::NEUTRAL });
        let abs: Vec<i32> = sticks.iter().filter(|e| e.0 == EV_ABS).map(|e| e.2).collect();
        assert_eq!(abs, vec![1, 2, 3, 4, 5, 6, 0, 0]);
    }
}
