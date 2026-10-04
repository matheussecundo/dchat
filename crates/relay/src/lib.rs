//! dchat-relay: a minimal, RAM-only Nostr relay for dchat signaling.
//!
//! It forwards ephemeral, signed, room-key-encrypted handshake events (kind 20001) between
//! members of the same room topic and stores nothing. Used by the `dchat-relay` binary for
//! production and by the dev server at `/nostr`.

mod connection;
mod hub;
pub mod limits;

pub use limits::RelayConfig;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use hub::Hub;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
struct RelayState {
    hub: Arc<Hub>,
    cfg: Arc<RelayConfig>,
}

/// The relay as an axum router: WebSocket at `/`, NIP-11 information document on plain GET.
/// Serve it with `into_make_service_with_connect_info::<SocketAddr>()` for per-IP limits.
pub fn router(cfg: RelayConfig) -> Router {
    let state = RelayState {
        hub: Arc::new(Hub::default()),
        cfg: Arc::new(cfg),
    };
    Router::new().route("/", get(entry)).with_state(state)
}

async fn entry(
    State(state): State<RelayState>,
    headers: HeaderMap,
    connect: Option<ConnectInfo<SocketAddr>>,
    ws: Option<WebSocketUpgrade>,
) -> Response {
    let Some(ws) = ws else {
        return info_document(&state.cfg, &headers);
    };
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !state.cfg.origin_allowed(origin) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let ip = client_ip(&headers, connect.map(|c| c.0), state.cfg.trust_proxy);
    let (hub, cfg) = (state.hub.clone(), state.cfg.clone());
    ws.max_message_size(state.cfg.max_message_bytes)
        .on_upgrade(move |socket| connection::handle(socket, hub, cfg, ip))
}

fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> String {
    if trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(ip) = forwarded {
            return ip.to_string();
        }
    }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".into())
}

/// NIP-11: clients asking with `Accept: application/nostr+json` get the relay's limits.
fn info_document(cfg: &RelayConfig, headers: &HeaderMap) -> Response {
    let wants_info = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/nostr+json"));
    if !wants_info {
        return "dchat-relay: connect with a Nostr client over WebSocket\n".into_response();
    }
    let body = serde_json::json!({
        "name": cfg.name,
        "description": "RAM-only relay for dchat signaling: ephemeral events only, nothing stored.",
        "software": "dchat-relay",
        "version": env!("CARGO_PKG_VERSION"),
        "supported_nips": [1, 11, 16],
        "limitation": {
            "max_message_length": cfg.max_message_bytes,
            "max_subscriptions": cfg.max_subscriptions,
            "max_filters": cfg.max_filters_per_req,
            "auth_required": false,
            "payment_required": false,
            "restricted_writes": true
        }
    });
    (
        [
            (header::CONTENT_TYPE, "application/nostr+json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body.to_string(),
    )
        .into_response()
}
