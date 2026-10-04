//! dchat-host: lets the members you allow in dchat control this computer's mouse, keyboard
//! and game controllers while you share your screen. Your dchat tab connects to it over
//! `ws://127.0.0.1` after you type the code it shows; it injects only what the tab forwards
//! from members you granted control, and releases everything when control ends.

pub mod config;
pub mod engine;
pub mod geometry;
pub mod inject;
pub mod keymap;
pub mod monitors;
pub mod pad_map;
pub mod pairing;
pub mod server;
pub mod status;
pub mod stop;

pub use config::AgentConfig;
pub use server::{router, AgentState};

use engine::{Engine, EngineHandle};
use inject::mock::Recording;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct Agent {
    pub state: Arc<AgentState>,
    pub engine: EngineHandle,
}

/// Start the input engine thread and route its events. Call inside a tokio runtime.
pub fn start(
    cfg: AgentConfig,
    engine: Engine,
    fixed_code: Option<String>,
    recording: Option<Recording>,
    status: mpsc::UnboundedSender<String>,
) -> Agent {
    let caps = engine.caps().to_agent_caps();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let handle = engine::spawn(engine, events_tx);
    let state = AgentState::new(cfg, caps, handle.clone(), fixed_code, recording, status);
    let dispatch = state.clone();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            dispatch.engine_event(event);
        }
    });
    Agent { state, engine: handle }
}
