//! End-to-end tests of dchat-relay over real WebSockets.

use futures_util::{SinkExt, StreamExt};
use protocol::{NostrBurnerKey, NostrEvent, KIND_EPHEMERAL_SIGNAL};
use relay::{router, RelayConfig};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start(cfg: RelayConfig) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(cfg).into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    addr
}

async fn connect(addr: SocketAddr) -> Ws {
    connect_async(format!("ws://{addr}/")).await.expect("connect").0
}

async fn send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string().into())).await.unwrap();
}

/// Next JSON message, skipping pings; panics after 3 s.
async fn recv(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timed out waiting for a message")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

/// Assert nothing arrives within 300 ms.
async fn expect_silence(ws: &mut Ws) {
    if let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout(Duration::from_millis(300), ws.next()).await {
        panic!("unexpected message: {text}");
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn signal(kind: u64, topic: &str, created_at: u64) -> NostrEvent {
    NostrBurnerKey::generate()
        .unwrap()
        .create_event(kind, vec![vec!["d".into(), topic.into()]], "ciphertext".into(), created_at)
        .unwrap()
}

async fn subscribe(ws: &mut Ws, sub: &str, topic: &str) {
    send(ws, json!(["REQ", sub, {"kinds": [KIND_EPHEMERAL_SIGNAL], "#d": [topic]}])).await;
    assert_eq!(recv(ws).await, json!(["EOSE", sub]));
}

#[tokio::test]
async fn routes_events_to_matching_subscribers_only() {
    let addr = start(RelayConfig::default()).await;
    let (mut alice, mut bob, mut eve) = (connect(addr).await, connect(addr).await, connect(addr).await);
    subscribe(&mut alice, "a", "room-1").await;
    subscribe(&mut bob, "b", "room-1").await;
    subscribe(&mut eve, "e", "room-2").await;

    let event = signal(KIND_EPHEMERAL_SIGNAL, "room-1", now());
    send(&mut alice, json!(["EVENT", event])).await;
    assert_eq!(recv(&mut alice).await, json!(["OK", event.id, true, ""]));

    let delivered = recv(&mut bob).await;
    assert_eq!(delivered[0], "EVENT");
    assert_eq!(delivered[1], "b");
    assert_eq!(delivered[2]["id"], json!(event.id));
    expect_silence(&mut eve).await;
    // The sender does not get its own event back.
    expect_silence(&mut alice).await;

    // Nothing is stored: a late subscriber gets no backlog.
    let mut late = connect(addr).await;
    subscribe(&mut late, "l", "room-1").await;
    expect_silence(&mut late).await;
}

#[tokio::test]
async fn rejects_events_outside_the_policy() {
    let addr = start(RelayConfig::default()).await;
    let (mut alice, mut bob) = (connect(addr).await, connect(addr).await);
    subscribe(&mut bob, "b", "room").await;

    let cases = [
        (signal(1, "room", now()), "blocked:"),
        (signal(KIND_EPHEMERAL_SIGNAL, "room", now() - 3600), "invalid: created_at is too old"),
        (signal(KIND_EPHEMERAL_SIGNAL, "room", now() + 3600), "invalid: created_at is in the future"),
        (
            {
                let mut e = signal(KIND_EPHEMERAL_SIGNAL, "room", now());
                e.content = "tampered".into();
                e
            },
            "invalid: bad signature",
        ),
    ];
    for (event, reason) in cases {
        send(&mut alice, json!(["EVENT", event])).await;
        let reply = recv(&mut alice).await;
        assert_eq!(reply[0], "OK");
        assert_eq!(reply[2], false, "{reply}");
        assert!(reply[3].as_str().unwrap().starts_with(reason), "{reply}");
    }
    expect_silence(&mut bob).await;

    // A short signature must be refused without crashing the relay.
    let mut short = signal(KIND_EPHEMERAL_SIGNAL, "room", now());
    short.sig = "00".into();
    send(&mut alice, json!(["EVENT", short])).await;
    assert_eq!(recv(&mut alice).await[2], false);
}

#[tokio::test]
async fn limits_subscriptions_and_event_rate() {
    let cfg = RelayConfig {
        max_subscriptions: 2,
        event_burst: 3,
        events_per_sec: 1,
        ..RelayConfig::default()
    };
    let addr = start(cfg).await;
    let mut ws = connect(addr).await;
    subscribe(&mut ws, "s1", "room").await;
    subscribe(&mut ws, "s2", "room").await;
    send(&mut ws, json!(["REQ", "s3", {"kinds": [KIND_EPHEMERAL_SIGNAL]}])).await;
    let closed = recv(&mut ws).await;
    assert_eq!(closed[0], "CLOSED");
    assert_eq!(closed[1], "s3");
    // Replacing an existing subscription is allowed.
    subscribe(&mut ws, "s1", "other").await;

    let mut accepted = 0;
    for _ in 0..5 {
        let event = signal(KIND_EPHEMERAL_SIGNAL, "room", now());
        send(&mut ws, json!(["EVENT", event])).await;
        let reply = recv(&mut ws).await;
        if reply[2] == true {
            accepted += 1;
        } else {
            assert!(reply[3].as_str().unwrap().starts_with("rate-limited:"), "{reply}");
        }
    }
    assert_eq!(accepted, 3, "the burst allowance, then rate-limited");
}

#[tokio::test]
async fn caps_connections_per_ip() {
    let addr = start(RelayConfig { max_connections_per_ip: 2, ..RelayConfig::default() }).await;
    let _a = connect(addr).await;
    let _b = connect(addr).await;
    let mut third = connect(addr).await;
    let notice = recv(&mut third).await;
    assert_eq!(notice[0], "NOTICE");
    assert!(notice[1].as_str().unwrap().starts_with("rate-limited:"));
    // Closing a connection frees its slot.
    drop(_a);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut again = connect(addr).await;
    subscribe(&mut again, "s", "room").await;
}

#[tokio::test]
async fn enforces_the_origin_allow_list() {
    let addr = start(RelayConfig {
        allowed_origins: vec!["https://chat.example.com".into()],
        ..RelayConfig::default()
    })
    .await;
    let request = |origin: &str| {
        let mut req = format!("ws://{addr}/").into_client_request().unwrap();
        req.headers_mut().insert("Origin", origin.parse().unwrap());
        req
    };
    assert!(connect_async(request("https://evil.example")).await.is_err());
    let (mut ok, _) = connect_async(request("https://chat.example.com")).await.unwrap();
    subscribe(&mut ok, "s", "room").await;
}

#[tokio::test]
async fn serves_the_nip11_information_document() {
    let addr = start(RelayConfig::default()).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: relay\r\nAccept: application/nostr+json\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("application/nostr+json"));
    let body: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["software"], "dchat-relay");
    assert_eq!(body["supported_nips"], json!([1, 11, 16]));
}

#[tokio::test]
async fn rejects_oversized_messages() {
    let addr = start(RelayConfig { max_message_bytes: 1024, ..RelayConfig::default() }).await;
    let mut ws = connect(addr).await;
    let _ = ws.send(Message::Text("x".repeat(4096).into())).await;
    // The relay closes the connection instead of processing it.
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                _ => continue,
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true));
}
