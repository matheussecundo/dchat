//! Away and return. A member who drops out of reach without saying goodbye (no `Leave`, no
//! `PeerLeft`) is away for `AWAY_GRACE_MS`, then gone. A phone that switched apps comes back
//! within that time and finds its place unchanged: no "left" line, files, DMs and the admin
//! role wait for it. Each tab judges by its own clock, from when the member dropped out of
//! its reach. Pure Rust, no browser APIs.
//!
//! The grace is a rule every member applies alike (admin succession waits for it), so it is
//! part of the wire fingerprint.

use std::collections::{HashMap, HashSet};

/// How long a member who dropped out of reach stays away before counting as gone.
pub const AWAY_GRACE_MS: u64 = 5 * 60_000;

/// What changed since the last update.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Transitions {
    /// Dropped out of reach just now: away from now on.
    pub away: Vec<String>,
    /// Back within the grace.
    pub back: Vec<String>,
    /// Away for the whole grace: gone now.
    pub expired: Vec<String>,
    /// Back after counting as gone.
    pub returned: Vec<String>,
}

/// Members who dropped out of reach, as this tab saw it.
#[derive(Debug, Clone)]
pub struct Absences {
    grace_ms: u64,
    /// When each away member dropped out of reach (ms, our clock).
    lost_at: HashMap<String, u64>,
    /// Away for longer than the grace, and not heard leaving.
    gone: HashSet<String>,
}

impl Default for Absences {
    fn default() -> Self {
        Self::new(AWAY_GRACE_MS)
    }
}

impl Absences {
    pub fn new(grace_ms: u64) -> Self {
        Self { grace_ms, lost_at: HashMap::new(), gone: HashSet::new() }
    }

    /// Change the grace (tests shorten it); members already away keep their `lost_at`.
    pub fn set_grace(&mut self, grace_ms: u64) {
        self.grace_ms = grace_ms;
    }

    /// Who is in reach changed, or time passed. `previous` and `present` are the members in
    /// reach before and now (us included, never away).
    pub fn update(&mut self, previous: &HashSet<String>, present: &HashSet<String>, now: u64) -> Transitions {
        let mut changes = Transitions::default();
        for pubkey in present {
            if self.lost_at.remove(pubkey).is_some() {
                changes.back.push(pubkey.clone());
            } else if self.gone.remove(pubkey) {
                changes.returned.push(pubkey.clone());
            }
        }
        for pubkey in previous.difference(present) {
            if self.gone.contains(pubkey) || self.lost_at.contains_key(pubkey) {
                continue;
            }
            self.lost_at.insert(pubkey.clone(), now);
            changes.away.push(pubkey.clone());
        }
        let grace = self.grace_ms;
        let expired: Vec<String> =
            self.lost_at.iter().filter(|(_, at)| now >= **at + grace).map(|(pubkey, _)| pubkey.clone()).collect();
        for pubkey in expired {
            self.lost_at.remove(&pubkey);
            self.gone.insert(pubkey.clone());
            changes.expired.push(pubkey);
        }
        for list in [&mut changes.away, &mut changes.back, &mut changes.expired, &mut changes.returned] {
            list.sort();
        }
        changes
    }

    /// The member left for good (`Leave`, `PeerLeft`, kicked): forget them. Returns whether
    /// they were away (their "left" line hasn't been shown yet).
    pub fn forget(&mut self, pubkey: &str) -> bool {
        self.gone.remove(pubkey);
        self.lost_at.remove(pubkey).is_some()
    }

    pub fn is_away(&self, pubkey: &str) -> bool {
        self.lost_at.contains_key(pubkey)
    }

    /// Members away now.
    pub fn away(&self) -> HashSet<String> {
        self.lost_at.keys().cloned().collect()
    }

    /// When the earliest absence expires, if anyone is away.
    pub fn next_expiry(&self) -> Option<u64> {
        self.lost_at.values().map(|at| at + self.grace_ms).min()
    }

    /// When the last absence among `pubkeys` expires, if any of them is away.
    pub fn last_expiry_of<'a>(&self, pubkeys: impl IntoIterator<Item = &'a str>) -> Option<u64> {
        pubkeys.into_iter().filter_map(|p| self.lost_at.get(p)).map(|at| at + self.grace_ms).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(pubkeys: &[&str]) -> HashSet<String> {
        pubkeys.iter().map(|p| p.to_string()).collect()
    }

    fn list(pubkeys: &[&str]) -> Vec<String> {
        pubkeys.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn test_dropping_out_of_reach_is_away_then_gone() {
        let mut absences = Absences::new(1000);
        let changes = absences.update(&set(&["me", "a", "b"]), &set(&["me", "b"]), 100);
        assert_eq!(changes.away, list(&["a"]));
        assert!(absences.is_away("a"));
        assert_eq!(absences.away(), set(&["a"]));
        assert_eq!(absences.next_expiry(), Some(1100));
        absences.update(&set(&["me", "b"]), &set(&["me"]), 300);
        assert_eq!(absences.last_expiry_of(["a", "b", "c"]), Some(1300));
        assert_eq!(absences.last_expiry_of(["c"]), None);
        absences.update(&set(&["me"]), &set(&["me", "b"]), 301);

        // Nothing new just before the grace ends; gone once it has.
        let changes = absences.update(&set(&["me", "b"]), &set(&["me", "b"]), 1099);
        assert_eq!(changes, Transitions::default());
        let changes = absences.update(&set(&["me", "b"]), &set(&["me", "b"]), 1100);
        assert_eq!(changes.expired, list(&["a"]));
        assert!(!absences.is_away("a"));
        assert_eq!(absences.next_expiry(), None);
    }

    #[test]
    fn test_back_within_the_grace_keeps_nothing_and_says_nothing_else() {
        let mut absences = Absences::new(1000);
        absences.update(&set(&["me", "a"]), &set(&["me"]), 100);
        let changes = absences.update(&set(&["me"]), &set(&["me", "a"]), 600);
        assert_eq!(changes.back, list(&["a"]));
        assert!(changes.away.is_empty() && changes.expired.is_empty() && changes.returned.is_empty());
        assert!(!absences.is_away("a"));
        // A later absence starts its own grace.
        absences.update(&set(&["me", "a"]), &set(&["me"]), 2000);
        assert_eq!(absences.next_expiry(), Some(3000));
    }

    #[test]
    fn test_back_after_the_grace_is_a_return() {
        let mut absences = Absences::new(1000);
        absences.update(&set(&["me", "a"]), &set(&["me"]), 100);
        absences.update(&set(&["me"]), &set(&["me"]), 1200);
        let changes = absences.update(&set(&["me"]), &set(&["me", "a"]), 5000);
        assert_eq!(changes.returned, list(&["a"]));
        assert!(changes.back.is_empty());
        // Dropping out again is a fresh absence.
        let changes = absences.update(&set(&["me", "a"]), &set(&["me"]), 6000);
        assert_eq!(changes.away, list(&["a"]));
    }

    #[test]
    fn test_the_absence_counts_from_the_first_drop() {
        let mut absences = Absences::new(1000);
        absences.update(&set(&["me", "a"]), &set(&["me"]), 100);
        // Reported again (another recompute with a stale previous set): the clock stays.
        let changes = absences.update(&set(&["me", "a"]), &set(&["me"]), 500);
        assert!(changes.away.is_empty());
        assert_eq!(absences.next_expiry(), Some(1100));
    }

    #[test]
    fn test_leaving_for_good_is_forgotten() {
        let mut absences = Absences::new(1000);
        absences.update(&set(&["me", "a", "b"]), &set(&["me"]), 100);
        assert!(absences.forget("a"));
        assert!(!absences.is_away("a"));
        assert!(!absences.forget("a"));
        absences.update(&set(&["me"]), &set(&["me"]), 2000);
        // A gone member who then leaves explicitly was not away.
        assert!(!absences.forget("b"));
        // Forgotten members come back as newcomers, not returns.
        let changes = absences.update(&set(&["me"]), &set(&["me", "a", "b"]), 3000);
        assert_eq!(changes, Transitions::default());
    }

    #[test]
    fn test_shorter_grace_applies_to_members_already_away() {
        let mut absences = Absences::new(AWAY_GRACE_MS);
        absences.update(&set(&["me", "a"]), &set(&["me"]), 100);
        absences.set_grace(50);
        let changes = absences.update(&set(&["me"]), &set(&["me"]), 150);
        assert_eq!(changes.expired, list(&["a"]));
    }
}
