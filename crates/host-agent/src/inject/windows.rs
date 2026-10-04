//! Windows: `SendInput` with scan codes (key positions) and absolute positions over the
//! whole virtual desktop. Windows can't be controlled while UAC or the lock screen is up,
//! and windows of elevated apps only when dchat-host runs as administrator too.

use super::win_input::{self, WinInput};
use super::{Caps, Injector};
use protocol::{DomCode, MouseButton, PadState, PointerMode};
use std::io;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, MOUSEINPUT,
};

#[derive(Default)]
pub struct SendInputInjector {
    pending: Vec<WinInput>,
}

impl SendInputInjector {
    pub fn new() -> Self {
        Self::default()
    }
}

fn to_input(input: &WinInput) -> INPUT {
    match *input {
        WinInput::Mouse { dx, dy, data, flags } => INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        },
        WinInput::Key { vk, scan, flags } => INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        },
    }
}

impl Injector for SendInputInjector {
    fn caps(&self) -> Caps {
        Caps {
            mouse_keyboard: Ok(()),
            pads: Err("controllers are not supported by this version of dchat-host yet".into()),
            // Windows does not repeat injected keys by itself.
            software_repeat: true,
        }
    }

    fn move_abs(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.pending.push(win_input::move_abs(x, y));
        Ok(())
    }

    fn move_rel(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        self.pending.push(win_input::move_rel(dx, dy));
        Ok(())
    }

    fn button(&mut self, _mode: PointerMode, button: MouseButton, down: bool) -> io::Result<()> {
        self.pending.push(win_input::button(button, down));
        Ok(())
    }

    fn wheel(&mut self, _mode: PointerMode, dx: i32, dy: i32) -> io::Result<()> {
        self.pending.extend(win_input::wheel(dx, dy));
        Ok(())
    }

    fn key(&mut self, code: DomCode, down: bool) -> io::Result<()> {
        self.pending.push(win_input::key(code, down));
        Ok(())
    }

    fn pad_plug(&mut self, _slot: u8) -> io::Result<()> {
        Err(io::Error::other("controllers are not supported by this version of dchat-host yet"))
    }

    fn pad_update(&mut self, _slot: u8, _state: &PadState) -> io::Result<()> {
        Ok(())
    }

    fn pad_unplug(&mut self, _slot: u8) {}

    /// One `SendInput` per batch, so a click and its position arrive together.
    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let inputs: Vec<INPUT> = self.pending.drain(..).map(|i| to_input(&i)).collect();
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if (sent as usize) < inputs.len() {
            return Err(io::Error::other(
                "Windows blocked the input (an administrator window, UAC or the lock screen has focus)",
            ));
        }
        Ok(())
    }
}

/// Positions in physical pixels on every monitor: call before anything else.
pub fn use_physical_pixels() {
    use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Whether dchat-host runs as administrator (only then can it control elevated windows).
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}
