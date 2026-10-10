//! The link to `dchat-host`, the companion app that injects remote-control input on this
//! computer. It lives at the app level, so pairing survives a room moving to a new link.
//! Only opened when the user clicks Connect, never on page load. The pairing code and the
//! session token stay in RAM.

use leptos::prelude::{Set, WriteSignal};
use protocol::{
    pair_ack_matches, pair_proof, resume_ack_matches, resume_proof, AgentCaps, AgentOs, AgentToTab, InputEvent,
    MonitorInfo, RefuseReason, TabToAgent, AGENT_PROTOCOL_VERSION,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

const RECONNECT_FIRST_MS: i32 = 1000;
const RECONNECT_MAX_MS: i32 = 10_000;
/// The app keeps a dropped session this long; after that a new code is needed.
const RESUME_WINDOW_MS: f64 = 28_000.0;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum AgentStatus {
    #[default]
    Off,
    Connecting,
    Paired { os: AgentOs, caps: AgentCaps },
    WrongCode,
    Locked { retry_secs: u64 },
    Busy,
    /// Nothing answered on that port (not running, or the browser blocked local access).
    Unreachable,
    VersionMismatch,
    /// Something answered but could not prove it knows the code.
    NotTheApp,
    /// The user stopped remote control on the host computer.
    Stopped,
}

impl AgentStatus {
    pub fn is_paired(&self) -> bool {
        matches!(self, AgentStatus::Paired { .. })
    }

    /// For `data-status` attributes and i18n keys (`agent_status_<key>`).
    pub fn key(&self) -> &'static str {
        match self {
            AgentStatus::Off => "off",
            AgentStatus::Connecting => "connecting",
            AgentStatus::Paired { .. } => "paired",
            AgentStatus::WrongCode => "wrong_code",
            AgentStatus::Locked { .. } => "locked",
            AgentStatus::Busy => "busy",
            AgentStatus::Unreachable => "unreachable",
            AgentStatus::VersionMismatch => "version",
            AgentStatus::NotTheApp => "not_the_app",
            AgentStatus::Stopped => "stopped",
        }
    }
}

/// What the room session needs to hear about.
pub enum AgentEvent {
    /// (Re)paired: resend the screen and current grants.
    Ready,
    /// Gone (closed, stopped or unpaired): control must end.
    Lost,
}

#[derive(Clone, Copy)]
pub struct AgentSignals {
    pub status: WriteSignal<AgentStatus>,
    pub monitors: WriteSignal<Option<(Vec<MonitorInfo>, Option<u32>)>>,
    pub warning: WriteSignal<Option<String>>,
}

pub struct AgentLink {
    port: u16,
    signals: AgentSignals,
    socket: RefCell<Option<WebSocket>>,
    code: RefCell<Option<String>>,
    token: RefCell<Option<String>>,
    /// Our nonce and the app's, for checking the app's proof.
    nonces: RefCell<Option<(String, String)>>,
    os: Cell<AgentOs>,
    caps: RefCell<Option<AgentCaps>>,
    paired: Cell<bool>,
    ever_paired: Cell<bool>,
    lost_at: Cell<f64>,
    user_closed: Cell<bool>,
    backoff: Cell<i32>,
    listener: RefCell<Option<Rc<dyn Fn(AgentEvent)>>>,
}

impl AgentLink {
    pub fn connect(port: u16, code: &str, signals: AgentSignals) -> Rc<Self> {
        let link = Rc::new(Self {
            port,
            signals,
            socket: RefCell::new(None),
            code: RefCell::new(Some(code.to_string())),
            token: RefCell::new(None),
            nonces: RefCell::new(None),
            os: Cell::new(AgentOs::Other),
            caps: RefCell::new(None),
            paired: Cell::new(false),
            ever_paired: Cell::new(false),
            lost_at: Cell::new(0.0),
            user_closed: Cell::new(false),
            backoff: Cell::new(RECONNECT_FIRST_MS),
            listener: RefCell::new(None),
        });
        link.open();
        link
    }

    pub fn is_paired(&self) -> bool {
        self.paired.get()
    }

    /// What the app can do on this computer (once paired).
    pub fn caps(&self) -> Option<AgentCaps> {
        self.caps.borrow().clone().filter(|_| self.paired.get())
    }

    pub fn set_listener(&self, listener: Option<Rc<dyn Fn(AgentEvent)>>) {
        *self.listener.borrow_mut() = listener;
    }

    fn notify(&self, event: AgentEvent) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }

    fn open(self: &Rc<Self>) {
        self.signals.status.set(AgentStatus::Connecting);
        // The literal loopback address: browsers treat it as trustworthy from https pages.
        let Ok(ws) = WebSocket::new(&format!("ws://127.0.0.1:{}/", self.port)) else {
            self.signals.status.set(AgentStatus::Unreachable);
            return;
        };
        let on_message = {
            let link = Rc::downgrade(self);
            Closure::wrap(Box::new(move |ev: MessageEvent| {
                if let (Some(link), Some(text)) = (link.upgrade(), ev.data().as_string()) {
                    if let Ok(message) = serde_json::from_str::<AgentToTab>(&text) {
                        link.on_message(message);
                    }
                }
            }) as Box<dyn FnMut(MessageEvent)>)
        };
        let on_close = {
            let link = Rc::downgrade(self);
            let socket = ws.clone();
            Closure::wrap(Box::new(move |_: CloseEvent| {
                if let Some(link) = link.upgrade() {
                    link.on_close(&socket);
                }
            }) as Box<dyn FnMut(CloseEvent)>)
        };
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_message.forget();
        on_close.forget();
        *self.socket.borrow_mut() = Some(ws);
    }

    fn send_raw(&self, message: &TabToAgent) -> bool {
        let Ok(text) = serde_json::to_string(message) else {
            return false;
        };
        self.socket
            .borrow()
            .as_ref()
            .filter(|ws| ws.ready_state() == WebSocket::OPEN)
            .is_some_and(|ws| ws.send_with_str(&text).is_ok())
    }

    /// Send to the paired app; nothing goes anywhere before the app proved itself.
    pub fn send(&self, message: &TabToAgent) -> bool {
        self.paired.get() && self.send_raw(message)
    }

    pub fn send_input(&self, who: u32, events: Vec<InputEvent>) -> bool {
        !events.is_empty() && self.send(&TabToAgent::Input { who, ev: events })
    }

    fn on_message(self: &Rc<Self>, message: AgentToTab) {
        match message {
            AgentToTab::Challenge { v, nonce, os, .. } => {
                self.os.set(os);
                if v != AGENT_PROTOCOL_VERSION {
                    self.fail(AgentStatus::VersionMismatch);
                    return;
                }
                let client_nonce = uuid::Uuid::new_v4().to_string();
                let token = self.token.borrow().clone();
                let hello = match (token, self.code.borrow().clone()) {
                    (Some(token), _) => TabToAgent::Resume {
                        v,
                        client_nonce: client_nonce.clone(),
                        proof: resume_proof(&token, &nonce, &client_nonce),
                    },
                    (None, Some(code)) => TabToAgent::Hello {
                        v,
                        client_nonce: client_nonce.clone(),
                        proof: pair_proof(&code, &nonce, &client_nonce),
                    },
                    (None, None) => {
                        self.fail(AgentStatus::Off);
                        return;
                    }
                };
                *self.nonces.borrow_mut() = Some((client_nonce, nonce));
                self.send_raw(&hello);
            }
            AgentToTab::Paired { proof, token, caps } => {
                let Some((client_nonce, nonce)) = self.nonces.borrow().clone() else {
                    return;
                };
                let code = self.code.borrow().clone().unwrap_or_default();
                if !pair_ack_matches(&code, &client_nonce, &nonce, &proof) {
                    self.fail(AgentStatus::NotTheApp);
                    return;
                }
                // One-time code: forget it; reconnects use the session token.
                *self.code.borrow_mut() = None;
                *self.token.borrow_mut() = Some(token);
                self.paired_with(caps);
            }
            AgentToTab::Resumed { proof, caps } => {
                let Some((client_nonce, nonce)) = self.nonces.borrow().clone() else {
                    return;
                };
                let token = self.token.borrow().clone().unwrap_or_default();
                if !resume_ack_matches(&token, &client_nonce, &nonce, &proof) {
                    self.fail(AgentStatus::NotTheApp);
                    return;
                }
                self.paired_with(caps);
            }
            AgentToTab::Refused { reason } => {
                let status = match reason {
                    RefuseReason::BadCode => AgentStatus::WrongCode,
                    RefuseReason::Locked { retry_secs } => AgentStatus::Locked { retry_secs },
                    RefuseReason::Busy => AgentStatus::Busy,
                    RefuseReason::Version { .. } => AgentStatus::VersionMismatch,
                    RefuseReason::BadToken | RefuseReason::Timeout => AgentStatus::Off,
                };
                self.fail(status);
            }
            AgentToTab::Monitors { list, chosen } => self.signals.monitors.set(Some((list, chosen))),
            AgentToTab::Warning { code } => self.signals.warning.set(Some(code)),
            AgentToTab::Stopped { .. } => self.fail(AgentStatus::Stopped),
        }
    }

    fn paired_with(&self, caps: AgentCaps) {
        *self.caps.borrow_mut() = Some(caps.clone());
        self.paired.set(true);
        self.ever_paired.set(true);
        self.backoff.set(RECONNECT_FIRST_MS);
        self.signals.status.set(AgentStatus::Paired { os: self.os.get(), caps });
        self.notify(AgentEvent::Ready);
    }

    /// End for good (no reconnect): wrong code, stopped on the host, impostor...
    fn fail(&self, status: AgentStatus) {
        self.user_closed.set(true);
        *self.token.borrow_mut() = None;
        if let Some(ws) = self.socket.borrow_mut().take() {
            let _ = ws.close();
        }
        let was_paired = self.paired.replace(false);
        self.signals.status.set(status);
        if was_paired {
            self.notify(AgentEvent::Lost);
        }
    }

    fn on_close(self: &Rc<Self>, socket: &WebSocket) {
        let current = self.socket.borrow().as_ref().is_some_and(|ws| ws == socket);
        if !current {
            return;
        }
        self.socket.borrow_mut().take();
        let was_paired = self.paired.replace(false);
        if was_paired {
            self.lost_at.set(js_sys::Date::now());
            self.notify(AgentEvent::Lost);
        }
        if self.user_closed.get() {
            return;
        }
        let can_resume =
            self.token.borrow().is_some() && js_sys::Date::now() - self.lost_at.get() < RESUME_WINDOW_MS;
        if !self.ever_paired.get() {
            self.signals.status.set(AgentStatus::Unreachable);
            return;
        }
        if !can_resume {
            *self.token.borrow_mut() = None;
            self.signals.status.set(AgentStatus::Off);
            return;
        }
        self.signals.status.set(AgentStatus::Connecting);
        let delay = self.backoff.get();
        self.backoff.set((delay * 2).min(RECONNECT_MAX_MS));
        let link = Rc::downgrade(self);
        leptos::prelude::set_timeout(
            move || {
                if let Some(link) = link.upgrade() {
                    if !link.user_closed.get() {
                        link.open();
                    }
                }
            },
            std::time::Duration::from_millis(delay as u64),
        );
    }

    /// The user disconnects: tell the app the session is over.
    pub fn disconnect(&self) {
        if self.paired.get() {
            self.send_raw(&TabToAgent::Bye);
        }
        self.fail(AgentStatus::Off);
    }
}
