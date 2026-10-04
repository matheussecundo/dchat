//! Monitor layout from X11 RandR (also available through XWayland).

use crate::geometry::{Monitor, Rect};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;

pub fn monitors() -> Option<Vec<Monitor>> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let reply = conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
    let monitors = reply
        .monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
                .unwrap_or_else(|| format!("monitor {i}"));
            Monitor {
                id: i as u32,
                name,
                rect: Rect { x: m.x as i32, y: m.y as i32, w: m.width as u32, h: m.height as u32 },
                pixels: (m.width as u32, m.height as u32),
                primary: m.primary,
            }
        })
        .collect::<Vec<_>>();
    (!monitors.is_empty()).then_some(monitors)
}
