//! Capturing this viewer's mouse and keyboard over a shared screen's tile while they control
//! it. Listeners are added by hand (not through Leptos) so they can be non-passive and run
//! in the capture phase: wheel scrolling and keys must go to the controlled computer, not
//! to this page. Dropping the capture removes everything and releases all held input.

use crate::layout::{contain_rect, picture_fraction};
use protocol::{is_release_chord, normalize_position, DomCode, InputEvent, MouseButton, WheelAccumulator};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    window, AddEventListenerOptions, Event, EventTarget, HtmlElement, HtmlVideoElement, KeyboardEvent, MouseEvent,
    PointerEvent, WheelEvent,
};

pub type InputSink = Rc<dyn Fn(Vec<InputEvent>)>;

/// Pointer moves are sent at most this often (coalesced).
const MOVE_FLUSH_MS: i32 = 16;
/// Tells the host we are still here, so it can release input if we vanish.
const HEARTBEAT_MS: i32 = 500;

struct Listener {
    target: EventTarget,
    kind: &'static str,
    capture: bool,
    callback: Closure<dyn FnMut(Event)>,
}

pub struct InputCapture {
    listeners: Vec<Listener>,
    intervals: Vec<(i32, Closure<dyn FnMut()>)>,
    sink: InputSink,
}

impl InputCapture {
    /// Start forwarding input over `surface` (the overlay above `video`). `on_release` is
    /// called (asynchronously) when the viewer stops: the release shortcut, leaving the tab.
    pub fn engage(
        surface: &HtmlElement,
        video: &HtmlVideoElement,
        sink: InputSink,
        on_release: Rc<dyn Fn()>,
    ) -> Result<Self, JsValue> {
        let win = window().ok_or("no window")?;
        let doc = win.document().ok_or("no document")?;
        let mut capture = Self { listeners: Vec::new(), intervals: Vec::new(), sink: sink.clone() };

        let position: Rc<dyn Fn(&MouseEvent) -> (u16, u16)> = {
            let (surface, video) = (surface.clone(), video.clone());
            Rc::new(move |ev: &MouseEvent| {
                let rect = surface.get_bounding_client_rect();
                let picture = contain_rect(rect.width(), rect.height(), video.video_width() as f64, video.video_height() as f64);
                let (fx, fy) = picture_fraction(ev.client_x() as f64 - rect.left(), ev.client_y() as f64 - rect.top(), picture);
                normalize_position(fx, fy)
            })
        };
        let pending_move: Rc<Cell<Option<(u16, u16)>>> = Rc::new(Cell::new(None));
        let release = {
            let on_release = on_release.clone();
            // Never tear the capture down from inside one of its own listeners.
            Rc::new(move || {
                let on_release = on_release.clone();
                wasm_bindgen_futures::spawn_local(async move { on_release() });
            })
        };

        let target: EventTarget = surface.clone().into();
        {
            let (position, pending) = (position.clone(), pending_move.clone());
            capture.listen(&target, "pointermove", false, move |ev| {
                if let Some(ev) = ev.dyn_ref::<MouseEvent>() {
                    pending.set(Some(position(ev)));
                }
            })?;
        }
        for (kind, down) in [("pointerdown", true), ("pointerup", false)] {
            let (position, pending, sink, surface) = (position.clone(), pending_move.clone(), sink.clone(), surface.clone());
            capture.listen(&target, kind, false, move |ev| {
                ev.prevent_default();
                let Some(mouse) = ev.dyn_ref::<MouseEvent>() else {
                    return;
                };
                if let (true, Some(pointer)) = (down, ev.dyn_ref::<PointerEvent>()) {
                    let _ = surface.set_pointer_capture(pointer.pointer_id());
                }
                let Some(button) = MouseButton::from_dom(mouse.button()) else {
                    return;
                };
                pending.set(None);
                sink(vec![InputEvent::Button { button, down, at: Some(position(mouse)) }]);
            })?;
        }
        {
            let (position, sink) = (position.clone(), sink.clone());
            let wheel = Rc::new(RefCell::new(WheelAccumulator::default()));
            capture.listen(&target, "wheel", false, move |ev| {
                ev.prevent_default();
                let Some(wheel_ev) = ev.dyn_ref::<WheelEvent>() else {
                    return;
                };
                let mut acc = wheel.borrow_mut();
                acc.push(wheel_ev.delta_x(), wheel_ev.delta_y(), wheel_ev.delta_mode());
                if let Some((dx, dy)) = acc.take() {
                    sink(vec![InputEvent::Wheel { dx, dy, at: Some(position(wheel_ev)) }]);
                }
            })?;
        }
        for kind in ["contextmenu", "auxclick"] {
            capture.listen(&target, kind, false, |ev| ev.prevent_default())?;
        }
        // The tile's own double-click toggles fullscreen; while controlling it belongs to the host.
        capture.listen(&target, "dblclick", false, |ev| {
            ev.prevent_default();
            ev.stop_propagation();
        })?;

        let win_target: EventTarget = win.clone().into();
        for (kind, down) in [("keydown", true), ("keyup", false)] {
            let (sink, release) = (sink.clone(), release.clone());
            capture.listen(&win_target, kind, true, move |ev| {
                let Some(key) = ev.dyn_ref::<KeyboardEvent>() else {
                    return;
                };
                // Keys go to the controlled computer, not to dchat's own shortcuts.
                ev.prevent_default();
                ev.stop_propagation();
                if down && is_release_chord(key.ctrl_key(), key.alt_key(), key.shift_key(), &key.code()) {
                    release();
                    return;
                }
                if let Some(code) = DomCode::from_code(&key.code()) {
                    sink(vec![InputEvent::Key { code, down, repeat: down && key.repeat() }]);
                }
            })?;
        }
        {
            let release = release.clone();
            capture.listen(&win_target, "blur", false, move |_| release())?;
        }
        {
            let release = release.clone();
            let doc_c = doc.clone();
            capture.listen(&doc.clone().into(), "visibilitychange", false, move |_| {
                if doc_c.hidden() {
                    release();
                }
            })?;
        }
        // Closing the tab mid-control would leave the host's keys to the watchdog: ask first.
        capture.listen(&win_target, "beforeunload", false, |ev| ev.prevent_default())?;

        {
            let (pending, sink) = (pending_move.clone(), sink.clone());
            capture.every(MOVE_FLUSH_MS, move || {
                if let Some((x, y)) = pending.take() {
                    sink(vec![InputEvent::PointerAbs { x, y }]);
                }
            })?;
        }
        {
            let sink = sink.clone();
            capture.every(HEARTBEAT_MS, move || sink(vec![InputEvent::Alive]))?;
        }
        let _ = surface.focus();
        Ok(capture)
    }

    fn listen(
        &mut self,
        target: &EventTarget,
        kind: &'static str,
        capture: bool,
        handler: impl FnMut(Event) + 'static,
    ) -> Result<(), JsValue> {
        let callback = Closure::wrap(Box::new(handler) as Box<dyn FnMut(Event)>);
        let options = AddEventListenerOptions::new();
        options.set_passive(false);
        options.set_capture(capture);
        target.add_event_listener_with_callback_and_add_event_listener_options(kind, callback.as_ref().unchecked_ref(), &options)?;
        self.listeners.push(Listener { target: target.clone(), kind, capture, callback });
        Ok(())
    }

    fn every(&mut self, ms: i32, mut tick: impl FnMut() + 'static) -> Result<(), JsValue> {
        let callback = Closure::wrap(Box::new(move || tick()) as Box<dyn FnMut()>);
        let id = window()
            .ok_or("no window")?
            .set_interval_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), ms)?;
        self.intervals.push((id, callback));
        Ok(())
    }
}

impl Drop for InputCapture {
    fn drop(&mut self) {
        for l in self.listeners.drain(..) {
            let _ = l.target.remove_event_listener_with_callback_and_bool(l.kind, l.callback.as_ref().unchecked_ref(), l.capture);
        }
        if let Some(win) = window() {
            for (id, _callback) in self.intervals.drain(..) {
                win.clear_interval_with_handle(id);
            }
        }
        (self.sink)(vec![InputEvent::ReleaseAll]);
    }
}
