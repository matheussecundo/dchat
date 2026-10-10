//! Admin succession: when the room's last admin is gone, the member who has been there longest
//! takes over. Pure Rust, no browser APIs.
//!
//! - Seniority is the admin's own view: when its tab first saw each member (its clock, so
//!   nobody can claim to be older than they are), ahead of which come the members a previous
//!   admin ranked (`FirstSeen::inherit`).
//! - While admins are present, one of them (the lowest session pubkey, the maintainer) hands
//!   the admin secret, sealed, to the heir: the most senior present member who isn't an admin.
//!   It hands it again whenever the heir changes; an older heir drops its copy on seeing the
//!   newer grant (`on_heir_grant`).
//! - The heir keeps the secret dormant, in RAM only. Once no admin has been present for
//!   `ADMIN_ABSENT_MS` while someone else is, it takes over (`due`): it writes the secret into
//!   its link and signs an admin proof like any admin, then picks its own heir.
//! - Members away (out of reach within `protocol::presence::AWAY_GRACE_MS`) still count: an
//!   away admin holds off the takeover and an away heir stays the heir, so a phone that
//!   switched apps neither loses the room's admin nor spreads the secret. Only a reachable
//!   admin hands the secret over, and only to a reachable heir (a handover can't reach an
//!   away one: it waits until they are back).
//!
//! Enforced by honest clients: whoever holds the secret could use it any time.

use crate::room::Member;
use std::collections::HashMap;
use std::fmt;

/// How long a room goes without an admin before the heir takes over. Long enough for an
/// admin's reload (its new session comes back with a fresh proof and the count resets).
pub const ADMIN_ABSENT_MS: u64 = 15_000;

/// Seniority as this tab sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirstSeen {
    /// When this tab first saw each member (ms, its own clock).
    at: HashMap<String, u64>,
    /// A previous admin's order, most senior first: ahead of everyone only we have seen.
    inherited: Vec<String>,
}

impl FirstSeen {
    /// Note `pubkey` as present now (only its first sighting counts).
    pub fn saw(&mut self, pubkey: &str, now: u64) {
        self.at.entry(pubkey.to_string()).or_insert(now);
    }

    /// Take a previous admin's order (from the handover), replacing any earlier one.
    pub fn inherit(&mut self, ranking: Vec<String>) {
        self.inherited = ranking;
    }

    /// Sort key: inherited rank first, then our own first sighting, then the pubkey.
    fn key<'a>(&self, pubkey: &'a str) -> (usize, u64, &'a str) {
        let inherited = self.inherited.iter().position(|p| p == pubkey).unwrap_or(usize::MAX);
        (inherited, self.at.get(pubkey).copied().unwrap_or(u64::MAX), pubkey)
    }

    /// `pubkeys`, most senior first.
    pub fn order<'a>(&self, pubkeys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut sorted: Vec<&str> = pubkeys.into_iter().collect();
        sorted.sort_by_key(|p| self.key(p));
        sorted.dedup();
        sorted.into_iter().map(str::to_string).collect()
    }
}

/// The present admin that hands the secret to the heir: the lowest session pubkey.
pub fn maintainer<'a>(present: &[&'a Member]) -> Option<&'a str> {
    present.iter().filter(|m| m.is_admin).map(|m| m.pubkey.as_str()).min()
}

/// The most senior present member other than `me` who isn't an admin.
pub fn pick_heir(seen: &FirstSeen, present: &[&Member], me: &str) -> Option<String> {
    let candidates = present.iter().filter(|m| !m.is_admin && m.pubkey != me).map(|m| m.pubkey.as_str());
    seen.order(candidates).into_iter().next()
}

/// What a heir grant meant to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantOutcome {
    /// We are the heir now.
    Kept,
    /// Someone else is: our dormant copy is gone.
    Dropped,
    /// Someone else is; we held nothing.
    Ignored,
    /// Older than a grant already seen.
    Stale,
}

/// This tab's part in succession.
#[derive(Clone, Default)]
pub struct Succession {
    pub seen: FirstSeen,
    /// The heir we last handed the secret to (as maintainer).
    handed_to: Option<String>,
    /// No handover before this (ms): members are still coming back after a rekey.
    hold_until: u64,
    /// Since when no admin is present while another member is.
    no_admin_since: Option<u64>,
    /// The admin secret (hex), while we are the heir.
    dormant: Option<String>,
    /// The latest heir grant seen, as (ts, envelope id).
    last_grant: Option<(u64, String)>,
}

impl fmt::Debug for Succession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Succession")
            .field("seen", &self.seen)
            .field("handed_to", &self.handed_to)
            .field("hold_until", &self.hold_until)
            .field("no_admin_since", &self.no_admin_since)
            .field("dormant", &self.dormant.as_ref().map(|_| "<secret>"))
            .field("last_grant", &self.last_grant)
            .finish()
    }
}

impl Succession {
    pub fn new(seen: FirstSeen, hold_until: u64) -> Self {
        Self { seen, hold_until, ..Self::default() }
    }

    /// The room changed (or time passed). `present`: the members in reach, us included;
    /// `away`: members out of reach but within their grace. Returns the member to hand the
    /// secret to now, when we are the maintainer and the heir changed.
    pub fn update(&mut self, me: &str, present: &[&Member], away: &[&Member], now: u64) -> Option<String> {
        let admin_around = present.iter().chain(away).any(|m| m.is_admin);
        // Alone, maybe it is our own network that dropped: away members are no company.
        let company = present.iter().any(|m| m.pubkey != me);
        if admin_around || !company {
            self.no_admin_since = None;
        } else if self.no_admin_since.is_none() {
            self.no_admin_since = Some(now);
        }
        let i_am_admin = present.iter().any(|m| m.pubkey == me && m.is_admin);
        if i_am_admin {
            self.dormant = None;
        }
        if !i_am_admin || maintainer(present) != Some(me) {
            self.handed_to = None;
            return None;
        }
        if now < self.hold_until {
            return None;
        }
        let candidates: Vec<&Member> = present.iter().chain(away).copied().collect();
        let heir = pick_heir(&self.seen, &candidates, me)?;
        if self.handed_to.as_deref() == Some(heir.as_str()) {
            return None;
        }
        // A handover sent now would not reach an away heir: it gets it once back.
        if away.iter().any(|m| m.pubkey == heir) {
            return None;
        }
        self.handed_to = Some(heir.clone());
        Some(heir)
    }

    /// The handover to `handed_to` could not be sent: try again next time.
    pub fn forget_handover(&mut self) {
        self.handed_to = None;
    }

    /// A heir grant (`ts`, `id`) from an admin; `mine` is the secret when it was sealed to us.
    pub fn on_heir_grant(&mut self, ts: u64, id: &str, mine: Option<String>) -> GrantOutcome {
        let grant = (ts, id.to_string());
        if self.last_grant.as_ref().is_some_and(|last| *last >= grant) {
            return GrantOutcome::Stale;
        }
        self.last_grant = Some(grant);
        match mine {
            Some(secret) => {
                self.dormant = Some(secret);
                GrantOutcome::Kept
            }
            None if self.dormant.take().is_some() => GrantOutcome::Dropped,
            None => GrantOutcome::Ignored,
        }
    }

    /// Whether we hold the secret and the room has gone without an admin long enough.
    pub fn due(&self, now: u64) -> bool {
        self.dormant.is_some() && self.no_admin_since.is_some_and(|since| now >= since + ADMIN_ABSENT_MS)
    }

    pub fn has_dormant(&self) -> bool {
        self.dormant.is_some()
    }

    pub fn take_dormant(&mut self) -> Option<String> {
        self.dormant.take()
    }

    pub fn clear_dormant(&mut self) {
        self.dormant = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(pubkey: &str, is_admin: bool) -> Member {
        Member { pubkey: pubkey.into(), name: pubkey.to_uppercase(), join_ts: 0, is_admin }
    }

    fn seen(order: &[&str]) -> FirstSeen {
        let mut seen = FirstSeen::default();
        for (i, p) in order.iter().enumerate() {
            seen.saw(p, 100 + i as u64);
        }
        seen
    }

    #[test]
    fn test_heir_is_the_most_senior_present_non_admin() {
        let (ana, bo, cy, dee) = (member("ana", true), member("bo", false), member("cy", false), member("dee", false));
        let order = seen(&["ana", "cy", "bo", "dee"]);
        assert_eq!(pick_heir(&order, &[&ana, &bo, &cy, &dee], "ana").as_deref(), Some("cy"));
        assert_eq!(pick_heir(&order, &[&ana, &bo, &dee], "ana").as_deref(), Some("bo"), "absent members don't count");
        assert_eq!(pick_heir(&order, &[&ana], "ana"), None);
        // Seen again later: only the first sighting counts.
        let mut again = order.clone();
        again.saw("cy", 999);
        assert_eq!(pick_heir(&again, &[&ana, &bo, &cy], "ana").as_deref(), Some("cy"));
        // Never seen: last, by pubkey.
        let (x, y) = (member("x", false), member("y", false));
        assert_eq!(pick_heir(&FirstSeen::default(), &[&ana, &y, &x], "ana").as_deref(), Some("x"));
        // Admins and me are never the heir.
        let co_admin = member("bo", true);
        assert_eq!(pick_heir(&order, &[&ana, &co_admin, &dee], "ana").as_deref(), Some("dee"));
    }

    #[test]
    fn test_inherited_order_comes_first() {
        let mut order = seen(&["bo", "cy", "dee"]);
        order.inherit(vec!["dee".into(), "cy".into()]);
        assert_eq!(order.order(["bo", "cy", "dee"]), vec!["dee", "cy", "bo"]);
    }

    #[test]
    fn test_only_the_lowest_admin_hands_over_once_per_heir() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        assert_eq!(s.update("ana", &[&ana, &bo, &cy], &[], 1000).as_deref(), Some("bo"));
        assert_eq!(s.update("ana", &[&ana, &bo, &cy], &[], 1001), None, "already handed");
        assert_eq!(s.update("ana", &[&ana, &cy], &[], 1002).as_deref(), Some("cy"), "the heir left");
        // Bo becomes an admin too (made admin): the heir stays Cy; Ana still maintains.
        let bo_admin = member("bo", true);
        assert_eq!(s.update("ana", &[&ana, &bo_admin, &cy], &[], 1003), None);
        // From Bo's side: not the maintainer (ana < bo), so nothing to hand.
        let mut b = Succession::new(seen(&["ana", "cy"]), 0);
        assert_eq!(b.update("bo", &[&ana, &bo_admin, &cy], &[], 1003), None);
        // A non-admin never hands over.
        let mut c = Succession::new(seen(&["ana", "bo"]), 0);
        assert_eq!(c.update("cy", &[&ana, &bo, &cy], &[], 1003), None);
    }

    #[test]
    fn test_no_handover_while_holding() {
        let (ana, bo) = (member("ana", true), member("bo", false));
        let mut s = Succession::new(seen(&["ana", "bo"]), 5000);
        assert_eq!(s.update("ana", &[&ana, &bo], &[], 4999), None);
        assert_eq!(s.update("ana", &[&ana, &bo], &[], 5000).as_deref(), Some("bo"));
    }

    #[test]
    fn test_maintainer_change_hands_over_again() {
        let (ana, bo, cy) = (member("ana", true), member("bo", true), member("cy", false));
        let mut b = Succession::new(seen(&["ana", "cy"]), 0);
        assert_eq!(b.update("bo", &[&ana, &bo, &cy], &[], 1), None, "ana maintains");
        assert_eq!(b.update("bo", &[&bo, &cy], &[], 2).as_deref(), Some("cy"), "ana left: bo maintains");
    }

    #[test]
    fn test_heir_takes_over_after_fifteen_seconds_without_an_admin() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        s.update("bo", &[&ana, &bo, &cy], &[], 0);
        assert_eq!(s.on_heir_grant(10, "g1", Some("secret".into())), GrantOutcome::Kept);
        s.update("bo", &[&bo, &cy], &[], 1000);
        assert!(!s.due(1000 + ADMIN_ABSENT_MS - 1));
        assert!(s.due(1000 + ADMIN_ABSENT_MS));

        // The admin comes back (a reload): the count starts over.
        s.update("bo", &[&ana, &bo, &cy], &[], 2000);
        assert!(!s.due(100_000));
        s.update("bo", &[&bo, &cy], &[], 50_000);
        assert!(!s.due(50_000 + ADMIN_ABSENT_MS - 1));
        assert!(s.due(50_000 + ADMIN_ABSENT_MS));
        assert_eq!(s.take_dormant().as_deref(), Some("secret"));
        assert!(!s.due(1_000_000));
    }

    #[test]
    fn test_no_countdown_while_alone() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo"]), 0);
        s.update("bo", &[&ana, &bo], &[], 0);
        s.on_heir_grant(1, "g", Some("secret".into()));
        s.update("bo", &[&bo], &[], 1000);
        assert!(!s.due(1_000_000), "maybe it is our own network that dropped");
        s.update("bo", &[&bo, &cy], &[], 2_000_000);
        assert!(!s.due(2_000_000 + ADMIN_ABSENT_MS - 1));
        assert!(s.due(2_000_000 + ADMIN_ABSENT_MS));
    }

    #[test]
    fn test_newer_grants_win_and_drop_older_copies() {
        let mut s = Succession::default();
        assert_eq!(s.on_heir_grant(10, "a", Some("secret".into())), GrantOutcome::Kept);
        assert_eq!(s.on_heir_grant(20, "b", None), GrantOutcome::Dropped);
        assert!(!s.has_dormant());
        // A delayed copy of the older grant does not bring the secret back.
        assert_eq!(s.on_heir_grant(10, "a", Some("secret".into())), GrantOutcome::Stale);
        assert!(!s.has_dormant());
        assert_eq!(s.on_heir_grant(20, "b", None), GrantOutcome::Stale);
        assert_eq!(s.on_heir_grant(30, "c", None), GrantOutcome::Ignored);
        // Becoming an admin drops a dormant copy.
        let (bo,) = (member("bo", true),);
        s.on_heir_grant(40, "d", Some("secret".into()));
        s.update("bo", &[&bo], &[], 50);
        assert!(!s.has_dormant());
    }

    #[test]
    fn test_an_away_admin_holds_off_the_takeover() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        s.update("bo", &[&ana, &bo, &cy], &[], 0);
        s.on_heir_grant(1, "g", Some("secret".into()));
        // Ana's phone switched apps: away, not gone. No countdown however long it lasts.
        s.update("bo", &[&bo, &cy], &[&ana], 1000);
        assert!(!s.due(1_000_000));
        // Her absence expired (she counts as left): the countdown starts there.
        s.update("bo", &[&bo, &cy], &[], 400_000);
        assert!(!s.due(400_000 + ADMIN_ABSENT_MS - 1));
        assert!(s.due(400_000 + ADMIN_ABSENT_MS));
    }

    #[test]
    fn test_away_members_are_no_company() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        s.update("bo", &[&ana, &bo, &cy], &[], 0);
        s.on_heir_grant(1, "g", Some("secret".into()));
        // Ana's absence expired, Cy is away: maybe it is our own network that dropped.
        s.update("bo", &[&bo], &[&cy], 1000);
        assert!(!s.due(1_000_000));
    }

    #[test]
    fn test_an_away_heir_stays_the_heir() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        assert_eq!(s.update("ana", &[&ana, &bo, &cy], &[], 1).as_deref(), Some("bo"));
        // Bo switched apps: still the heir, the secret goes to nobody else.
        assert_eq!(s.update("ana", &[&ana, &cy], &[&bo], 2), None);
        assert_eq!(s.update("ana", &[&ana, &bo, &cy], &[], 3), None, "back: nothing to hand again");
        // Bo's absence expired: Cy is the heir now.
        assert_eq!(s.update("ana", &[&ana, &cy], &[], 4).as_deref(), Some("cy"));
    }

    #[test]
    fn test_a_new_heir_who_is_away_gets_the_secret_once_back() {
        let (ana, bo, cy) = (member("ana", true), member("bo", false), member("cy", false));
        // Ana starts maintaining (e.g. after a rekey) while the most senior member, Bo, is away.
        let mut s = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        assert_eq!(s.update("ana", &[&ana, &cy], &[&bo], 1), None, "a handover would not reach Bo");
        assert_eq!(s.update("ana", &[&ana, &bo, &cy], &[], 2).as_deref(), Some("bo"));
    }

    #[test]
    fn test_only_a_reachable_admin_maintains() {
        let (ana, bo, cy) = (member("ana", true), member("bo", true), member("cy", false));
        // Ana (the lower pubkey) is away: Bo maintains meanwhile.
        let mut b = Succession::new(seen(&["ana", "bo", "cy"]), 0);
        assert_eq!(b.update("bo", &[&bo, &cy], &[&ana], 1).as_deref(), Some("cy"));
    }

    #[test]
    fn test_debug_never_shows_the_secret() {
        let mut s = Succession::default();
        s.on_heir_grant(1, "g", Some("deadbeefsecret".into()));
        assert!(!format!("{s:?}").contains("deadbeefsecret"));
    }
}
