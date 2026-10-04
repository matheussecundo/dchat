//! The localhost WebSocket the sharer's dchat tab connects to.
//!
//! Before anything else: the Host header must be this loopback port (no DNS rebinding), the
//! Origin must be an allowed dchat site, and the tab must prove it knows the one-time code
//! (and check that we know it too). One paired tab at a time.

use crate::config::{AgentConfig, TokenBucket};
use crate::engine::{EngineCmd, EngineEvent, EngineHandle};
use crate::inject::mock::Recording;
use crate::pairing::{random_token, PairOutcome, Pairing, RESUME_GRACE};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use protocol::{AgentCaps, AgentOs, AgentToTab, RefuseReason, TabToAgent, AGENT_PROTOCOL_VERSION};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// WebSocket close codes the tab understands.
pub mod close {
    pub const VERSION: u16 = 4000;
    pub const STOPPED: u16 = 4001;
    pub const BAD_CODE: u16 = 4002;
    pub const LOCKED: u16 = 4003;
    pub const BUSY: u16 = 4004;
    pub const REPLACED: u16 = 4005;
    pub const BAD_TOKEN: u16 = 4006;
    pub const TIMEOUT: u16 = 4008;
    pub const LIMITS: u16 = 4009;
}

enum Outgoing {
    Text(String),
    Close(u16, &'static str),
}

pub struct AgentState {
    pub cfg: AgentConfig,
    pub caps: AgentCaps,
    pub os: AgentOs,
    pairing: Mutex<Pairing>,
    engine: EngineHandle,
    unauthenticated: AtomicUsize,
    generation: AtomicU64,
    current: Mutex<Option<(u64, mpsc::UnboundedSender<Outgoing>)>>,
    status: mpsc::UnboundedSender<String>,
    recording: Option<Recording>,
}

impl AgentState {
    pub fn new(
        cfg: AgentConfig,
        caps: AgentCaps,
        engine: EngineHandle,
        fixed_code: Option<String>,
        recording: Option<Recording>,
        status: mpsc::UnboundedSender<String>,
    ) -> Arc<Self> {
        let os = if cfg!(windows) {
            AgentOs::Windows
        } else if cfg!(target_os = "linux") {
            AgentOs::Linux
        } else {
            AgentOs::Other
        };
        Arc::new(Self {
            cfg,
            caps,
            os,
            pairing: Mutex::new(Pairing::new(fixed_code)),
            engine,
            unauthenticated: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            current: Mutex::new(None),
            status,
            recording,
        })
    }

    fn print(&self, line: impl Into<String>) {
        let _ = self.status.send(line.into());
    }

    pub fn pairing_code(&self) -> Option<String> {
        self.pairing.lock().expect("pairing lock").display_code()
    }

    fn announce_code(&self) {
        if let Some(code) = self.pairing_code() {
            self.print(format!("Pairing code: {code}"));
        }
    }

    fn send_current(&self, message: &AgentToTab) {
        if let Some((_, tx)) = self.current.lock().expect("session lock").as_ref() {
            if let Ok(text) = serde_json::to_string(message) {
                let _ = tx.send(Outgoing::Text(text));
            }
        }
    }

    /// The user stopped remote control here (Enter, hotkey, Ctrl+C): release everything,
    /// tell the tab, end the session and show a new code.
    pub fn stop(&self, reason: &str) {
        self.engine.send(EngineCmd::EndSession);
        let current = self.current.lock().expect("session lock").take();
        if let Some((_, tx)) = current {
            if let Ok(text) = serde_json::to_string(&AgentToTab::Stopped { reason: reason.to_string() }) {
                let _ = tx.send(Outgoing::Text(text));
            }
            let _ = tx.send(Outgoing::Close(close::STOPPED, "stopped on the host"));
        }
        let had_session = {
            let mut pairing = self.pairing.lock().expect("pairing lock");
            let had = pairing.has_session();
            pairing.end_session();
            had
        };
        if had_session {
            self.print(format!("Remote control stopped ({reason})."));
            self.announce_code();
        }
    }

    /// Engine events: status lines for the terminal, warnings and monitors for the tab.
    pub fn engine_event(&self, event: EngineEvent) {
        match event {
            EngineEvent::Status(line) => self.print(line),
            EngineEvent::Warning(line) => {
                self.print(format!("Warning: {line}"));
                self.send_current(&AgentToTab::Warning { code: line });
            }
            EngineEvent::Monitors { list, chosen } => self.send_current(&AgentToTab::Monitors { list, chosen }),
        }
    }
}

pub fn router(state: Arc<AgentState>) -> Router {
    let mut router = Router::new().route("/", get(entry)).route("/health", get(|| async { "ok" }));
    if state.cfg.test_api && state.recording.is_some() {
        router = router.route("/__test/events", get(test_events)).route("/__test/reset", post(test_reset));
    }
    router.with_state(state)
}

async fn test_events(State(state): State<Arc<AgentState>>) -> Response {
    let events = state.recording.as_ref().map(|r| r.lock().expect("recording lock").clone()).unwrap_or_default();
    Json(events).into_response()
}

/// End any session and forget what was recorded, so each test starts unpaired.
async fn test_reset(State(state): State<Arc<AgentState>>) -> &'static str {
    state.stop("test reset");
    // Let the engine finish releasing before the recording is cleared.
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Some(recording) = &state.recording {
        recording.lock().expect("recording lock").clear();
    }
    "ok"
}

/// Counts a socket that has not paired yet; released when it pairs or goes away.
struct UnauthSlot(Arc<AgentState>);

impl Drop for UnauthSlot {
    fn drop(&mut self) {
        self.0.unauthenticated.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn entry(State(state): State<Arc<AgentState>>, headers: HeaderMap, ws: Option<WebSocketUpgrade>) -> Response {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !state.cfg.host_allowed(host) {
        return (StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    let Some(ws) = ws else {
        return "dchat-host: connect from dchat's \"Remote control\" dialog\n".into_response();
    };
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !state.cfg.origin_allowed(origin) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    if state.unauthenticated.fetch_add(1, Ordering::SeqCst) >= state.cfg.max_unauthenticated {
        state.unauthenticated.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, "too many connections").into_response();
    }
    let slot = UnauthSlot(state.clone());
    ws.max_message_size(state.cfg.max_frame_bytes).on_upgrade(move |socket| connection(socket, state, slot))
}

fn text(message: &AgentToTab) -> Outgoing {
    Outgoing::Text(serde_json::to_string(message).unwrap_or_default())
}

async fn connection(socket: WebSocket, state: Arc<AgentState>, slot: UnauthSlot) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();
    let ping_every = state.cfg.ping_interval;
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(ping_every);
        ping.tick().await;
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Outgoing::Text(t)) => {
                        if sink.send(Message::Text(t)).await.is_err() {
                            break;
                        }
                    }
                    Some(Outgoing::Close(code, reason)) => {
                        let frame = CloseFrame { code, reason: Cow::Borrowed(reason) };
                        let _ = sink.send(Message::Close(Some(frame))).await;
                        break;
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    });

    let agent_nonce = random_token(16);
    let _ = tx.send(text(&AgentToTab::Challenge {
        v: AGENT_PROTOCOL_VERSION,
        nonce: agent_nonce.clone(),
        os: state.os,
        agent: format!("dchat-host {}", env!("CARGO_PKG_VERSION")),
    }));

    let generation = match authenticate(&mut stream, &state, &tx, &agent_nonce).await {
        Some(generation) => generation,
        None => {
            drop(tx);
            let _ = writer.await;
            return;
        }
    };
    drop(slot);
    serve(&mut stream, &state, &tx).await;

    // The socket is gone. If it was still the current session, release everything and wait
    // a little for the tab to resume before ending the session.
    let was_current = {
        let mut current = state.current.lock().expect("session lock");
        let mine = current.as_ref().is_some_and(|(g, _)| *g == generation);
        if mine {
            *current = None;
        }
        mine
    };
    if was_current {
        state.engine.send(EngineCmd::ReleaseAll);
        state.pairing.lock().expect("pairing lock").disconnected(Instant::now());
        state.print("dchat disconnected: released all input. It can reconnect for a moment.");
        if let Some(code) = state.pairing_code() {
            state.print(format!("To pair a new tab instead, use code: {code}"));
        }
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESUME_GRACE + Duration::from_millis(100)).await;
            let expired = state.pairing.lock().expect("pairing lock").expire(Instant::now());
            if expired {
                state.engine.send(EngineCmd::EndSession);
                state.print("dchat disconnected: remote control ended.");
                state.announce_code();
            }
        });
    }
    drop(tx);
    let _ = writer.await;
}

async fn next_text(stream: &mut futures_util::stream::SplitStream<WebSocket>) -> Option<String> {
    loop {
        match stream.next().await? {
            Ok(Message::Text(t)) => return Some(t),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            _ => return None,
        }
    }
}

/// The first frame must pair or resume. Returns the session generation on success.
async fn authenticate(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    state: &Arc<AgentState>,
    tx: &mpsc::UnboundedSender<Outgoing>,
    agent_nonce: &str,
) -> Option<u64> {
    let refuse = |reason: RefuseReason, code: u16| {
        let _ = tx.send(text(&AgentToTab::Refused { reason }));
        let _ = tx.send(Outgoing::Close(code, "refused"));
    };
    let first = match tokio::time::timeout(state.cfg.pre_auth_timeout, next_text(stream)).await {
        Ok(Some(text)) if text.len() <= 1024 => text,
        Ok(_) => {
            let _ = tx.send(Outgoing::Close(close::TIMEOUT, "expected pairing"));
            return None;
        }
        Err(_) => {
            refuse(RefuseReason::Timeout, close::TIMEOUT);
            return None;
        }
    };
    let now = Instant::now();
    match serde_json::from_str::<TabToAgent>(&first) {
        Ok(TabToAgent::Hello { v, client_nonce, proof }) | Ok(TabToAgent::Resume { v, client_nonce, proof })
            if v != AGENT_PROTOCOL_VERSION =>
        {
            let _ = (client_nonce, proof);
            refuse(RefuseReason::Version { agent_v: AGENT_PROTOCOL_VERSION }, close::VERSION);
            None
        }
        Ok(TabToAgent::Hello { client_nonce, proof, .. }) => {
            let outcome = state.pairing.lock().expect("pairing lock").try_pair(agent_nonce, &client_nonce, &proof, now);
            match outcome {
                PairOutcome::Paired { token, ack } => {
                    // A fresh session: nothing from an earlier tab carries over.
                    state.engine.send(EngineCmd::EndSession);
                    let generation = take_over(state, tx);
                    let _ = tx.send(text(&AgentToTab::Paired { proof: ack, token, caps: state.caps.clone() }));
                    state.print("Paired with dchat. Waiting for you to allow someone to control this computer.");
                    Some(generation)
                }
                PairOutcome::Refused(reason) => {
                    let code = match reason {
                        RefuseReason::Busy => close::BUSY,
                        RefuseReason::Locked { .. } => close::LOCKED,
                        _ => close::BAD_CODE,
                    };
                    if reason == RefuseReason::BadCode {
                        tokio::time::sleep(state.cfg.bad_code_delay).await;
                    }
                    if let RefuseReason::Locked { retry_secs } = reason {
                        state.print(format!("Too many wrong codes: pairing paused for {retry_secs} s."));
                        state.announce_code();
                    }
                    refuse(reason, code);
                    None
                }
            }
        }
        Ok(TabToAgent::Resume { client_nonce, proof, .. }) => {
            let resumed = state.pairing.lock().expect("pairing lock").try_resume(agent_nonce, &client_nonce, &proof, now);
            match resumed {
                Ok(ack) => {
                    let generation = take_over(state, tx);
                    let _ = tx.send(text(&AgentToTab::Resumed { proof: ack, caps: state.caps.clone() }));
                    Some(generation)
                }
                Err(reason) => {
                    refuse(reason, close::BAD_TOKEN);
                    None
                }
            }
        }
        _ => {
            let _ = tx.send(Outgoing::Close(close::TIMEOUT, "expected pairing"));
            None
        }
    }
}

/// Make this socket the session's connection, closing an older one of the same session.
fn take_over(state: &Arc<AgentState>, tx: &mpsc::UnboundedSender<Outgoing>) -> u64 {
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let previous = state.current.lock().expect("session lock").replace((generation, tx.clone()));
    if let Some((_, old)) = previous {
        let _ = old.send(Outgoing::Close(close::REPLACED, "replaced by a newer connection"));
    }
    generation
}

async fn serve(stream: &mut futures_util::stream::SplitStream<WebSocket>, state: &Arc<AgentState>, tx: &mpsc::UnboundedSender<Outgoing>) {
    let cfg = &state.cfg;
    let mut bucket = TokenBucket::new(cfg.message_burst, cfg.messages_per_sec, Instant::now());
    let mut last_rx = Instant::now();
    let mut over_limit_since: Option<Instant> = None;
    let mut check = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            msg = stream.next() => {
                let now = Instant::now();
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                        last_rx = now;
                        continue;
                    }
                    Some(Ok(Message::Binary(_))) => continue,
                    _ => return,
                };
                last_rx = now;
                if !bucket.try_take(now) {
                    let since = *over_limit_since.get_or_insert(now);
                    if now.duration_since(since) > Duration::from_secs(10) {
                        let _ = tx.send(Outgoing::Close(close::LIMITS, "too much input"));
                        return;
                    }
                    continue;
                }
                over_limit_since = None;
                let Ok(message) = serde_json::from_str::<TabToAgent>(&text) else {
                    continue;
                };
                match message {
                    TabToAgent::Screen { surface, width, height, .. } => {
                        state.engine.send(EngineCmd::Screen { surface, width, height });
                    }
                    TabToAgent::UseMonitor { id } => state.engine.send(EngineCmd::UseMonitor(id)),
                    TabToAgent::Control { who, name, kbm, pad } => state.engine.send(EngineCmd::Control { who, name, kbm, pad }),
                    TabToAgent::Input { who, ev } => {
                        if ev.len() <= cfg.max_events_per_frame {
                            state.engine.send(EngineCmd::Input { who, events: ev });
                        }
                    }
                    TabToAgent::Bye => {
                        state.engine.send(EngineCmd::EndSession);
                        state.current.lock().expect("session lock").take();
                        state.pairing.lock().expect("pairing lock").end_session();
                        state.print("dchat ended remote control.");
                        state.announce_code();
                        let _ = tx.send(Outgoing::Close(1000, "bye"));
                        return;
                    }
                    TabToAgent::Hello { .. } | TabToAgent::Resume { .. } => {}
                }
            }
            _ = check.tick() => {
                if last_rx.elapsed() > cfg.idle_timeout {
                    let _ = tx.send(Outgoing::Close(close::TIMEOUT, "no answer"));
                    return;
                }
            }
        }
    }
}
