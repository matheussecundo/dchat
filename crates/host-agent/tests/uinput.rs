//! The real Linux backend. Runs only where /dev/uinput is writable (the udev rule in
//! `dist/`); elsewhere it says so and passes.
#![cfg(target_os = "linux")]

use host_agent::inject::linux::UinputInjector;
use host_agent::inject::Injector;
use protocol::{DomCode, MouseButton, PointerMode};

#[test]
fn creates_devices_and_injects_when_uinput_is_available() {
    if std::fs::OpenOptions::new().write(true).open("/dev/uinput").is_err() {
        eprintln!("skipped: /dev/uinput is not writable here (install dist/60-dchat-host.rules)");
        return;
    }
    let mut injector = UinputInjector::new();
    if let Err(err) = injector.caps().mouse_keyboard {
        panic!("uinput is writable but the devices failed: {err}");
    }
    // Give the desktop a moment to pick the devices up, then harmless input only.
    std::thread::sleep(std::time::Duration::from_millis(300));
    injector.key(DomCode::ShiftLeft, true).unwrap();
    injector.key(DomCode::ShiftLeft, false).unwrap();
    injector.move_rel(1, -1).unwrap();
    injector.move_rel(-1, 1).unwrap();
    injector.wheel(PointerMode::Desktop, 0, 0).unwrap();
    injector.button(PointerMode::Game, MouseButton::Back, false).unwrap();
}
