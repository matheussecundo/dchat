//! Who may talk to dchat-host and how much. Pure and unit-tested.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// The port in the Host header must match (blocks DNS-rebinding attacks).
    pub port: u16,
    /// Browser origins of dchat sites allowed to connect. Never empty.
    pub allowed_origins: Vec<String>,
    /// Sockets that have not paired yet (any website could open one).
    pub max_unauthenticated: usize,
    pub pre_auth_timeout: Duration,
    pub ping_interval: Duration,
    /// No frame or pong for this long: the tab is gone; release everything.
    pub idle_timeout: Duration,
    pub max_frame_bytes: usize,
    pub max_events_per_frame: usize,
    pub messages_per_sec: f64,
    pub message_burst: f64,
    /// Answering a wrong code slowly makes guessing slower still.
    pub bad_code_delay: Duration,
    /// Serve `/__test/*` (only with the mock injector).
    pub test_api: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            port: protocol::DEFAULT_AGENT_PORT,
            allowed_origins: Vec::new(),
            max_unauthenticated: 4,
            pre_auth_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(15),
            max_frame_bytes: 16 * 1024,
            max_events_per_frame: 64,
            messages_per_sec: 500.0,
            message_burst: 1000.0,
            bad_code_delay: Duration::from_secs(1),
            test_api: false,
        }
    }
}

/// Origins baked in at build time (`DCHAT_HOST_ORIGINS=https://a,https://b`).
pub fn built_in_origins() -> Vec<String> {
    normalize_origins(option_env!("DCHAT_HOST_ORIGINS").unwrap_or("").split(','))
}

pub fn normalize_origins<'a>(origins: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for origin in origins {
        let origin = origin.trim().trim_end_matches('/');
        if !origin.is_empty() && !out.iter().any(|o| o == origin) {
            out.push(origin.to_string());
        }
    }
    out
}

impl AgentConfig {
    /// Exact match against the allow-list; a missing or `null` Origin never passes.
    pub fn origin_allowed(&self, origin: Option<&str>) -> bool {
        origin
            .map(|o| o.trim_end_matches('/'))
            .filter(|o| !o.is_empty() && *o != "null")
            .is_some_and(|o| self.allowed_origins.iter().any(|a| a == o))
    }

    pub fn host_allowed(&self, host: Option<&str>) -> bool {
        host.is_some_and(|h| h == format!("127.0.0.1:{}", self.port) || h == format!("localhost:{}", self.port))
    }
}

/// Simple token bucket.
#[derive(Debug)]
pub struct TokenBucket {
    tokens: f64,
    burst: f64,
    per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(burst: f64, per_sec: f64, now: Instant) -> Self {
        Self { tokens: burst, burst, per_sec, last: now }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_sec).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AgentConfig {
        AgentConfig { port: 7448, allowed_origins: normalize_origins(["https://chat.example.com/", " "]), ..Default::default() }
    }

    #[test]
    fn test_origin_allow_list() {
        let cfg = cfg();
        assert_eq!(cfg.allowed_origins, vec!["https://chat.example.com"]);
        assert!(cfg.origin_allowed(Some("https://chat.example.com")));
        assert!(!cfg.origin_allowed(Some("https://chat.example.com.evil.io")));
        assert!(!cfg.origin_allowed(Some("http://chat.example.com")));
        assert!(!cfg.origin_allowed(Some("null")));
        assert!(!cfg.origin_allowed(None));
        assert!(!AgentConfig::default().origin_allowed(Some("https://chat.example.com")), "an empty list allows nobody");
    }

    #[test]
    fn test_host_header_must_be_loopback_on_our_port() {
        let cfg = cfg();
        assert!(cfg.host_allowed(Some("127.0.0.1:7448")));
        assert!(cfg.host_allowed(Some("localhost:7448")));
        assert!(!cfg.host_allowed(Some("127.0.0.1:7449")));
        assert!(!cfg.host_allowed(Some("evil.example:7448")), "DNS rebinding");
        assert!(!cfg.host_allowed(Some("127.0.0.1.nip.io:7448")));
        assert!(!cfg.host_allowed(None));
    }

    #[test]
    fn test_token_bucket() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(2.0, 1.0, now);
        assert!(bucket.try_take(now));
        assert!(bucket.try_take(now));
        assert!(!bucket.try_take(now));
        assert!(bucket.try_take(now + Duration::from_secs(1)));
    }
}
