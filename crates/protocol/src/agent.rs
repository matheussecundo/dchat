//! The link between the sharer's dchat tab and the local `dchat-host` app, which injects
//! remote-control input into the operating system. JSON over `ws://127.0.0.1:PORT`.
//!
//! Pairing is mutual: the app prints a one-time code, the user types it into dchat, and each
//! side proves it knows the code (HMAC over both nonces) before anything else happens. So
//! the app only obeys the user's own dchat tab, and the tab never sends keystrokes to some
//! other program that happens to listen on the port.

use crate::input::{InputEvent, PointerMode};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Bumped whenever these messages change; the tab and the app must match exactly.
pub const AGENT_PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_AGENT_PORT: u16 = 7448;
/// Crockford base32: no I, L, O or U, so a code read aloud or retyped survives.
pub const PAIRING_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const PAIRING_CODE_LENGTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentOs {
    Windows,
    Linux,
    Other,
}

/// What the app can inject on this computer, with the reason when something is missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentCaps {
    pub mouse_keyboard: bool,
    pub mouse_keyboard_error: Option<String>,
    /// How many virtual controllers it can create (0 when unavailable).
    pub pads: u8,
    pub pads_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "r")]
pub enum RefuseReason {
    BadCode,
    Locked { retry_secs: u64 },
    Busy,
    Version { agent_v: u32 },
    BadToken,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum TabToAgent {
    /// Pair with the code the user typed: `proof = pair_proof(code, nonce, client_nonce)`.
    Hello { v: u32, client_nonce: String, proof: String },
    /// Reconnect to the same session with the token from `Paired` (never sent in clear).
    Resume { v: u32, client_nonce: String, proof: String },
    /// What is being shared, from `track.getSettings()`.
    Screen { surface: String, width: u32, height: u32, label: String },
    UseMonitor { id: u32 },
    /// Everything member `who` may do now (both `None` ends their control).
    Control { who: u32, name: String, kbm: Option<PointerMode>, pad: Option<u8> },
    Input { who: u32, ev: Vec<InputEvent> },
    Bye,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum AgentToTab {
    Challenge { v: u32, nonce: String, os: AgentOs, agent: String },
    /// `proof = pair_ack(code, client_nonce, nonce)`: the tab checks it before trusting us.
    Paired { proof: String, token: String, caps: AgentCaps },
    Resumed { proof: String, caps: AgentCaps },
    Refused { reason: RefuseReason },
    Monitors { list: Vec<MonitorInfo>, chosen: Option<u32> },
    Warning { code: String },
    /// The user stopped remote control on this computer (hotkey, terminal, Ctrl+C).
    Stopped { reason: String },
}

/// Uppercase, no separators or spaces, and the Crockford look-alikes (O, I, L) fixed.
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// `K7QM4XPA` → `K7QM-4XPA`.
pub fn format_code(code: &str) -> String {
    let normalized = normalize_code(code);
    let (a, b) = normalized.split_at(normalized.len().min(4));
    if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}-{b}")
    }
}

fn mac(key: &[u8], label: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(label.as_bytes());
    mac
}

fn mac_hex(key: &[u8], label: &str, first: &str, second: &str) -> String {
    let mut mac = mac(key, label);
    for part in [first, second] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part.as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

fn mac_matches(key: &[u8], label: &str, first: &str, second: &str, proof: &str) -> bool {
    let Ok(expected) = hex::decode(proof) else {
        return false;
    };
    let mut mac = mac(key, label);
    for part in [first, second] {
        mac.update(&(part.len() as u32).to_be_bytes());
        mac.update(part.as_bytes());
    }
    mac.verify_slice(&expected).is_ok()
}

/// The tab's proof that the user typed the app's code.
pub fn pair_proof(code: &str, agent_nonce: &str, client_nonce: &str) -> String {
    mac_hex(normalize_code(code).as_bytes(), "dchat-agent-pair:v1", agent_nonce, client_nonce)
}

/// The app's proof back: it knows the code too.
pub fn pair_ack(code: &str, client_nonce: &str, agent_nonce: &str) -> String {
    mac_hex(normalize_code(code).as_bytes(), "dchat-agent-ack:v1", client_nonce, agent_nonce)
}

pub fn resume_proof(token: &str, agent_nonce: &str, client_nonce: &str) -> String {
    mac_hex(token.as_bytes(), "dchat-agent-resume:v1", agent_nonce, client_nonce)
}

pub fn resume_ack(token: &str, client_nonce: &str, agent_nonce: &str) -> String {
    mac_hex(token.as_bytes(), "dchat-agent-resume-ack:v1", client_nonce, agent_nonce)
}

/// Constant-time checks of the proofs above.
pub fn pair_proof_matches(code: &str, agent_nonce: &str, client_nonce: &str, proof: &str) -> bool {
    mac_matches(normalize_code(code).as_bytes(), "dchat-agent-pair:v1", agent_nonce, client_nonce, proof)
}

pub fn pair_ack_matches(code: &str, client_nonce: &str, agent_nonce: &str, proof: &str) -> bool {
    mac_matches(normalize_code(code).as_bytes(), "dchat-agent-ack:v1", client_nonce, agent_nonce, proof)
}

pub fn resume_proof_matches(token: &str, agent_nonce: &str, client_nonce: &str, proof: &str) -> bool {
    mac_matches(token.as_bytes(), "dchat-agent-resume:v1", agent_nonce, client_nonce, proof)
}

pub fn resume_ack_matches(token: &str, client_nonce: &str, agent_nonce: &str, proof: &str) -> bool {
    mac_matches(token.as_bytes(), "dchat-agent-resume-ack:v1", client_nonce, agent_nonce, proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{MouseButton, PadState};
    use crate::keycodes::DomCode;
    use sha2::Digest;

    /// The agent wire fingerprint recorded for `AGENT_PROTOCOL_VERSION`. When the test below
    /// fails, bump the version and record the new pair here.
    const RECORDED: (u32, &str) = (1, "5a4b41de133fb5c19a2e58d42e93e91af1f500b6e8744390903199f3472ed609");

    fn tab_messages() -> Vec<TabToAgent> {
        vec![
            TabToAgent::Hello { v: 1, client_nonce: "c".into(), proof: "p".into() },
            TabToAgent::Resume { v: 1, client_nonce: "c".into(), proof: "p".into() },
            TabToAgent::Screen { surface: "monitor".into(), width: 1920, height: 1080, label: "screen:0:0".into() },
            TabToAgent::UseMonitor { id: 2 },
            TabToAgent::Control { who: 1, name: "Bo · 3fa2".into(), kbm: Some(PointerMode::Game), pad: Some(1) },
            TabToAgent::Input {
                who: 1,
                ev: vec![
                    InputEvent::PointerAbs { x: 1, y: 2 },
                    InputEvent::PointerRel { dx: -1, dy: 1 },
                    InputEvent::Button { button: MouseButton::Left, down: true, at: Some((3, 4)) },
                    InputEvent::Wheel { dx: 0, dy: 120, at: None },
                    InputEvent::Key { code: DomCode::KeyA, down: true, repeat: false },
                    InputEvent::Pad { index: 0, state: PadState { buttons: 1, axes: [1, 2, 3, 4], triggers: [5, 6] } },
                    InputEvent::PadGone { index: 0 },
                    InputEvent::Alive,
                    InputEvent::ReleaseAll,
                    InputEvent::Mode { mode: PointerMode::Desktop },
                ],
            },
            TabToAgent::Bye,
        ]
    }

    fn tab_kind(m: &TabToAgent) -> &'static str {
        match m {
            TabToAgent::Hello { .. } => "Hello",
            TabToAgent::Resume { .. } => "Resume",
            TabToAgent::Screen { .. } => "Screen",
            TabToAgent::UseMonitor { .. } => "UseMonitor",
            TabToAgent::Control { .. } => "Control",
            TabToAgent::Input { .. } => "Input",
            TabToAgent::Bye => "Bye",
        }
    }

    fn agent_messages() -> Vec<AgentToTab> {
        let caps = AgentCaps { mouse_keyboard: true, mouse_keyboard_error: None, pads: 0, pads_error: Some("x".into()) };
        vec![
            AgentToTab::Challenge { v: 1, nonce: "n".into(), os: AgentOs::Linux, agent: "dchat-host 0.1.0".into() },
            AgentToTab::Paired { proof: "p".into(), token: "t".into(), caps: caps.clone() },
            AgentToTab::Resumed { proof: "p".into(), caps },
            AgentToTab::Refused { reason: RefuseReason::Locked { retry_secs: 30 } },
            AgentToTab::Refused { reason: RefuseReason::Version { agent_v: 2 } },
            AgentToTab::Monitors {
                list: vec![MonitorInfo { id: 0, name: "DP-1".into(), width: 2560, height: 1440, primary: true }],
                chosen: Some(0),
            },
            AgentToTab::Warning { code: "w".into() },
            AgentToTab::Stopped { reason: "hotkey".into() },
        ]
    }

    fn agent_kind(m: &AgentToTab) -> &'static str {
        match m {
            AgentToTab::Challenge { .. } => "Challenge",
            AgentToTab::Paired { .. } => "Paired",
            AgentToTab::Resumed { .. } => "Resumed",
            AgentToTab::Refused { .. } => "Refused",
            AgentToTab::Monitors { .. } => "Monitors",
            AgentToTab::Warning { .. } => "Warning",
            AgentToTab::Stopped { .. } => "Stopped",
        }
    }

    #[test]
    fn agent_wire_matches_agent_protocol_version() {
        let kinds: std::collections::HashSet<_> = tab_messages().iter().map(tab_kind).collect();
        assert_eq!(kinds.len(), 7);
        let kinds: std::collections::HashSet<_> = agent_messages().iter().map(agent_kind).collect();
        assert_eq!(kinds.len(), 7);
        let mut wire: Vec<String> = tab_messages().iter().map(|m| serde_json::to_string(m).unwrap()).collect();
        wire.extend(agent_messages().iter().map(|m| serde_json::to_string(m).unwrap()));
        wire.push(pair_proof("K7QM-4XPA", "a", "b"));
        wire.push(resume_proof("token", "a", "b"));
        let current = hex::encode(Sha256::digest(wire.join("\n").as_bytes()));
        assert_eq!(
            RECORDED.0, AGENT_PROTOCOL_VERSION,
            "AGENT_PROTOCOL_VERSION changed: record RECORDED = ({AGENT_PROTOCOL_VERSION}, \"{current}\")"
        );
        assert_eq!(
            RECORDED.1, current,
            "The tab/app wire format changed: bump AGENT_PROTOCOL_VERSION and set RECORDED = (<new version>, \"{current}\")"
        );
    }

    #[test]
    fn test_messages_round_trip() {
        for m in tab_messages() {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<TabToAgent>(&json).unwrap(), m);
        }
        for m in agent_messages() {
            let json = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<AgentToTab>(&json).unwrap(), m);
        }
        assert_eq!(serde_json::to_string(&TabToAgent::Bye).unwrap(), r#"{"t":"Bye"}"#);
    }

    #[test]
    fn test_codes_are_forgiving_to_type() {
        assert_eq!(normalize_code(" k7qm-4xpa "), "K7QM4XPA");
        assert_eq!(normalize_code("O1IL"), "0111");
        assert_eq!(format_code("k7qm4xpa"), "K7QM-4XPA");
        assert!(PAIRING_CODE_ALPHABET.iter().all(|c| !b"ILOU".contains(c)));
    }

    #[test]
    fn test_pairing_is_mutual_and_bound_to_both_nonces() {
        let proof = pair_proof("K7QM-4XPA", "agent-nonce", "tab-nonce");
        assert!(pair_proof_matches("k7qm4xpa", "agent-nonce", "tab-nonce", &proof), "typing variations still match");
        assert!(!pair_proof_matches("K7QM-4XPB", "agent-nonce", "tab-nonce", &proof), "wrong code");
        assert!(!pair_proof_matches("K7QM-4XPA", "other-nonce", "tab-nonce", &proof), "replayed to another challenge");
        assert!(!pair_proof_matches("K7QM-4XPA", "agent-nonce", "tab-nonce", "zz"), "not hex");
        // The app's answer is a different MAC: a captured tab proof can't be echoed back.
        let ack = pair_ack("K7QM-4XPA", "tab-nonce", "agent-nonce");
        assert_ne!(ack, proof);
        assert!(pair_ack_matches("K7QM-4XPA", "tab-nonce", "agent-nonce", &ack));
        assert!(!pair_ack_matches("K7QM-4XPA", "tab-nonce", "agent-nonce", &proof));
        let resume = resume_proof("tok", "n1", "n2");
        assert!(resume_proof_matches("tok", "n1", "n2", &resume));
        assert!(!resume_ack_matches("tok", "n2", "n1", &resume));
        assert!(resume_ack_matches("tok", "n2", "n1", &resume_ack("tok", "n2", "n1")));
    }
}
