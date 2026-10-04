//! `dchat-host`: run it on the computer whose screen you share, then type its code into
//! dchat's "Remote control" dialog.

use clap::Parser;
use host_agent::config::{built_in_origins, normalize_origins, origin_from_input, AgentConfig};
use host_agent::engine::Engine;
use host_agent::inject::mock::Recorder;
use host_agent::inject::{platform_injector, Injector};
use host_agent::engine::EngineHandle;
use host_agent::{monitors, router, start, stop};
use protocol::DEFAULT_AGENT_PORT;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

/// For the panic hook: release everything held even if dchat-host crashes.
static ENGINE: OnceLock<EngineHandle> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(version, about = "Let members you allow in dchat control this computer while you share your screen")]
struct Args {
    /// Local port the dchat tab connects to (only 127.0.0.1 is ever listened on).
    #[arg(long, default_value_t = DEFAULT_AGENT_PORT, env = "DCHAT_HOST_PORT")]
    port: u16,

    /// Address of a dchat site allowed to connect, e.g. https://chat.example.com (repeatable).
    #[arg(long = "allow-origin", env = "DCHAT_HOST_ALLOWED_ORIGINS", value_delimiter = ',')]
    allow_origins: Vec<String>,

    /// Which monitor is shared, by number from --list-monitors (when it can't be detected).
    #[arg(long)]
    monitor: Option<u32>,

    /// Print the monitors and exit.
    #[arg(long)]
    list_monitors: bool,

    /// Release held keys and buttons after this long without input from the controller.
    #[arg(long, default_value_t = 1500)]
    watchdog_ms: u64,

    /// Record input instead of injecting it (testing).
    #[arg(long)]
    mock_injector: bool,

    /// With --mock-injector: print each recorded event as a JSON line.
    #[arg(long, requires = "mock_injector")]
    print_events: bool,

    /// With --mock-injector: use this pairing code and never rotate it.
    #[arg(long, requires = "mock_injector")]
    test_code: Option<String>,

    /// With --mock-injector: how many virtual controllers to pretend to have.
    #[arg(long, requires = "mock_injector", default_value_t = 4)]
    mock_pads: u8,

    /// Don't register the global stop shortcut (Ctrl+Alt+Shift+Q).
    #[arg(long)]
    no_hotkey: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Logs go to stderr and never contain input: only errors and counts.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .init();
    let args = Args::parse();
    #[cfg(windows)]
    host_agent::inject::windows::use_physical_pixels();

    if args.list_monitors {
        let found = monitors::detect();
        if found.is_empty() {
            println!("No monitor layout available (positions map onto the whole desktop).");
        }
        for m in found {
            let primary = if m.primary { ", primary" } else { "" };
            println!("{}: {} {}x{} at {},{}{primary}", m.id, m.name, m.pixels.0, m.pixels.1, m.rect.x, m.rect.y);
        }
        return Ok(());
    }

    let interactive = std::io::stdin().is_terminal();
    let mut origins = built_in_origins();
    origins.extend(normalize_origins(args.allow_origins.iter().map(String::as_str)));
    let mut origins = normalize_origins(origins.iter().map(String::as_str));
    if origins.is_empty() {
        if !interactive {
            fatal("No dchat site is allowed to connect. Start with --allow-origin https://<your dchat site>.", 2, false);
        }
        origins.push(ask_for_site());
    }

    let (injector, recording): (Box<dyn Injector>, _) = if args.mock_injector {
        let mut recorder = Recorder::new(args.mock_pads);
        recorder.print = args.print_events;
        let log = recorder.log.clone();
        (Box::new(recorder), Some(log))
    } else {
        (platform_injector(), None)
    };
    // Recording (tests): no monitor layout, so positions don't depend on the test machine.
    let monitor_source: host_agent::engine::MonitorSource =
        if args.mock_injector { Box::new(Vec::new) } else { Box::new(monitors::detect) };
    let engine = Engine::new(injector, monitor_source, args.monitor, Duration::from_millis(args.watchdog_ms));

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => fatal(
            &format!("Cannot listen on {addr}: {err}. Is dchat-host already running? (--port picks another port)"),
            1,
            interactive,
        ),
    };

    let (status_tx, mut status_rx) = mpsc::unbounded_channel::<String>();
    let cfg = AgentConfig { port: args.port, allowed_origins: origins.clone(), test_api: args.mock_injector, ..AgentConfig::default() };
    let agent = start(cfg, engine, args.test_code.clone(), recording, status_tx);
    let _ = ENGINE.set(agent.engine.clone());
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(engine) = ENGINE.get() {
            engine.shutdown();
        }
        default_hook(info);
    }));
    let caps = agent.state.caps.clone();

    println!("dchat-host {} · ws://{addr} · allowed: {}", env!("CARGO_PKG_VERSION"), origins.join(", "));
    match &caps.mouse_keyboard_error {
        None => println!("Mouse and keyboard: ready"),
        Some(err) => println!("Mouse and keyboard: unavailable: {err}"),
    }
    #[cfg(windows)]
    if !args.mock_injector && !host_agent::inject::windows::is_elevated() {
        println!("Note: windows of apps running as administrator can't be controlled unless dchat-host runs as administrator too.");
    }
    match &caps.pads_error {
        None => println!("Controllers: up to {}", caps.pads),
        Some(err) => println!("Controllers: unavailable: {err}"),
    }
    let shortcut = if args.no_hotkey {
        Err("turned off".to_string())
    } else {
        let state = agent.state.clone();
        stop::watch(move || state.stop(&format!("{} pressed on this computer", stop::STOP_SHORTCUT)))
    };
    let mut ways = Vec::new();
    if shortcut.is_ok() {
        ways.push(format!("{} anywhere", stop::STOP_SHORTCUT));
    }
    if interactive {
        ways.push("Enter here".to_string());
    }
    ways.push("Ctrl+C".to_string());
    ways.push("Stop in dchat".to_string());
    println!("Stop remote control: {}", ways.join(", "));
    if let Err(why) = shortcut {
        println!("({} shortcut: {why})", stop::STOP_SHORTCUT);
    }
    if let Some(code) = agent.state.pairing_code() {
        println!("Pairing code: {code}");
    }
    tokio::spawn(async move {
        while let Some(line) = status_rx.recv().await {
            println!("{line}");
        }
    });

    if interactive {
        let state = agent.state.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                state.stop("Enter pressed on this computer");
                line.clear();
            }
        });
    }

    let state = agent.state.clone();
    let engine = agent.engine.clone();
    axum::serve(listener, router(agent.state.clone()))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            state.stop("dchat-host closed");
            engine.shutdown();
        })
        .await?;
    Ok(())
}

/// No site was built in or given: ask (double-clicking on Windows lands here). Only the
/// site's origin is kept; nothing typed is stored, and if someone pastes a whole room link
/// anyway, its path and key are dropped and never printed.
fn ask_for_site() -> String {
    println!("Which dchat site may connect to this computer? Type its address, for example");
    println!("https://chat.example.com, then press Enter. Only that site will be able to use dchat-host.");
    loop {
        print!("> ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => std::process::exit(2),
            Ok(_) => {}
        }
        match origin_from_input(&line) {
            Some(origin) => {
                println!("Only {origin} may connect.");
                return origin;
            }
            None => println!("That doesn't look like a web address (for example https://chat.example.com). Try again:"),
        }
    }
}

/// Print why dchat-host can't run and exit. On Windows a double-clicked console window
/// would close at once, so it waits for Enter first.
fn fatal(message: &str, code: i32, interactive: bool) -> ! {
    eprintln!("{message}");
    if cfg!(windows) && interactive {
        eprintln!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(code)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    // Also when the terminal is closed, or Windows closes the console or logs off.
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match (signal(SignalKind::terminate()), signal(SignalKind::hangup())) {
            (Ok(mut term), Ok(mut hup)) => {
                tokio::select! {
                    _ = term.recv() => {},
                    _ = hup.recv() => {},
                }
            }
            _ => std::future::pending::<()>().await,
        }
    };
    #[cfg(windows)]
    let terminate = async {
        use tokio::signal::windows::{ctrl_close, ctrl_logoff, ctrl_shutdown};
        match (ctrl_close(), ctrl_logoff(), ctrl_shutdown()) {
            (Ok(mut close), Ok(mut logoff), Ok(mut shutdown)) => {
                tokio::select! {
                    _ = close.recv() => {},
                    _ = logoff.recv() => {},
                    _ = shutdown.recv() => {},
                }
            }
            _ => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(any(unix, windows)))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
