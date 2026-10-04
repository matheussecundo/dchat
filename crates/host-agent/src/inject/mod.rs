//! Turning input events into real OS input. One backend per platform, plus a recorder for
//! tests (`--mock-injector`).

pub mod mock;
#[cfg(target_os = "linux")]
pub mod linux;

use protocol::{AgentCaps, DomCode, MouseButton, PadState, PointerMode};
use std::io;

/// What a backend can do here, with the reason when something is unavailable.
#[derive(Clone, Debug)]
pub struct Caps {
    pub mouse_keyboard: Result<(), String>,
    pub pads: Result<u8, String>,
    /// The OS doesn't repeat held injected keys itself (Windows): forward repeats.
    pub software_repeat: bool,
}

impl Caps {
    pub fn to_agent_caps(&self) -> AgentCaps {
        AgentCaps {
            mouse_keyboard: self.mouse_keyboard.is_ok(),
            mouse_keyboard_error: self.mouse_keyboard.clone().err(),
            pads: *self.pads.as_ref().unwrap_or(&0),
            pads_error: self.pads.clone().err(),
        }
    }
}

/// Positions are 0..=65535 over the whole desktop (the bounding box of all monitors).
/// Wheel deltas are in 1/120 of a notch, positive = right/down.
pub trait Injector: Send {
    fn caps(&self) -> Caps;
    fn move_abs(&mut self, x: u16, y: u16) -> io::Result<()>;
    fn move_rel(&mut self, dx: i32, dy: i32) -> io::Result<()>;
    fn button(&mut self, mode: PointerMode, button: MouseButton, down: bool) -> io::Result<()>;
    fn wheel(&mut self, mode: PointerMode, dx: i32, dy: i32) -> io::Result<()>;
    fn key(&mut self, code: DomCode, down: bool) -> io::Result<()>;
    fn pad_plug(&mut self, slot: u8) -> io::Result<()>;
    fn pad_update(&mut self, slot: u8, state: &PadState) -> io::Result<()>;
    fn pad_unplug(&mut self, slot: u8);
    /// Hand the batch to the OS (Windows: one `SendInput`; uinput: already sent).
    fn flush(&mut self) -> io::Result<()>;
}

/// Where nothing can be injected (unsupported OS, or the backend failed to start).
pub struct Unavailable(pub String);

impl Injector for Unavailable {
    fn caps(&self) -> Caps {
        Caps { mouse_keyboard: Err(self.0.clone()), pads: Err(self.0.clone()), software_repeat: false }
    }
    fn move_abs(&mut self, _: u16, _: u16) -> io::Result<()> {
        Ok(())
    }
    fn move_rel(&mut self, _: i32, _: i32) -> io::Result<()> {
        Ok(())
    }
    fn button(&mut self, _: PointerMode, _: MouseButton, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn wheel(&mut self, _: PointerMode, _: i32, _: i32) -> io::Result<()> {
        Ok(())
    }
    fn key(&mut self, _: DomCode, _: bool) -> io::Result<()> {
        Ok(())
    }
    fn pad_plug(&mut self, _: u8) -> io::Result<()> {
        Err(io::Error::other(self.0.clone()))
    }
    fn pad_update(&mut self, _: u8, _: &PadState) -> io::Result<()> {
        Ok(())
    }
    fn pad_unplug(&mut self, _: u8) {}
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// This platform's real backend.
pub fn platform_injector() -> Box<dyn Injector> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::UinputInjector::new())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(Unavailable("dchat-host can't inject input on this operating system yet".into()))
    }
}
