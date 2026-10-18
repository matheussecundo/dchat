//! Who stays on the relays, and when members talk there. Pure Rust, no browser APIs.
//!
//! Relays carry discovery and first handshakes only, and see the IP address of every member
//! connected to them. So only a few members stay: the room's two **introducers** (present
//! admins first, then the lowest pubkeys), which newcomers link to first; every other
//! handshake goes over the mesh (`RoomBody::RoutedSignal`). Everyone else joins the relays
//! only while it has to: no open link (joining, back from a pause with dead links, cut off),
//! a handshake with someone the mesh can't reach, or a `Knock` from an introducer that
//! couldn't link with a newcomer. Every member applies these rules alike.
//!
//! A member announces itself (`Presence`) often only while it has no open link. Linked
//! introducers announce themselves rarely, as a safety net that lets two parts of a split
//! room find each other; other linked members never do.

use crate::room::Member;
use std::collections::HashSet;

/// Members who stay on the relays for newcomers.
pub const INTRODUCERS: usize = 2;
/// A member that no longer needs the relays leaves them after this long (no flapping when
/// the reason changes, a handshake under way finishes).
pub const LEAVE_HOLD_MS: u64 = 10_000;
/// A `Knock` keeps the members it reaches on the relays this long, to link with a newcomer
/// the introducers couldn't link with.
pub const KNOCK_WINDOW_MS: u64 = 60_000;
/// An introducer knocks for the same member at most this often.
pub const KNOCK_AGAIN_MS: u64 = 60_000;

/// How often a member beacons while it has no open link, once its first `FAST_BEACON_FOR_MS`
/// (every tick) are over.
pub const UNLINKED_BEACON_MS: u64 = 15_000;
/// After joining, a pause, or becoming an introducer: beacon on every tick for this long.
pub const FAST_BEACON_FOR_MS: u64 = 20_000;
/// A linked introducer beacons about this often (±25 %, `beacon_due`'s `jitter`).
pub const SAFETY_BEACON_MS: u64 = 60_000;

/// The room's introducers as `present` (members reachable from us, and us) sees them:
/// present admins by lowest pubkey, then the other present members by lowest pubkey.
pub fn introducers<'a>(members: impl IntoIterator<Item = &'a Member>, present: &HashSet<String>) -> Vec<String> {
    let mut candidates: Vec<(bool, &str)> = members
        .into_iter()
        .filter(|m| present.contains(&m.pubkey))
        .map(|m| (!m.is_admin, m.pubkey.as_str()))
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates.into_iter().take(INTRODUCERS).map(|(_, pk)| pk.to_string()).collect()
}

/// Whether a member is on the relays, with a hold before it leaves them.
#[derive(Debug, Default, Clone)]
pub struct RelayRole {
    on: bool,
    /// Since when the member is on the relays without needing them.
    idle_since: Option<u64>,
}

impl RelayRole {
    /// Whether to be on the relays at `now`, given whether the member needs them (`wants`).
    pub fn update(&mut self, wants: bool, now: u64) -> bool {
        if wants {
            self.on = true;
            self.idle_since = None;
        } else if self.on {
            let since = *self.idle_since.get_or_insert(now);
            if now.saturating_sub(since) >= LEAVE_HOLD_MS {
                self.on = false;
                self.idle_since = None;
            }
        }
        self.on
    }
}

/// Why a member is on the relays, for its beacon pace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeaconRole {
    /// No open link: it must be found.
    Unlinked,
    /// Linked, and staying on the relays for newcomers.
    Introducer,
    /// Linked, on the relays for a handshake or a knock: newcomers' beacons find it.
    Visitor,
}

/// Whether a member in `role` beacons at `now`, having last done so at `last` (0: never).
/// `fast_until`: the end of its fast window (join, pause, becoming an introducer); `jitter`
/// in `[0, 1)`, drawn again after each beacon, spreads safety beacons over ±25 %.
pub fn beacon_due(role: BeaconRole, now: u64, last: u64, fast_until: u64, jitter: f64) -> bool {
    let period = match role {
        BeaconRole::Visitor => return false,
        _ if now < fast_until => return true,
        BeaconRole::Unlinked => UNLINKED_BEACON_MS as f64,
        BeaconRole::Introducer => SAFETY_BEACON_MS as f64 * (0.75 + 0.5 * jitter.clamp(0.0, 1.0)),
    };
    now.saturating_sub(last) as f64 >= period
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(pk: &str, is_admin: bool) -> Member {
        Member { pubkey: pk.into(), name: pk.into(), join_ts: 0, is_admin }
    }

    fn set(pks: &[&str]) -> HashSet<String> {
        pks.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn test_introducers_are_admins_then_lowest_pubkeys() {
        let members = [member("d", true), member("a", false), member("c", false), member("b", true)];
        assert_eq!(introducers(&members, &set(&["a", "b", "c", "d"])), ["b", "d"]);
        assert_eq!(introducers(&members, &set(&["a", "c", "d"])), ["d", "a"]);
        assert_eq!(introducers(&members, &set(&["a", "c"])), ["a", "c"]);
        assert_eq!(introducers(&members, &set(&["c"])), ["c"]);
        // Away, evicted or unknown members are not in `present`, so never introducers.
        assert_eq!(introducers(&members, &set(&["c", "z"])), ["c"]);
    }

    #[test]
    fn test_introducers_do_not_depend_on_order() {
        let mut members = vec![member("e", false), member("b", false), member("x", true), member("a", false)];
        let present = set(&["a", "b", "e", "x"]);
        let expected = introducers(&members, &present);
        members.reverse();
        assert_eq!(introducers(&members, &present), expected);
        assert_eq!(expected, ["x", "a"]);
    }

    #[test]
    fn test_each_side_of_a_split_room_has_its_own_introducers() {
        let members = [member("a", true), member("b", false), member("c", false), member("d", false)];
        assert_eq!(introducers(&members, &set(&["a", "b"])), ["a", "b"]);
        assert_eq!(introducers(&members, &set(&["c", "d"])), ["c", "d"]);
    }

    #[test]
    fn test_members_leave_the_relays_after_a_hold() {
        let mut role = RelayRole::default();
        assert!(!role.update(false, 0), "off until needed");
        assert!(role.update(true, 1000));
        assert!(role.update(false, 2000), "held");
        assert!(role.update(true, 5000), "needed again: the hold starts over");
        assert!(role.update(false, 6000));
        assert!(role.update(false, 6000 + LEAVE_HOLD_MS - 1));
        assert!(!role.update(false, 6000 + LEAVE_HOLD_MS));
        assert!(!role.update(false, 99_000));
    }

    #[test]
    fn test_fast_window_beacons_every_time() {
        for role in [BeaconRole::Unlinked, BeaconRole::Introducer] {
            assert!(beacon_due(role, 1000, 999, 5000, 0.5));
        }
        assert!(!beacon_due(BeaconRole::Visitor, 1000, 0, 5000, 0.5), "visitors never beacon");
    }

    #[test]
    fn test_unlinked_members_beacon_every_15_seconds() {
        assert!(!beacon_due(BeaconRole::Unlinked, 30_000, 20_000, 0, 0.0));
        assert!(beacon_due(BeaconRole::Unlinked, 35_000, 20_000, 0, 0.0));
    }

    #[test]
    fn test_linked_introducers_beacon_rarely_with_jitter() {
        assert!(!beacon_due(BeaconRole::Introducer, 44_000, 0, 0, 0.0));
        assert!(beacon_due(BeaconRole::Introducer, 45_000, 0, 0, 0.0));
        assert!(!beacon_due(BeaconRole::Introducer, 74_000, 0, 0, 0.999));
        assert!(beacon_due(BeaconRole::Introducer, 75_000, 0, 0, 0.999));
        // Never before the fast window ends: the window itself beaconed.
        assert!(!beacon_due(BeaconRole::Introducer, 30_000, 20_000, 20_000, 0.5));
    }
}
