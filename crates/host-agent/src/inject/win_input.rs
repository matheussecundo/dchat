//! What dchat-host hands to Windows' `SendInput`, built as plain data so it is unit-tested
//! on every platform. Flag values are the Win32 constants.

use crate::keymap;
use protocol::{DomCode, MouseButton};

pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
pub const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
pub const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
pub const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
pub const MOUSEEVENTF_XDOWN: u32 = 0x0080;
pub const MOUSEEVENTF_XUP: u32 = 0x0100;
pub const MOUSEEVENTF_WHEEL: u32 = 0x0800;
pub const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
pub const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
pub const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
pub const XBUTTON1: u32 = 0x0001;
pub const XBUTTON2: u32 = 0x0002;
pub const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
pub const KEYEVENTF_KEYUP: u32 = 0x0002;
pub const KEYEVENTF_SCANCODE: u32 = 0x0008;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinInput {
    Mouse { dx: i32, dy: i32, data: u32, flags: u32 },
    Key { vk: u16, scan: u16, flags: u32 },
}

/// 0..=65535 over the whole virtual desktop (all monitors), like our positions.
pub fn move_abs(x: u16, y: u16) -> WinInput {
    WinInput::Mouse {
        dx: x as i32,
        dy: y as i32,
        data: 0,
        flags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    }
}

pub fn move_rel(dx: i32, dy: i32) -> WinInput {
    WinInput::Mouse { dx, dy, data: 0, flags: MOUSEEVENTF_MOVE }
}

pub fn button(button: MouseButton, down: bool) -> WinInput {
    let (flags, data) = match (button, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
    };
    WinInput::Mouse { dx: 0, dy: 0, data, flags }
}

/// Deltas in 1/120 notch with positive = down/right. Windows: positive wheel scrolls up
/// (away from the user), positive horizontal wheel scrolls right.
pub fn wheel(dx: i32, dy: i32) -> Vec<WinInput> {
    let mut out = Vec::new();
    if dy != 0 {
        out.push(WinInput::Mouse { dx: 0, dy: 0, data: (-dy) as u32, flags: MOUSEEVENTF_WHEEL });
    }
    if dx != 0 {
        out.push(WinInput::Mouse { dx: 0, dy: 0, data: dx as u32, flags: MOUSEEVENTF_HWHEEL });
    }
    out
}

/// By scan code (the key's position), except where only a virtual key works (Pause).
pub fn key(code: DomCode, down: bool) -> WinInput {
    let key = keymap::windows(code);
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    match key.vk {
        Some(vk) => WinInput::Key { vk, scan: 0, flags: up },
        None => WinInput::Key {
            vk: 0,
            scan: key.scan,
            flags: KEYEVENTF_SCANCODE | up | if key.extended { KEYEVENTF_EXTENDEDKEY } else { 0 },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mouse_records() {
        assert_eq!(move_abs(65535, 0), WinInput::Mouse { dx: 65535, dy: 0, data: 0, flags: 0xC001 });
        assert_eq!(move_rel(-3, 4), WinInput::Mouse { dx: -3, dy: 4, data: 0, flags: 0x0001 });
        assert_eq!(button(MouseButton::Back, true), WinInput::Mouse { dx: 0, dy: 0, data: 1, flags: 0x0080 });
        assert_eq!(button(MouseButton::Forward, false), WinInput::Mouse { dx: 0, dy: 0, data: 2, flags: 0x0100 });
        // Scrolling down one notch is a negative Windows wheel delta.
        assert_eq!(wheel(0, 120), vec![WinInput::Mouse { dx: 0, dy: 0, data: (-120i32) as u32, flags: 0x0800 }]);
        assert_eq!(wheel(240, 0), vec![WinInput::Mouse { dx: 0, dy: 0, data: 240, flags: 0x1000 }]);
        assert!(wheel(0, 0).is_empty());
    }

    #[test]
    fn test_key_records() {
        assert_eq!(key(DomCode::KeyA, true), WinInput::Key { vk: 0, scan: 0x1E, flags: KEYEVENTF_SCANCODE });
        assert_eq!(
            key(DomCode::ArrowUp, false),
            WinInput::Key { vk: 0, scan: 0x48, flags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY }
        );
        assert_eq!(key(DomCode::Pause, true), WinInput::Key { vk: keymap::VK_PAUSE, scan: 0, flags: 0 });
        assert_eq!(key(DomCode::Pause, false), WinInput::Key { vk: keymap::VK_PAUSE, scan: 0, flags: KEYEVENTF_KEYUP });
    }
}
