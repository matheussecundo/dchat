//! Relay policy: what dchat-relay accepts. Pure and unit-tested.

use protocol::{verify_event, NostrEvent, KIND_EPHEMERAL_SIGNAL};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Event kinds accepted; dchat only publishes ephemeral signaling (kind 20001).
    pub allowed_kinds: Vec<u64>,
    /// Largest WebSocket message accepted (SDP offers are the largest dchat events).
    pub max_message_bytes: usize,
    pub max_subscriptions: usize,
    pub max_filters_per_req: usize,
    /// Open connections allowed per client IP (mobile carriers put many users behind one).
    pub max_connections_per_ip: usize,
    /// Accept events whose `created_at` is at most this old / this far ahead (seconds).
    pub max_event_age_secs: u64,
    pub max_future_secs: u64,
    /// Per-connection token bucket: a member joining a 25-person room sends ~100 events at once.
    pub event_burst: u32,
    pub events_per_sec: u32,
    /// Browser origins allowed to connect (e.g. `https://chat.example.com`); empty = any.
    pub allowed_origins: Vec<String>,
    /// Take the client IP from `X-Forwarded-For` (only behind a reverse proxy you control).
    pub trust_proxy: bool,
    /// Name in the NIP-11 relay information document.
    pub name: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            allowed_kinds: vec![KIND_EPHEMERAL_SIGNAL],
            max_message_bytes: 128 * 1024,
            max_subscriptions: 8,
            max_filters_per_req: 4,
            max_connections_per_ip: 64,
            max_event_age_secs: 300,
            max_future_secs: 60,
            event_burst: 300,
            events_per_sec: 30,
            allowed_origins: Vec::new(),
            trust_proxy: false,
            name: "dchat-relay".into(),
        }
    }
}

impl RelayConfig {
    pub fn origin_allowed(&self, origin: Option<&str>) -> bool {
        self.allowed_origins.is_empty()
            || origin.is_some_and(|o| self.allowed_origins.iter().any(|a| a.trim_end_matches('/') == o))
    }
}

/// Why an event was refused; `reason()` follows the NIP-01 machine-readable prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Kind,
    NotEphemeral,
    NoTopic,
    Stale,
    Future,
    BadSignature,
    RateLimited,
}

impl Rejection {
    pub fn reason(&self) -> &'static str {
        match self {
            Rejection::Kind => "blocked: this relay only carries dchat signaling",
            Rejection::NotEphemeral => "blocked: only ephemeral events are relayed",
            Rejection::NoTopic => "invalid: missing d tag",
            Rejection::Stale => "invalid: created_at is too old",
            Rejection::Future => "invalid: created_at is in the future",
            Rejection::BadSignature => "invalid: bad signature or id",
            Rejection::RateLimited => "rate-limited: slow down",
        }
    }
}

/// Validate an event against the relay policy (cheap checks first, signature last).
pub fn check_event(event: &NostrEvent, now_secs: u64, cfg: &RelayConfig) -> Result<(), Rejection> {
    if !cfg.allowed_kinds.contains(&event.kind) {
        return Err(Rejection::Kind);
    }
    // Ephemeral kinds are never stored: this relay keeps nothing on disk or in a backlog.
    if !(20000..30000).contains(&event.kind) {
        return Err(Rejection::NotEphemeral);
    }
    if !event.tags.iter().any(|t| t.len() >= 2 && t[0] == "d" && !t[1].is_empty()) {
        return Err(Rejection::NoTopic);
    }
    if event.created_at + cfg.max_event_age_secs < now_secs {
        return Err(Rejection::Stale);
    }
    if event.created_at > now_secs + cfg.max_future_secs {
        return Err(Rejection::Future);
    }
    match verify_event(event) {
        Ok(true) => Ok(()),
        _ => Err(Rejection::BadSignature),
    }
}

/// Token bucket for per-connection event rates.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_per_sec: u32, now: Instant) -> Self {
        Self {
            capacity: capacity as f64,
            tokens: capacity as f64,
            refill_per_sec: refill_per_sec as f64,
            last: now,
        }
    }

    pub fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
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
    use protocol::NostrBurnerKey;
    use std::time::Duration;

    const NOW: u64 = 1_800_000_000;

    fn event(kind: u64, created_at: u64, tags: Vec<Vec<String>>) -> NostrEvent {
        NostrBurnerKey::generate().unwrap().create_event(kind, tags, "x".into(), created_at).unwrap()
    }

    fn topic() -> Vec<Vec<String>> {
        vec![vec!["d".into(), "abc".into()]]
    }

    #[test]
    fn test_accepts_fresh_signed_dchat_event() {
        assert_eq!(check_event(&event(20001, NOW, topic()), NOW, &RelayConfig::default()), Ok(()));
    }

    #[test]
    fn test_rejects_other_kinds_and_non_ephemeral() {
        let cfg = RelayConfig::default();
        assert_eq!(check_event(&event(1, NOW, topic()), NOW, &cfg), Err(Rejection::Kind));
        let open = RelayConfig { allowed_kinds: vec![1, 20002], ..cfg };
        assert_eq!(check_event(&event(1, NOW, topic()), NOW, &open), Err(Rejection::NotEphemeral));
        assert_eq!(check_event(&event(20002, NOW, topic()), NOW, &open), Ok(()));
    }

    #[test]
    fn test_requires_topic_and_fresh_timestamp() {
        let cfg = RelayConfig::default();
        assert_eq!(check_event(&event(20001, NOW, vec![]), NOW, &cfg), Err(Rejection::NoTopic));
        assert_eq!(check_event(&event(20001, NOW - 301, topic()), NOW, &cfg), Err(Rejection::Stale));
        assert_eq!(check_event(&event(20001, NOW - 299, topic()), NOW, &cfg), Ok(()));
        assert_eq!(check_event(&event(20001, NOW + 61, topic()), NOW, &cfg), Err(Rejection::Future));
    }

    #[test]
    fn test_rejects_tampered_events() {
        let mut e = event(20001, NOW, topic());
        e.content = "forged".into();
        assert_eq!(check_event(&e, NOW, &RelayConfig::default()), Err(Rejection::BadSignature));
        let mut e = event(20001, NOW, topic());
        e.sig = "00".into();
        assert_eq!(check_event(&e, NOW, &RelayConfig::default()), Err(Rejection::BadSignature));
    }

    #[test]
    fn test_origin_allow_list() {
        let any = RelayConfig::default();
        assert!(any.origin_allowed(None));
        assert!(any.origin_allowed(Some("https://evil.example")));
        let pinned = RelayConfig { allowed_origins: vec!["https://chat.example.com/".into()], ..any };
        assert!(pinned.origin_allowed(Some("https://chat.example.com")));
        assert!(!pinned.origin_allowed(Some("https://evil.example")));
        assert!(!pinned.origin_allowed(None));
    }

    #[test]
    fn test_token_bucket_burst_then_refill() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::new(3, 2, t0);
        assert!(bucket.try_take(t0) && bucket.try_take(t0) && bucket.try_take(t0));
        assert!(!bucket.try_take(t0));
        assert!(bucket.try_take(t0 + Duration::from_millis(500)), "one token refilled after 0.5 s");
        assert!(!bucket.try_take(t0 + Duration::from_millis(500)));
        // Never exceeds capacity after a long pause.
        let later = t0 + Duration::from_secs(60);
        assert!((0..3).all(|_| bucket.try_take(later)));
        assert!(!bucket.try_take(later));
    }
}
