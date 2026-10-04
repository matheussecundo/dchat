//! Linux: virtual input devices through uinput, which works the same on X11 and Wayland.
//! Needs write access to /dev/uinput (the udev rule in `dist/60-dchat-host.rules`).
//!
//! Three devices: a keyboard, an absolute pointer for desktop mode (like a VM "tablet") and
//! a relative mouse for game mode, so absolute and relative motion never mix on one device.

use super::{Caps, Injector};
use crate::keymap;
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent as EvEvent, InputId, KeyCode,
    RelativeAxisCode, UinputAbsSetup,
};
use protocol::{DomCode, MouseButton, PadState, PointerMode};
use std::io;

const VENDOR: u16 = 0x1209;
const NOTCH: i32 = 120;

struct Devices {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    mouse: VirtualDevice,
}

pub struct UinputInjector {
    devices: Result<Devices, String>,
    /// Low-resolution wheel remainders (high-resolution units not yet a whole notch).
    wheel_rest: (i32, i32),
}

impl UinputInjector {
    pub fn new() -> Self {
        Self { devices: create_devices().map_err(|err| explain(&err)), wheel_rest: (0, 0) }
    }

    fn devices(&mut self) -> io::Result<&mut Devices> {
        self.devices.as_mut().map_err(|err| io::Error::other(err.clone()))
    }
}

impl Default for UinputInjector {
    fn default() -> Self {
        Self::new()
    }
}

fn explain(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::PermissionDenied => "no permission to use /dev/uinput: install dist/60-dchat-host.rules \
            (see the README), then log out and back in"
            .into(),
        io::ErrorKind::NotFound => "/dev/uinput is missing: run `sudo modprobe uinput` \
            (and add `uinput` to /etc/modules-load.d/ to keep it)"
            .into(),
        _ => format!("cannot create virtual input devices: {err}"),
    }
}

fn button_code(button: MouseButton) -> KeyCode {
    KeyCode(match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Back => 0x113,
        MouseButton::Forward => 0x114,
    })
}

fn buttons() -> AttributeSet<KeyCode> {
    let mut set = AttributeSet::<KeyCode>::new();
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward] {
        set.insert(button_code(button));
    }
    set
}

fn wheels(extra: &[RelativeAxisCode]) -> AttributeSet<RelativeAxisCode> {
    let mut set = AttributeSet::<RelativeAxisCode>::new();
    for axis in [
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_WHEEL_HI_RES,
        RelativeAxisCode::REL_HWHEEL_HI_RES,
    ]
    .iter()
    .chain(extra)
    {
        set.insert(*axis);
    }
    set
}

fn create_devices() -> io::Result<Devices> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for &code in DomCode::ALL {
        keys.insert(KeyCode(keymap::evdev(code)));
    }
    let keyboard = VirtualDevice::builder()?
        .name("dchat-host keyboard")
        .input_id(InputId::new(BusType::BUS_USB, VENDOR, 0x0001, 1))
        .with_keys(&keys)?
        .build()?;

    let axis = |code| UinputAbsSetup::new(code, AbsInfo::new(0, 0, 65535, 0, 0, 0));
    let pointer = VirtualDevice::builder()?
        .name("dchat-host pointer")
        .input_id(InputId::new(BusType::BUS_USB, VENDOR, 0x0002, 1))
        .with_keys(&buttons())?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))?
        .with_relative_axes(&wheels(&[]))?
        .build()?;

    let mouse = VirtualDevice::builder()?
        .name("dchat-host mouse")
        .input_id(InputId::new(BusType::BUS_USB, VENDOR, 0x0003, 1))
        .with_keys(&buttons())?
        .with_relative_axes(&wheels(&[RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y]))?
        .build()?;

    Ok(Devices { keyboard, pointer, mouse })
}

fn ev(kind: EventType, code: u16, value: i32) -> EvEvent {
    EvEvent::new(kind.0, code, value)
}

impl Injector for UinputInjector {
    fn caps(&self) -> Caps {
        Caps {
            mouse_keyboard: self.devices.as_ref().map(|_| ()).map_err(Clone::clone),
            pads: Err("controllers are not supported by this version of dchat-host yet".into()),
            software_repeat: false,
        }
    }

    fn move_abs(&mut self, x: u16, y: u16) -> io::Result<()> {
        let pointer = &mut self.devices()?.pointer;
        pointer.emit(&[
            ev(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, x as i32),
            ev(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, y as i32),
        ])
    }

    fn move_rel(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        let mouse = &mut self.devices()?.mouse;
        mouse.emit(&[
            ev(EventType::RELATIVE, RelativeAxisCode::REL_X.0, dx),
            ev(EventType::RELATIVE, RelativeAxisCode::REL_Y.0, dy),
        ])
    }

    fn button(&mut self, mode: PointerMode, button: MouseButton, down: bool) -> io::Result<()> {
        let devices = self.devices()?;
        let device = if mode == PointerMode::Game { &mut devices.mouse } else { &mut devices.pointer };
        device.emit(&[ev(EventType::KEY, button_code(button).0, down as i32)])
    }

    fn wheel(&mut self, mode: PointerMode, dx: i32, dy: i32) -> io::Result<()> {
        // evdev: positive REL_WHEEL scrolls up, positive REL_HWHEEL right.
        let (rest_x, rest_y) = (self.wheel_rest.0 + dx, self.wheel_rest.1 - dy);
        let (notches_x, notches_y) = (rest_x / NOTCH, rest_y / NOTCH);
        self.wheel_rest = (rest_x - notches_x * NOTCH, rest_y - notches_y * NOTCH);
        let mut events = Vec::with_capacity(4);
        if dy != 0 {
            events.push(ev(EventType::RELATIVE, RelativeAxisCode::REL_WHEEL_HI_RES.0, -dy));
        }
        if dx != 0 {
            events.push(ev(EventType::RELATIVE, RelativeAxisCode::REL_HWHEEL_HI_RES.0, dx));
        }
        if notches_y != 0 {
            events.push(ev(EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, notches_y));
        }
        if notches_x != 0 {
            events.push(ev(EventType::RELATIVE, RelativeAxisCode::REL_HWHEEL.0, notches_x));
        }
        if events.is_empty() {
            return Ok(());
        }
        let devices = self.devices()?;
        let device = if mode == PointerMode::Game { &mut devices.mouse } else { &mut devices.pointer };
        device.emit(&events)
    }

    fn key(&mut self, code: DomCode, down: bool) -> io::Result<()> {
        let keyboard = &mut self.devices()?.keyboard;
        keyboard.emit(&[ev(EventType::KEY, keymap::evdev(code), down as i32)])
    }

    fn pad_plug(&mut self, _slot: u8) -> io::Result<()> {
        Err(io::Error::other("controllers are not supported by this version of dchat-host yet"))
    }

    fn pad_update(&mut self, _slot: u8, _state: &PadState) -> io::Result<()> {
        Ok(())
    }

    fn pad_unplug(&mut self, _slot: u8) {}

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
