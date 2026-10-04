//! Records what would have been injected (tests and `--mock-injector`).

use super::{Caps, Injector};
use protocol::{DomCode, MouseButton, PadState, PointerMode};
use serde::Serialize;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "k")]
pub enum Injected {
    MoveAbs { x: u16, y: u16 },
    MoveRel { dx: i32, dy: i32 },
    Button { mode: PointerMode, button: MouseButton, down: bool },
    Wheel { dx: i32, dy: i32 },
    Key { code: DomCode, down: bool },
    PadPlug { slot: u8 },
    PadUpdate { slot: u8, state: PadState },
    PadUnplug { slot: u8 },
}

pub type Recording = Arc<Mutex<Vec<Injected>>>;

pub struct Recorder {
    pub log: Recording,
    /// Print each event as a JSON line (`--print-events`).
    pub print: bool,
    pub pads: u8,
    pub software_repeat: bool,
}

impl Recorder {
    pub fn new(pads: u8) -> Self {
        Self { log: Recording::default(), print: false, pads, software_repeat: false }
    }

    fn record(&mut self, event: Injected) {
        if self.print {
            if let Ok(line) = serde_json::to_string(&event) {
                println!("{line}");
            }
        }
        self.log.lock().expect("recording lock").push(event);
    }
}

impl Injector for Recorder {
    fn caps(&self) -> Caps {
        Caps {
            mouse_keyboard: Ok(()),
            pads: if self.pads > 0 { Ok(self.pads) } else { Err("controllers disabled (mock)".into()) },
            software_repeat: self.software_repeat,
        }
    }
    fn move_abs(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.record(Injected::MoveAbs { x, y });
        Ok(())
    }
    fn move_rel(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        self.record(Injected::MoveRel { dx, dy });
        Ok(())
    }
    fn button(&mut self, mode: PointerMode, button: MouseButton, down: bool) -> io::Result<()> {
        self.record(Injected::Button { mode, button, down });
        Ok(())
    }
    fn wheel(&mut self, _mode: PointerMode, dx: i32, dy: i32) -> io::Result<()> {
        self.record(Injected::Wheel { dx, dy });
        Ok(())
    }
    fn key(&mut self, code: DomCode, down: bool) -> io::Result<()> {
        self.record(Injected::Key { code, down });
        Ok(())
    }
    fn pad_plug(&mut self, slot: u8) -> io::Result<()> {
        self.record(Injected::PadPlug { slot });
        Ok(())
    }
    fn pad_update(&mut self, slot: u8, state: &PadState) -> io::Result<()> {
        self.record(Injected::PadUpdate { slot, state: *state });
        Ok(())
    }
    fn pad_unplug(&mut self, slot: u8) {
        self.record(Injected::PadUnplug { slot });
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
