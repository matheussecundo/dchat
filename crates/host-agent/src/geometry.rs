//! Where on this computer a viewer's pointer lands. Pure and unit-tested.
//!
//! Viewers send positions normalized over the shared picture (0..=65535 on each axis). The
//! shared picture is one monitor; injection backends take absolute positions normalized
//! over the whole desktop (the bounding box of all monitors), as Windows' `VIRTUALDESK`
//! flag and the uinput absolute pointer both do.

use protocol::MonitorInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub id: u32,
    pub name: String,
    /// Position in the desktop's coordinate space (what absolute injection is relative to).
    pub rect: Rect,
    /// Physical pixel size (what the browser reports for a captured monitor).
    pub pixels: (u32, u32),
    pub primary: bool,
}

impl Monitor {
    pub fn info(&self) -> MonitorInfo {
        MonitorInfo {
            id: self.id,
            name: self.name.clone(),
            width: self.pixels.0,
            height: self.pixels.1,
            primary: self.primary,
        }
    }
}

/// The bounding box of all monitors.
pub fn desktop_of(monitors: &[Monitor]) -> Option<Rect> {
    let x0 = monitors.iter().map(|m| m.rect.x).min()?;
    let y0 = monitors.iter().map(|m| m.rect.y).min()?;
    let x1 = monitors.iter().map(|m| m.rect.x + m.rect.w as i32).max()?;
    let y1 = monitors.iter().map(|m| m.rect.y + m.rect.h as i32).max()?;
    Some(Rect { x: x0, y: y0, w: (x1 - x0) as u32, h: (y1 - y0) as u32 })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Chosen(u32),
    /// Several monitors could be the shared one: the sharer has to pick.
    Ambiguous(Vec<u32>),
}

/// Which monitor is being shared: the `--monitor` override, the only monitor, the one whose
/// pixel size matches the captured frame, or the one with the same aspect ratio (browsers
/// may scale the capture down).
pub fn pick_monitor(monitors: &[Monitor], frame: (u32, u32), override_id: Option<u32>) -> Choice {
    if let Some(id) = override_id.filter(|id| monitors.iter().any(|m| m.id == *id)) {
        return Choice::Chosen(id);
    }
    if monitors.len() == 1 {
        return Choice::Chosen(monitors[0].id);
    }
    let exact: Vec<u32> = monitors.iter().filter(|m| m.pixels == frame).map(|m| m.id).collect();
    if exact.len() == 1 {
        return Choice::Chosen(exact[0]);
    }
    let aspect = |(w, h): (u32, u32)| if h == 0 { 0.0 } else { w as f64 / h as f64 };
    let target = aspect(frame);
    let similar: Vec<u32> = monitors
        .iter()
        .filter(|m| target > 0.0 && (aspect(m.pixels) - target).abs() / target < 0.01)
        .map(|m| m.id)
        .collect();
    if similar.len() == 1 {
        return Choice::Chosen(similar[0]);
    }
    let candidates = if !exact.is_empty() { exact } else if !similar.is_empty() { similar } else { monitors.iter().map(|m| m.id).collect() };
    Choice::Ambiguous(candidates)
}

/// The desktop pixel under a normalized position on `monitor`.
pub fn to_monitor_px(monitor: &Rect, x: u16, y: u16) -> (i32, i32) {
    let along = |start: i32, len: u32, v: u16| start + ((v as u64 * len.saturating_sub(1) as u64 + 32767) / 65535) as i32;
    (along(monitor.x, monitor.w, x), along(monitor.y, monitor.h, y))
}

/// A desktop pixel as 0..=65535 over `desktop`. Rounds up, so converting back with
/// `floor(v * w / 65536)` lands on exactly this pixel, also with negative desktop origins.
pub fn to_abs16(px: (i32, i32), desktop: &Rect) -> (u16, u16) {
    let axis = |p: i32, start: i32, len: u32| {
        let offset = (p - start).clamp(0, len.saturating_sub(1) as i32) as u64;
        ((offset * 65536 + len as u64 - 1) / len.max(1) as u64).min(65535) as u16
    };
    (axis(px.0, desktop.x, desktop.w), axis(px.1, desktop.y, desktop.h))
}

/// Where a viewer's normalized position goes in desktop-wide absolute units. With one
/// monitor (or none known) the position is used as is.
pub fn map_position(monitors: &[Monitor], chosen: Option<u32>, x: u16, y: u16) -> (u16, u16) {
    let (Some(desktop), Some(monitor)) = (desktop_of(monitors), chosen.and_then(|id| monitors.iter().find(|m| m.id == id))) else {
        return (x, y);
    };
    if monitors.len() == 1 {
        return (x, y);
    }
    to_abs16(to_monitor_px(&monitor.rect, x, y), &desktop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: u32, x: i32, y: i32, w: u32, h: u32) -> Monitor {
        Monitor { id, name: format!("M{id}"), rect: Rect { x, y, w, h }, pixels: (w, h), primary: id == 0 }
    }

    #[test]
    fn test_abs16_lands_on_every_pixel() {
        for desktop in [Rect { x: 0, y: 0, w: 1920, h: 1080 }, Rect { x: -1280, y: -200, w: 4481, h: 1441 }] {
            for p in 0..desktop.w as i32 {
                let (v, _) = to_abs16((desktop.x + p, desktop.y), &desktop);
                let back = (v as u64 * desktop.w as u64 / 65536) as i32;
                assert_eq!(back, p, "pixel {p} of {desktop:?}");
            }
        }
    }

    #[test]
    fn test_edges_of_a_monitor_map_to_its_edges() {
        let rect = Rect { x: 1920, y: 0, w: 2560, h: 1440 };
        assert_eq!(to_monitor_px(&rect, 0, 0), (1920, 0));
        assert_eq!(to_monitor_px(&rect, 65535, 65535), (1920 + 2559, 1439));
        assert_eq!(to_monitor_px(&rect, 32768, 32768), (1920 + 1280, 720));
    }

    #[test]
    fn test_choosing_the_shared_monitor() {
        let laptop = monitor(0, 0, 0, 1920, 1080);
        let wide = monitor(1, 1920, 0, 2560, 1440);
        let twin = monitor(2, 4480, 0, 2560, 1440);
        assert_eq!(pick_monitor(&[laptop.clone()], (1280, 720), None), Choice::Chosen(0), "only one");
        assert_eq!(pick_monitor(&[laptop.clone(), wide.clone()], (2560, 1440), None), Choice::Chosen(1), "exact size");
        assert_eq!(
            pick_monitor(&[monitor(0, 0, 0, 1920, 1200), wide.clone()], (1280, 720), None),
            Choice::Chosen(1),
            "downscaled capture, same aspect"
        );
        assert_eq!(pick_monitor(&[laptop.clone(), wide.clone(), twin.clone()], (2560, 1440), None), Choice::Ambiguous(vec![1, 2]));
        assert_eq!(pick_monitor(&[laptop, wide, twin], (2560, 1440), Some(2)), Choice::Chosen(2), "override");
    }

    #[test]
    fn test_positions_on_the_second_monitor() {
        let monitors = [monitor(0, 0, 0, 1920, 1080), monitor(1, 1920, 0, 1920, 1080)];
        // The left edge of the right monitor is the middle of a 3840-wide desktop.
        let (x, y) = map_position(&monitors, Some(1), 0, 0);
        assert_eq!((x as u64 * 3840 / 65536, y), (1920, 0));
        // One monitor (or nothing known): positions pass through unchanged.
        assert_eq!(map_position(&monitors[..1], Some(0), 123, 456), (123, 456));
        assert_eq!(map_position(&[], None, 7, 8), (7, 8));
    }
}
