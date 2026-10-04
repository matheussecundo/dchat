//! dchat-host over real WebSockets, with the recording injector.

use futures_util::{SinkExt, StreamExt};
use host_agent::engine::Engine;
use host_agent::inject::mock::{Injected, Recorder, Recording};
use host_agent::{router, start, AgentConfig, AgentState};
use protocol::{
    pair_ack_matches, pair_proof, resume_ack_matches, resume_proof, AgentToTab, DomCode, InputEvent, PointerMode,
    RefuseReason, TabToAgent, AGENT_PROTOCOL_VERSION,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
const ORIGIN: &str = "https://chat.example.com";

struct Harness {
    addr: SocketAddr,
    log: Recording,
    state: Arc<AgentState>,
}

async fn start_agent(fixed_code: Option<&str>) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let recorder = Recorder::new(4);
    let log = recorder.log.clone();
    let engine = Engine::new(Box::new(recorder), Box::new(Vec::new), None, Duration::from_millis(300));
    let cfg = AgentConfig {
        port: addr.port(),
        allowed_origins: vec![ORIGIN.into()],
        pre_auth_timeout: Duration::from_millis(500),
        bad_code_delay: Duration::from_millis(10),
        test_api: true,
        ..AgentConfig::default()
    };
    let (status_tx, _status_rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(_status_rx);
    let agent = start(cfg, engine, fixed_code.map(String::from), Some(log.clone()), status_tx);
    let app = router(agent.state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness { addr, log, state: agent.state }
}

async fn connect_with(addr: SocketAddr, origin: Option<&str>, host: Option<&str>) -> Result<Ws, String> {
    let mut req = format!("ws://{addr}/").into_client_request().unwrap();
    if let Some(origin) = origin {
        req.headers_mut().insert("Origin", origin.parse().unwrap());
    }
    if let Some(host) = host {
        req.headers_mut().insert("Host", host.parse().unwrap());
    }
    connect_async(req).await.map(|(ws, _)| ws).map_err(|e| e.to_string())
}

async fn connect(addr: SocketAddr) -> Ws {
    connect_with(addr, Some(ORIGIN), None).await.expect("connect")
}

async fn send(ws: &mut Ws, msg: &TabToAgent) {
    ws.send(Message::Text(serde_json::to_string(msg).unwrap())).await.unwrap();
}

/// Next message from the app; `None` once the socket is closed. Panics after 3 s.
async fn recv(ws: &mut Ws) -> Option<AgentToTab> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(3), ws.next()).await.expect("timed out");
        match msg {
            Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return None,
        }
    }
}

async fn challenge(ws: &mut Ws) -> String {
    match recv(ws).await {
        Some(AgentToTab::Challenge { v, nonce, .. }) => {
            assert_eq!(v, AGENT_PROTOCOL_VERSION);
            nonce
        }
        other => panic!("expected a challenge, got {other:?}"),
    }
}

/// Pair with `code`; returns the session token.
async fn pair(ws: &mut Ws, code: &str) -> String {
    let nonce = challenge(ws).await;
    send(ws, &TabToAgent::Hello { v: AGENT_PROTOCOL_VERSION, client_nonce: "tab-nonce".into(), proof: pair_proof(code, &nonce, "tab-nonce") }).await;
    match recv(ws).await {
        Some(AgentToTab::Paired { proof, token, caps }) => {
            assert!(pair_ack_matches(code, "tab-nonce", &nonce, &proof), "the app proves it knows the code");
            assert!(caps.mouse_keyboard);
            token
        }
        other => panic!("expected Paired, got {other:?}"),
    }
}

async fn recorded(log: &Recording, expected: usize) -> Vec<Injected> {
    for _ in 0..100 {
        let events = log.lock().unwrap().clone();
        if events.len() >= expected {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    log.lock().unwrap().clone()
}

fn key(code: DomCode, down: bool) -> InputEvent {
    InputEvent::Key { code, down, repeat: false }
}

#[tokio::test]
async fn pairs_and_injects_only_granted_input() {
    let h = start_agent(Some("TEST0000")).await;
    let mut ws = connect(h.addr).await;
    pair(&mut ws, "test-0000").await;
    send(&mut ws, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::KeyA, true)] }).await;
    send(&mut ws, &TabToAgent::Control { who: 1, name: "Bo · 3fa2".into(), kbm: Some(PointerMode::Desktop), pad: None }).await;
    send(&mut ws, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::KeyB, true), key(DomCode::KeyB, false)] }).await;
    send(&mut ws, &TabToAgent::Input { who: 2, ev: vec![key(DomCode::KeyC, true)] }).await;
    let events = recorded(&h.log, 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(events, vec![Injected::Key { code: DomCode::KeyB, down: true }, Injected::Key { code: DomCode::KeyB, down: false }]);
    assert_eq!(h.log.lock().unwrap().len(), 2, "nothing from members without a grant");
}

#[tokio::test]
async fn refuses_other_origins_and_hosts() {
    let h = start_agent(Some("TEST0000")).await;
    assert!(connect_with(h.addr, None, None).await.is_err(), "no Origin");
    assert!(connect_with(h.addr, Some("null"), None).await.is_err());
    assert!(connect_with(h.addr, Some("https://evil.example"), None).await.is_err());
    let rebinding = format!("evil.example:{}", h.addr.port());
    assert!(connect_with(h.addr, Some(ORIGIN), Some(&rebinding)).await.is_err(), "DNS rebinding");
    let localhost = format!("localhost:{}", h.addr.port());
    assert!(connect_with(h.addr, Some(ORIGIN), Some(&localhost)).await.is_ok());
}

#[tokio::test]
async fn wrong_codes_lock_pairing_and_burn_the_code() {
    let h = start_agent(None).await;
    let code = h.state.pairing_code().unwrap();
    for attempt in 0..5 {
        let mut ws = connect(h.addr).await;
        let nonce = challenge(&mut ws).await;
        send(&mut ws, &TabToAgent::Hello { v: AGENT_PROTOCOL_VERSION, client_nonce: "c".into(), proof: pair_proof("WRONG000", &nonce, "c") }).await;
        let expected = if attempt < 4 { RefuseReason::BadCode } else { RefuseReason::Locked { retry_secs: 30 } };
        assert_eq!(recv(&mut ws).await, Some(AgentToTab::Refused { reason: expected }));
        assert_eq!(recv(&mut ws).await, None, "closed after refusing");
    }
    assert_ne!(h.state.pairing_code().unwrap(), code, "the code was burned");
    let mut ws = connect(h.addr).await;
    let nonce = challenge(&mut ws).await;
    send(&mut ws, &TabToAgent::Hello { v: AGENT_PROTOCOL_VERSION, client_nonce: "c".into(), proof: pair_proof(&code, &nonce, "c") }).await;
    assert!(matches!(recv(&mut ws).await, Some(AgentToTab::Refused { reason: RefuseReason::Locked { .. } })));
}

#[tokio::test]
async fn one_session_at_a_time_and_version_checked() {
    let h = start_agent(Some("TEST0000")).await;
    let mut first = connect(h.addr).await;
    pair(&mut first, "TEST0000").await;
    let mut second = connect(h.addr).await;
    let nonce = challenge(&mut second).await;
    send(&mut second, &TabToAgent::Hello { v: AGENT_PROTOCOL_VERSION, client_nonce: "c".into(), proof: pair_proof("TEST0000", &nonce, "c") }).await;
    assert_eq!(recv(&mut second).await, Some(AgentToTab::Refused { reason: RefuseReason::Busy }));

    let mut old = connect(h.addr).await;
    challenge(&mut old).await;
    send(&mut old, &TabToAgent::Hello { v: 999, client_nonce: "c".into(), proof: "p".into() }).await;
    assert_eq!(recv(&mut old).await, Some(AgentToTab::Refused { reason: RefuseReason::Version { agent_v: AGENT_PROTOCOL_VERSION } }));
}

#[tokio::test]
async fn sockets_must_pair_first_and_quickly() {
    let h = start_agent(Some("TEST0000")).await;
    let mut early = connect(h.addr).await;
    challenge(&mut early).await;
    send(&mut early, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::KeyA, true)] }).await;
    assert_eq!(recv(&mut early).await, None, "input before pairing closes the socket");

    let mut silent = connect(h.addr).await;
    challenge(&mut silent).await;
    assert_eq!(recv(&mut silent).await, Some(AgentToTab::Refused { reason: RefuseReason::Timeout }));
    assert!(h.log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn losing_the_tab_releases_everything_and_resume_keeps_the_grants() {
    let h = start_agent(Some("TEST0000")).await;
    let mut ws = connect(h.addr).await;
    let token = pair(&mut ws, "TEST0000").await;
    send(&mut ws, &TabToAgent::Control { who: 1, name: "Bo".into(), kbm: Some(PointerMode::Desktop), pad: None }).await;
    send(&mut ws, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::ShiftLeft, true)] }).await;
    recorded(&h.log, 1).await;
    drop(ws);
    let events = recorded(&h.log, 2).await;
    assert_eq!(events[1], Injected::Key { code: DomCode::ShiftLeft, down: false }, "released when the tab went away");

    let mut again = connect(h.addr).await;
    let nonce = challenge(&mut again).await;
    send(&mut again, &TabToAgent::Resume { v: AGENT_PROTOCOL_VERSION, client_nonce: "c2".into(), proof: resume_proof(&token, &nonce, "c2") }).await;
    match recv(&mut again).await {
        Some(AgentToTab::Resumed { proof, .. }) => assert!(resume_ack_matches(&token, "c2", &nonce, &proof)),
        other => panic!("expected Resumed, got {other:?}"),
    }
    send(&mut again, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::KeyX, true)] }).await;
    assert_eq!(recorded(&h.log, 3).await[2], Injected::Key { code: DomCode::KeyX, down: true });

    let mut forged = connect(h.addr).await;
    let nonce = challenge(&mut forged).await;
    send(&mut forged, &TabToAgent::Resume { v: AGENT_PROTOCOL_VERSION, client_nonce: "c".into(), proof: resume_proof("guess", &nonce, "c") }).await;
    assert_eq!(recv(&mut forged).await, Some(AgentToTab::Refused { reason: RefuseReason::BadToken }));
}

#[tokio::test]
async fn a_reloaded_tab_can_pair_again_right_away() {
    let h = start_agent(None).await;
    let first = h.state.pairing_code().unwrap();
    let mut ws = connect(h.addr).await;
    pair(&mut ws, &first).await;
    assert_eq!(h.state.pairing_code(), None, "no code while a tab is connected");
    drop(ws);
    let mut next_code = None;
    for _ in 0..50 {
        next_code = h.state.pairing_code();
        if next_code.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut reloaded = connect(h.addr).await;
    pair(&mut reloaded, &next_code.expect("a new code once the tab is gone")).await;
}

#[tokio::test]
async fn stopping_on_the_host_ends_the_session() {
    let h = start_agent(Some("TEST0000")).await;
    let mut ws = connect(h.addr).await;
    pair(&mut ws, "TEST0000").await;
    send(&mut ws, &TabToAgent::Control { who: 1, name: "Bo".into(), kbm: Some(PointerMode::Desktop), pad: None }).await;
    send(&mut ws, &TabToAgent::Input { who: 1, ev: vec![key(DomCode::KeyA, true)] }).await;
    recorded(&h.log, 1).await;
    h.state.stop("test");
    assert_eq!(recv(&mut ws).await, Some(AgentToTab::Stopped { reason: "test".into() }));
    assert_eq!(recv(&mut ws).await, None);
    assert_eq!(recorded(&h.log, 2).await[1], Injected::Key { code: DomCode::KeyA, down: false });
    // A new pairing works right away.
    let mut next = connect(h.addr).await;
    pair(&mut next, "TEST0000").await;
}

#[tokio::test]
async fn bye_ends_the_session_and_oversized_frames_close_it() {
    let h = start_agent(Some("TEST0000")).await;
    let mut ws = connect(h.addr).await;
    pair(&mut ws, "TEST0000").await;
    send(&mut ws, &TabToAgent::Bye).await;
    assert_eq!(recv(&mut ws).await, None);
    let mut next = connect(h.addr).await;
    pair(&mut next, "TEST0000").await;
    let _ = next.send(Message::Text("x".repeat(64 * 1024))).await;
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match next.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                _ => continue,
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true));
}

#[tokio::test]
async fn health_and_test_endpoints() {
    let h = start_agent(Some("TEST0000")).await;
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(format!("GET /__test/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", h.addr.port()).as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("[]"), "{response}");
}
