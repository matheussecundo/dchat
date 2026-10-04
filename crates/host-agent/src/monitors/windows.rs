//! Monitor layout on Windows, in physical pixels (dchat-host is per-monitor DPI aware).

use crate::geometry::{Monitor, Rect};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};

const MONITORINFOF_PRIMARY: u32 = 1;

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let found = &mut *(data as *mut Vec<Monitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
        let r = info.monitorInfo.rcMonitor;
        let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        found.push(Monitor {
            id: found.len() as u32,
            name: String::from_utf16_lossy(&info.szDevice[..len]),
            rect: Rect { x: r.left, y: r.top, w, h },
            pixels: (w, h),
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    1
}

pub fn monitors() -> Vec<Monitor> {
    let mut found: Vec<Monitor> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(collect), &mut found as *mut Vec<Monitor> as LPARAM);
    }
    found
}
