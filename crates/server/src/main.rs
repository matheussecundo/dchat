mod signaling;
mod tls;

use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use qrcode::render::unicode;
use qrcode::QrCode;
use signaling::{handle_websocket, AppState};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Clone, Default)]
pub struct ServerState {
    pub signaling: AppState,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Ephemeral P2P Chat Signaling & Static Server")]
pub struct Args {
    #[arg(short, long, default_value_t = 8443)]
    pub port: u16,

    #[arg(long, default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, default_value = "crates/client/dist")]
    pub static_dir: PathBuf,

    #[arg(long, help = "Run in HTTP mode instead of HTTPS (not recommended for mobile audio WebRTC)")]
    pub http: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let args = Args::parse();
    let state = ServerState::default();

    let lan_ip: Option<IpAddr> = local_ip_address::local_ip().ok();

    // Setup routes
    let static_dir = if args.static_dir.exists() {
        args.static_dir.clone()
    } else if PathBuf::from("dist").exists() {
        PathBuf::from("dist")
    } else {
        args.static_dir.clone()
    };

    let index_html = static_dir.join("index.html");

    let serve_dir = ServeDir::new(&static_dir)
        .not_found_service(ServeFile::new(&index_html));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        // The production relay code (crates/relay), so dev and E2E runs exercise it.
        .nest_service(
            "/nostr",
            relay::router(relay::RelayConfig {
                name: "dchat dev relay".into(),
                ..relay::RelayConfig::default()
            }),
        )
        .route("/health", get(|| async { "OK" }))
        .fallback_service(serve_dir)
        .layer(CorsLayer::permissive())
        .with_state(state);

    let socket_addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let scheme = if args.http { "http" } else { "https" };

    println!("\n{}", "=".repeat(68));
    println!("  🔒 dchat - Ephemeral Zero-Knowledge P2P Chat Server");
    println!("{}", "=".repeat(68));
    println!("  Mode:         {}", if args.http { "HTTP" } else { "HTTPS (Self-Signed Dev TLS)" });
    println!("  Localhost:    {}://localhost:{}", scheme, args.port);

    if let Some(ip) = lan_ip {
        let mobile_url = format!("{}://{}:{}", scheme, ip, args.port);
        println!("  Mobile LAN:   {}", mobile_url);
        println!("{}", "-".repeat(68));
        println!("  Scan this QR Code with your cellphones to connect:");

        if let Ok(code) = QrCode::new(mobile_url.as_bytes()) {
            let qr_string = code.render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Dark)
                .light_color(unicode::Dense1x2::Light)
                .build();
            println!("\n{}\n", qr_string);
        }

        if !args.http {
            println!("  [NOTE FOR PHONES]:");
            println!("  Because this uses a self-signed dev certificate for local testing,");
            println!("  tap 'Advanced' -> 'Proceed to site' when your phone browser opens.");
            println!("  This establishes a Secure Context (HTTPS), enabling WebRTC!");
        }
    } else {
        println!("  Mobile LAN:   (Could not detect local network IP)");
    }
    println!("{}\n", "=".repeat(68));

    if args.http {
        let listener = tokio::net::TcpListener::bind(socket_addr).await?;
        info!("Listening on HTTP at {}", socket_addr);
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    } else {
        let rustls_config = tls::generate_self_signed_config(lan_ip).await?;
        info!("Listening on HTTPS at {}", socket_addr);
        axum_server::bind_rustls(socket_addr, rustls_config)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    }

    Ok(())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state.signaling))
}

