//! Handshakes routed over the mesh (`RoomBody::RoutedSignal`): how much each member may have
//! forwarded. Pure Rust, no browser APIs (times in ms, passed in, so it runs in wasm).

/// Routed handshakes a forwarder passes on for one author in a burst, then per second.
pub const ROUTE_BURST: f64 = 200.0;
pub const ROUTE_PER_SECOND: f64 = 20.0;

/// A token bucket per author: a member joining a full room sends ~100 handshake messages at
/// once, a misbehaving one can't make forwarders flood the mesh for long.
#[derive(Debug, Default)]
pub struct RouteBudget {
    buckets: std::collections::HashMap<String, (f64, u64)>,
}

impl RouteBudget {
    /// Whether one more routed message from `author` may be forwarded at `now` (ms).
    pub fn allow(&mut self, author: &str, now: u64) -> bool {
        let (tokens, at) = self.buckets.entry(author.to_string()).or_insert((ROUTE_BURST, now));
        let elapsed = now.saturating_sub(*at) as f64 / 1000.0;
        *tokens = (*tokens + elapsed * ROUTE_PER_SECOND).min(ROUTE_BURST);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Forget members who left.
    pub fn forget(&mut self, author: &str) {
        self.buckets.remove(author);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_burst_then_steady_rate() {
        let mut budget = RouteBudget::default();
        for _ in 0..ROUTE_BURST as usize {
            assert!(budget.allow("a", 0));
        }
        assert!(!budget.allow("a", 0), "burst spent");
        assert!(budget.allow("b", 0), "per author");
        assert!(!budget.allow("a", 40), "less than a token back");
        assert!(budget.allow("a", 50), "one token per 50 ms");
        assert!(!budget.allow("a", 50));
        budget.forget("a");
        assert!(budget.allow("a", 50));
    }
}
