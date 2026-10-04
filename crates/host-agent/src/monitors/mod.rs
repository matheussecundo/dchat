//! The monitors of this computer, in the coordinate space absolute injection uses.

use crate::geometry::Monitor;

#[cfg(target_os = "linux")]
mod x11;
#[cfg(windows)]
mod windows;

/// The monitor layout, or nothing when it can't be read (then positions are mapped onto
/// the whole desktop, which is right for a single monitor).
pub fn detect() -> Vec<Monitor> {
    #[cfg(target_os = "linux")]
    {
        // RandR answers on X11 and, through XWayland, in most Wayland sessions too.
        x11::monitors().unwrap_or_default()
    }
    #[cfg(windows)]
    {
        windows::monitors()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Vec::new()
    }
}
