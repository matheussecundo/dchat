//! When a member talks on the relays. Pure Rust, no browser APIs.
//!
//! Relays carry discovery and first handshakes only; once a member is linked, the mesh
//! carries the rest. So a member announces itself (`Presence`) often only while it has no
//! open link: joining, back from a pause with dead links, or cut off. Linked members on the
//! relays announce themselves rarely, as a safety net that lets two parts of a split room
//! find each other.

/// How often a member beacons while it has no open link, once its first `FAST_BEACON_FOR_MS`
/// (every tick) are over.
pub const UNLINKED_BEACON_MS: u64 = 15_000;
/// After joining or a pause: beacon on every tick for this long (while it matters).
pub const FAST_BEACON_FOR_MS: u64 = 20_000;
/// A linked member on the relays beacons about this often (±25 %, `beacon_due`'s `jitter`).
pub const SAFETY_BEACON_MS: u64 = 60_000;

/// Why a member is on the relays, for its beacon pace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeaconRole {
    /// No open link: it must be found.
    Unlinked,
    /// Linked, and staying on the relays to be found by newcomers.
    Linked,
}

/// Whether a member in `role` beacons at `now`, having last done so at `last` (0: never).
/// `fast_until`: the end of its fast window (join, pause); `jitter` in `[0, 1)`, drawn again
/// after each beacon, spreads safety beacons over ±25 %.
pub fn beacon_due(role: BeaconRole, now: u64, last: u64, fast_until: u64, jitter: f64) -> bool {
    if now < fast_until {
        return true;
    }
    let period = match role {
        BeaconRole::Unlinked => UNLINKED_BEACON_MS as f64,
        BeaconRole::Linked => SAFETY_BEACON_MS as f64 * (0.75 + 0.5 * jitter.clamp(0.0, 1.0)),
    };
    now.saturating_sub(last) as f64 >= period
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_window_beacons_every_time() {
        for role in [BeaconRole::Unlinked, BeaconRole::Linked] {
            assert!(beacon_due(role, 1000, 999, 5000, 0.5));
        }
    }

    #[test]
    fn test_unlinked_members_beacon_every_15_seconds() {
        assert!(!beacon_due(BeaconRole::Unlinked, 30_000, 20_000, 0, 0.0));
        assert!(beacon_due(BeaconRole::Unlinked, 35_000, 20_000, 0, 0.0));
    }

    #[test]
    fn test_linked_members_beacon_rarely_with_jitter() {
        assert!(!beacon_due(BeaconRole::Linked, 44_000, 0, 0, 0.0));
        assert!(beacon_due(BeaconRole::Linked, 45_000, 0, 0, 0.0));
        assert!(!beacon_due(BeaconRole::Linked, 74_000, 0, 0, 0.999));
        assert!(beacon_due(BeaconRole::Linked, 75_000, 0, 0, 0.999));
        // Never before the fast window ends: the window itself beaconed.
        assert!(!beacon_due(BeaconRole::Linked, 30_000, 20_000, 20_000, 0.5));
    }
}
