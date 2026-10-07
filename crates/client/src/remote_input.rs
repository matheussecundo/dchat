//! Capturing this viewer's mouse and keyboard over a shared screen's tile while they control
//! it. Listeners are added by hand (not through Leptos) so they can be non-passive and run
//! in the capture phase: wheel scrolling and keys must go to the controlled computer, not
//! to this page. Dropping the capture removes everything and releases all held input.

use crate::layout::{contain_rect, picture_fraction};
use protocol::pacing::MoveSpacer;
use protocol::{
    is_release_chord, normalize_position, pad_state_from, DomCode, InputEvent, MouseButton, PadSampler, PadState,
    PointerMode, RelAccumulator, WheelAccumulator,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    window, AddEventListenerOptions, Event, EventTarget, HtmlElement, HtmlVideoElement, KeyboardEvent, MouseEvent,
    PointerEvent, WheelEvent,
};

pub type InputSink = Rc<dyn Fn(Vec<InputEvent>)>;

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
    mode: PointerMode,
    moves: Rc<Moves>,
}

/// Pointer moves, sent as they happen but at most one every `MOVE_MIN_GAP_MS`
/// (`MoveSpacer`); a move inside the gap waits for one trailing flush. Desktop mode keeps
/// the newest absolute position, game mode sums relative movement.
struct Moves {
    game: bool,
    sink: InputSink,
    /// Cleared when the capture ends: nothing is sent after that, above all no move after
    /// `ReleaseAll`.
    alive: Cell<bool>,
    spacer: RefCell<MoveSpacer>,
    abs: Cell<Option<(u16, u16)>>,
    rel: RefCell<RelAccumulator>,
    /// A move arrived since the last send.
    dirty: Cell<bool>,
    /// The trailing flush: its timeout and its callback (kept here so it outlives the timer).
    trailing: Cell<Option<i32>>,
    trailing_cb: RefCell<Option<Closure<dyn FnMut()>>>,
}

impl Moves {
    fn new(game: bool, sink: InputSink) -> Rc<Self> {
        let moves = Rc::new(Self {
            game,
            sink,
            alive: Cell::new(true),
            spacer: RefCell::new(MoveSpacer::default()),
            abs: Cell::new(None),
            rel: RefCell::new(RelAccumulator::default()),
            dirty: Cell::new(false),
            trailing: Cell::new(None),
            trailing_cb: RefCell::new(None),
        });
        let weak = Rc::downgrade(&moves);
        let flush = Closure::wrap(Box::new(move || {
            if let Some(moves) = weak.upgrade() {
                moves.trailing.set(None);
                moves.try_send();
            }
        }) as Box<dyn FnMut()>);
        *moves.trailing_cb.borrow_mut() = Some(flush);
        moves
    }

    /// The viewer moved: send now if the gap allows, otherwise once it ends.
    fn moved(&self) {
        self.dirty.set(true);
        self.try_send();
    }

    fn try_send(&self) {
        if !self.alive.get() || !self.dirty.get() {
            return;
        }
        match self.spacer.borrow_mut().try_send(js_sys::Date::now()) {
            Ok(()) => {
                let events = self.take();
                if !events.is_empty() {
                    (self.sink)(events);
                }
            }
            Err(wait_ms) => self.schedule(wait_ms),
        }
    }

    /// What is waiting, as events (whole pixels only in game mode; fractions stay).
    fn take(&self) -> Vec<InputEvent> {
        self.dirty.set(false);
        if self.game {
            let mut rel = self.rel.borrow_mut();
            std::iter::from_fn(|| rel.take()).map(|(dx, dy)| InputEvent::PointerRel { dx, dy }).collect()
        } else {
            self.abs.take().map(|(x, y)| InputEvent::PointerAbs { x, y }).into_iter().collect()
        }
    }

    fn schedule(&self, wait_ms: f64) {
        if self.trailing.get().is_some() {
            return;
        }
        let (Some(win), Some(cb)) = (window(), self.trailing_cb.borrow().as_ref().map(|cb| cb.as_ref().clone())) else {
            return;
        };
        let ms = wait_ms.ceil().clamp(1.0, 1000.0) as i32;
        if let Ok(handle) = win.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms) {
            self.trailing.set(Some(handle));
        }
    }

    /// Stop for good: cancel the trailing flush and forget what waits.
    fn shut(&self) {
        self.alive.set(false);
        if let (Some(handle), Some(win)) = (self.trailing.take(), window()) {
            win.clear_timeout_with_handle(handle);
        }
        self.dirty.set(false);
        self.abs.set(None);
        *self.rel.borrow_mut() = RelAccumulator::default();
    }
}

impl InputCapture {
    /// Start forwarding input over `surface` (the overlay above `video`). `on_release` is
    /// called (asynchronously) when the viewer stops: the release shortcut, leaving the tab,
    /// losing pointer lock. Desktop mode sends absolute positions over the picture; game
    /// mode locks the pointer and sends relative movement. Call from a click (pointer lock
    /// needs a user gesture).
    pub fn engage(
        surface: &HtmlElement,
        video: &HtmlVideoElement,
        mode: PointerMode,
        sink: InputSink,
        on_release: Rc<dyn Fn()>,
    ) -> Result<Self, JsValue> {
        let win = window().ok_or("no window")?;
        let doc = win.document().ok_or("no document")?;
        let game = mode == PointerMode::Game;
        let moves = Moves::new(game, sink.clone());
        let mut capture = Self { listeners: Vec::new(), intervals: Vec::new(), sink: sink.clone(), mode, moves: moves.clone() };

        let position: Rc<dyn Fn(&MouseEvent) -> (u16, u16)> = {
            let (surface, video) = (surface.clone(), video.clone());
            Rc::new(move |ev: &MouseEvent| {
                let rect = surface.get_bounding_client_rect();
                let picture = contain_rect(rect.width(), rect.height(), video.video_width() as f64, video.video_height() as f64);
                let (fx, fy) = picture_fraction(ev.client_x() as f64 - rect.left(), ev.client_y() as f64 - rect.top(), picture);
                normalize_position(fx, fy)
            })
        };
        let release = {
            let on_release = on_release.clone();
            // Never tear the capture down from inside one of its own listeners.
            Rc::new(move || {
                let on_release = on_release.clone();
                wasm_bindgen_futures::spawn_local(async move { on_release() });
            })
        };

        // Desktop mode: the newest position over the picture. Game mode: movement while the
        // pointer is locked, summed between sends.
        let target: EventTarget = surface.clone().into();
        {
            let (position, moves) = (position.clone(), moves.clone());
            capture.listen(&target, "pointermove", false, move |ev| {
                if let Some(ev) = ev.dyn_ref::<MouseEvent>() {
                    if game {
                        moves.rel.borrow_mut().add(ev.movement_x() as f64, ev.movement_y() as f64);
                    } else {
                        moves.abs.set(Some(position(ev)));
                    }
                    moves.moved();
                }
            })?;
        }
        for (kind, down) in [("pointerdown", true), ("pointerup", false)] {
            let (position, moves, sink, surface) = (position.clone(), moves.clone(), sink.clone(), surface.clone());
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
                // Desktop: the click carries its own position, so a waiting move is moot.
                // Game: movement so far goes first, so the click lands where the viewer aimed.
                let mut events = moves.take();
                if !game {
                    events.clear();
                }
                let at = (!game).then(|| position(mouse));
                events.push(InputEvent::Button { button, down, at });
                sink(events);
            })?;
        }
        {
            let (position, sink, moves) = (position.clone(), sink.clone(), moves.clone());
            let wheel = Rc::new(RefCell::new(WheelAccumulator::default()));
            capture.listen(&target, "wheel", false, move |ev| {
                ev.prevent_default();
                let Some(wheel_ev) = ev.dyn_ref::<WheelEvent>() else {
                    return;
                };
                let mut acc = wheel.borrow_mut();
                acc.push(wheel_ev.delta_x(), wheel_ev.delta_y(), wheel_ev.delta_mode());
                if let Some((dx, dy)) = acc.take() {
                    let at = (!game).then(|| position(wheel_ev));
                    // Game: movement so far goes first, as for clicks.
                    let mut events = if game { moves.take() } else { Vec::new() };
                    events.push(InputEvent::Wheel { dx, dy, at });
                    sink(events);
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

        if game {
            // Losing the lock (Esc, switching windows) ends control, like in a game.
            let surface_el: web_sys::Element = surface.clone().into();
            let (on_lost, doc_c) = (release.clone(), doc.clone());
            capture.listen(&doc.clone().into(), "pointerlockchange", false, move |_| {
                let locked = doc_c.pointer_lock_element().is_some_and(|el| el == surface_el);
                if !locked {
                    on_lost();
                }
            })?;
            let on_error = release.clone();
            capture.listen(&doc.clone().into(), "pointerlockerror", false, move |_| on_error())?;
            request_pointer_lock(surface);
            // In fullscreen, Chromium can also hand us Esc, Alt+Tab and the system keys.
            if doc.fullscreen_element().is_some() {
                keyboard_lock(true);
            }
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
        // First: no move may follow the `ReleaseAll` below.
        self.moves.shut();
        for l in self.listeners.drain(..) {
            let _ = l.target.remove_event_listener_with_callback_and_bool(l.kind, l.callback.as_ref().unchecked_ref(), l.capture);
        }
        if let Some(win) = window() {
            for (id, _callback) in self.intervals.drain(..) {
                win.clear_interval_with_handle(id);
            }
        }
        if self.mode == PointerMode::Game {
            if let Some(doc) = window().and_then(|w| w.document()) {
                if doc.pointer_lock_element().is_some() {
                    doc.exit_pointer_lock();
                }
            }
            keyboard_lock(false);
        }
        (self.sink)(vec![InputEvent::ReleaseAll]);
    }
}

/// Raw mouse movement where supported (`unadjustedMovement`), plain pointer lock otherwise.
fn request_pointer_lock(surface: &HtmlElement) {
    let Ok(request) = js_sys::Reflect::get(surface, &"requestPointerLock".into()).and_then(|f| f.dyn_into::<js_sys::Function>()) else {
        return;
    };
    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&options, &"unadjustedMovement".into(), &JsValue::TRUE);
    let result = request.call1(surface, &options);
    if let Ok(promise) = result.and_then(|r| r.dyn_into::<js_sys::Promise>()) {
        let (surface, request) = (surface.clone(), request.clone());
        let retry = Closure::once(move |_: JsValue| {
            let _ = request.call0(&surface);
        });
        let _ = promise.catch(&retry);
        retry.forget();
    }
}

/// `navigator.keyboard.lock()` / `unlock()` (Chromium only; ignored elsewhere).
fn keyboard_lock(on: bool) {
    let Some(win) = window() else {
        return;
    };
    let Ok(keyboard) = js_sys::Reflect::get(&win.navigator(), &"keyboard".into()) else {
        return;
    };
    let method = if on { "lock" } else { "unlock" };
    if let Ok(f) = js_sys::Reflect::get(&keyboard, &method.into()).and_then(|f| f.dyn_into::<js_sys::Function>()) {
        if let Ok(result) = f.call0(&keyboard) {
            if let Ok(promise) = result.dyn_into::<js_sys::Promise>() {
                let ignore = Closure::once(|_: JsValue| {});
                let _ = promise.catch(&ignore);
                ignore.forget();
            }
        }
    }
}

/// How often the controller is read (browsers have no gamepad events for buttons or sticks).
const PAD_POLL_MS: i32 = 8;

/// Sends this viewer's game controller while they hold a controller slot. Uses the first
/// controller in the browser's "standard" layout; nothing is sent while the tab is hidden
/// (the host's pad goes back to neutral). Browsers only show a controller after one of its
/// buttons is pressed on this page.
pub struct PadPoller {
    interval: Option<(i32, Closure<dyn FnMut()>)>,
    sink: InputSink,
}

impl PadPoller {
    pub fn start(sink: InputSink) -> Result<Self, JsValue> {
        let win = window().ok_or("no window")?;
        let sampler = RefCell::new(PadSampler::default());
        let connected = Cell::new(false);
        let tick_sink = sink.clone();
        let callback = Closure::wrap(Box::new(move || {
            let visible = window().and_then(|w| w.document()).is_some_and(|d| !d.hidden());
            match read_standard_pad().filter(|_| visible) {
                Some(state) => {
                    connected.set(true);
                    if let Some(state) = sampler.borrow_mut().sample(state, js_sys::Date::now()) {
                        tick_sink(vec![InputEvent::Pad { index: 0, state }]);
                    }
                }
                None => {
                    if connected.replace(false) {
                        *sampler.borrow_mut() = PadSampler::default();
                        tick_sink(vec![InputEvent::PadGone { index: 0 }]);
                    }
                }
            }
        }) as Box<dyn FnMut()>);
        let id = win.set_interval_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), PAD_POLL_MS)?;
        Ok(Self { interval: Some((id, callback)), sink })
    }
}

impl Drop for PadPoller {
    fn drop(&mut self) {
        if let (Some((id, _callback)), Some(win)) = (self.interval.take(), window()) {
            win.clear_interval_with_handle(id);
        }
        (self.sink)(vec![InputEvent::PadGone { index: 0 }]);
    }
}

/// The first connected "standard" controller, read through plain properties.
fn read_standard_pad() -> Option<PadState> {
    let navigator = window()?.navigator();
    let get = js_sys::Reflect::get(&navigator, &"getGamepads".into()).ok()?.dyn_into::<js_sys::Function>().ok()?;
    let pads = get.call0(&navigator).ok()?.dyn_into::<js_sys::Array>().ok()?;
    let field = |obj: &JsValue, name: &str| js_sys::Reflect::get(obj, &name.into()).unwrap_or(JsValue::UNDEFINED);
    pads.iter().filter(|p| !p.is_null() && !p.is_undefined()).find_map(|pad| {
        let standard = field(&pad, "mapping").as_string().as_deref() == Some("standard");
        if !standard || field(&pad, "connected").as_bool() != Some(true) {
            return None;
        }
        let buttons: Vec<(bool, f64)> = field(&pad, "buttons")
            .dyn_into::<js_sys::Array>()
            .ok()?
            .iter()
            .map(|b| (field(&b, "pressed").as_bool().unwrap_or(false), field(&b, "value").as_f64().unwrap_or(0.0)))
            .collect();
        let axes: Vec<f64> = field(&pad, "axes")
            .dyn_into::<js_sys::Array>()
            .ok()?
            .iter()
            .map(|a| a.as_f64().unwrap_or(0.0))
            .collect();
        Some(pad_state_from(&buttons, &axes))
    })
}
