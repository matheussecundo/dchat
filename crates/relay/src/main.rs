//! `dchat-relay`: run the dchat Nostr relay. Put it behind a TLS reverse proxy (Caddy):
//! browsers on an HTTPS page can only open `wss://` connections.

use clap::Parser;
use relay::{router, RelayConfig};
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "RAM-only Nostr relay for dchat signaling (ephemeral events only, nothing stored)")]
struct Args {
    /// Address to listen on (plain WebSocket; terminate TLS in front of it).
    #[arg(long, default_value = "127.0.0.1:7447", env = "DCHAT_RELAY_LISTEN")]
    listen: SocketAddr,

    /// Only accept browsers from these origins, e.g. https://chat.example.com (repeatable).
    /// Default: any origin.
    #[arg(long = "allowed-origin", env = "DCHAT_RELAY_ALLOWED_ORIGINS", value_delimiter = ',')]
    allowed_origins: Vec<String>,

    /// Read the client IP from X-Forwarded-For (set when behind a proxy you control).
    #[arg(long, env = "DCHAT_RELAY_TRUST_PROXY")]
    trust_proxy: bool,

    /// Open connections allowed per client IP.
    #[arg(long, default_value_t = 64, env = "DCHAT_RELAY_MAX_CONNECTIONS_PER_IP")]
    max_connections_per_ip: usize,

    /// Relay name shown in its NIP-11 information document.
    #[arg(long, default_value = "dchat-relay", env = "DCHAT_RELAY_NAME")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Logs carry counts and errors only: never IP addresses, room topics or content.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args = Args::parse();
    let cfg = RelayConfig {
        // `DCHAT_RELAY_ALLOWED_ORIGINS=` (empty) means any origin, not "an empty origin".
        allowed_origins: args
            .allowed_origins
            .into_iter()
            .map(|o| o.trim().trim_end_matches('/').to_string())
            .filter(|o| !o.is_empty())
            .collect(),
        trust_proxy: args.trust_proxy,
        max_connections_per_ip: args.max_connections_per_ip,
        name: args.name,
        ..RelayConfig::default()
    };
    tracing::info!(
        "dchat-relay {} listening on {} (origins: {}, trust proxy: {})",
        env!("CARGO_PKG_VERSION"),
        args.listen,
        if cfg.allowed_origins.is_empty() { "any".to_string() } else { cfg.allowed_origins.join(", ") },
        cfg.trust_proxy
    );

    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    axum::serve(listener, router(cfg).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("dchat-relay shutting down");
}
