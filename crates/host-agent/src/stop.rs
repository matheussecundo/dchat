//! The global stop shortcut: whoever sits at this computer can end remote control at any
//! time, whichever window has focus. Windows and X11 only: Wayland desktops don't let an
//! ordinary program watch keys globally (Enter in the terminal, Ctrl+C and Stop in dchat
//! still work there).

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::mpsc;
use std::time::Duration;

pub const STOP_SHORTCUT: &str = "Ctrl+Alt+Shift+Q";

fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
}

/// Watch for the stop shortcut on its own thread. `Err` says why it isn't available.
pub fn watch(on_stop: impl Fn() + Send + Sync + 'static) -> Result<(), String> {
    if cfg!(target_os = "linux") && wayland_session() {
        return Err("not available on Wayland desktops".into());
    }
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed {
            on_stop();
        }
    }));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("dchat-host-stop-shortcut".into())
        .spawn(move || {
            // The manager must live on the thread that runs its event loop (Windows).
            let manager = match GlobalHotKeyManager::new() {
                Ok(manager) => manager,
                Err(err) => {
                    let _ = ready_tx.send(Err(err.to_string()));
                    return;
                }
            };
            let shortcut = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT), Code::KeyQ);
            if let Err(err) = manager.register(shortcut) {
                let _ = ready_tx.send(Err(format!("{STOP_SHORTCUT} is taken by another program ({err})")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            run_event_loop();
            drop(manager);
        })
        .map_err(|err| err.to_string())?;
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| Err("no answer from the desktop".into()))
}

#[cfg(windows)]
fn run_event_loop() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn run_event_loop() {
    // X11: the crate reads events on its own thread; keep the manager alive.
    loop {
        std::thread::park();
    }
}
