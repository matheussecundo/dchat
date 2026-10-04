//! Physical keys, named like `KeyboardEvent.code` and carried on the wire as their USB HID
//! usage ID (page 0x07). Keys are positional: the controlled computer's own keyboard layout
//! decides which character a key types. Media and launch keys are left out for now.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! dom_codes {
    ($($name:ident = $hid:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum DomCode { $($name),* }

        impl DomCode {
            pub const ALL: &'static [DomCode] = &[$(DomCode::$name),*];

            /// USB HID usage ID (keyboard page).
            pub fn hid(self) -> u16 {
                match self { $(DomCode::$name => $hid),* }
            }

            pub fn from_hid(hid: u16) -> Option<Self> {
                match hid { $($hid => Some(DomCode::$name),)* _ => None }
            }

            /// The `KeyboardEvent.code` name.
            pub fn as_code(self) -> &'static str {
                match self { $(DomCode::$name => stringify!($name)),* }
            }

            pub fn from_code(code: &str) -> Option<Self> {
                match code { $(stringify!($name) => Some(DomCode::$name),)* _ => None }
            }
        }
    };
}

dom_codes! {
    KeyA = 0x04, KeyB = 0x05, KeyC = 0x06, KeyD = 0x07, KeyE = 0x08, KeyF = 0x09, KeyG = 0x0A,
    KeyH = 0x0B, KeyI = 0x0C, KeyJ = 0x0D, KeyK = 0x0E, KeyL = 0x0F, KeyM = 0x10, KeyN = 0x11,
    KeyO = 0x12, KeyP = 0x13, KeyQ = 0x14, KeyR = 0x15, KeyS = 0x16, KeyT = 0x17, KeyU = 0x18,
    KeyV = 0x19, KeyW = 0x1A, KeyX = 0x1B, KeyY = 0x1C, KeyZ = 0x1D, Digit1 = 0x1E, Digit2 = 0x1F,
    Digit3 = 0x20, Digit4 = 0x21, Digit5 = 0x22, Digit6 = 0x23, Digit7 = 0x24, Digit8 = 0x25,
    Digit9 = 0x26, Digit0 = 0x27, Enter = 0x28, Escape = 0x29, Backspace = 0x2A, Tab = 0x2B,
    Space = 0x2C, Minus = 0x2D, Equal = 0x2E, BracketLeft = 0x2F, BracketRight = 0x30,
    Backslash = 0x31, Semicolon = 0x33, Quote = 0x34, Backquote = 0x35, Comma = 0x36, Period = 0x37,
    Slash = 0x38, CapsLock = 0x39, F1 = 0x3A, F2 = 0x3B, F3 = 0x3C, F4 = 0x3D, F5 = 0x3E, F6 = 0x3F,
    F7 = 0x40, F8 = 0x41, F9 = 0x42, F10 = 0x43, F11 = 0x44, F12 = 0x45, PrintScreen = 0x46,
    ScrollLock = 0x47, Pause = 0x48, Insert = 0x49, Home = 0x4A, PageUp = 0x4B, Delete = 0x4C,
    End = 0x4D, PageDown = 0x4E, ArrowRight = 0x4F, ArrowLeft = 0x50, ArrowDown = 0x51, ArrowUp = 0x52,
    NumLock = 0x53, NumpadDivide = 0x54, NumpadMultiply = 0x55, NumpadSubtract = 0x56,
    NumpadAdd = 0x57, NumpadEnter = 0x58, Numpad1 = 0x59, Numpad2 = 0x5A, Numpad3 = 0x5B,
    Numpad4 = 0x5C, Numpad5 = 0x5D, Numpad6 = 0x5E, Numpad7 = 0x5F, Numpad8 = 0x60, Numpad9 = 0x61,
    Numpad0 = 0x62, NumpadDecimal = 0x63, IntlBackslash = 0x64, ContextMenu = 0x65, NumpadEqual = 0x67,
    F13 = 0x68, F14 = 0x69, F15 = 0x6A, F16 = 0x6B, F17 = 0x6C, F18 = 0x6D, F19 = 0x6E, F20 = 0x6F,
    F21 = 0x70, F22 = 0x71, F23 = 0x72, F24 = 0x73, NumpadComma = 0x85, IntlRo = 0x87, KanaMode = 0x88,
    IntlYen = 0x89, Convert = 0x8A, NonConvert = 0x8B, Lang1 = 0x90, Lang2 = 0x91, ControlLeft = 0xE0,
    ShiftLeft = 0xE1, AltLeft = 0xE2, MetaLeft = 0xE3, ControlRight = 0xE4, ShiftRight = 0xE5,
    AltRight = 0xE6, MetaRight = 0xE7,
}

impl Serialize for DomCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_code())
    }
}

impl<'de> Deserialize<'de> for DomCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        DomCode::from_code(&code).ok_or_else(|| serde::de::Error::custom(format!("unknown key code {code}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_every_key_round_trips_by_name_and_hid() {
        for &key in DomCode::ALL {
            assert_eq!(DomCode::from_code(key.as_code()), Some(key));
            assert_eq!(DomCode::from_hid(key.hid()), Some(key));
            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(serde_json::from_str::<DomCode>(&json).unwrap(), key);
        }
        let hids: std::collections::HashSet<u16> = DomCode::ALL.iter().map(|k| k.hid()).collect();
        assert_eq!(hids.len(), DomCode::ALL.len(), "HID usages are unique");
    }

    #[test]
    fn test_known_keys() {
        assert_eq!(DomCode::KeyA.hid(), 0x04);
        assert_eq!(DomCode::from_code("ControlLeft").map(DomCode::hid), Some(0xE0));
        assert_eq!(DomCode::from_code("NumpadEnter").map(DomCode::hid), Some(0x58));
        assert_eq!(DomCode::from_code("AudioVolumeUp"), None, "media keys are not supported yet");
        assert_eq!(DomCode::from_code("keya"), None, "names are case-sensitive, like KeyboardEvent.code");
        assert_eq!(DomCode::from_hid(0x32), None);
        assert!(serde_json::from_str::<DomCode>("\"Bogus\"").is_err());
    }
}
