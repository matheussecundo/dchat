//! Deterministic group-room logic shared by every member: roster, link graph,
//! gossip routing, cap eviction and room parameters. Pure Rust, no browser APIs.

use crate::fragment::FragmentParams;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

pub const DEFAULT_MEMBER_CAP: usize = 25;
pub const DEFAULT_VOICE_CAP: usize = 8;
pub const DEFAULT_VIDEO_CAP: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub pubkey: String,
    pub name: String,
    pub join_ts: u64,
    pub is_admin: bool,
}

/// Every member's announcement plus the direct links each member reports.
#[derive(Debug, Default)]
pub struct Roster {
    members: BTreeMap<String, Member>,
    link_states: HashMap<String, (u64, BTreeSet<String>)>,
}

impl Roster {
    /// Insert or update a member; returns whether anything changed.
    pub fn upsert(&mut self, member: Member) -> bool {
        if self.members.get(&member.pubkey) == Some(&member) {
            return false;
        }
        self.members.insert(member.pubkey.clone(), member);
        true
    }

    /// Forget a member and the links it reported; returns whether it was known.
    pub fn remove(&mut self, pubkey: &str) -> bool {
        let had_member = self.members.remove(pubkey).is_some();
        let had_links = self.link_states.remove(pubkey).is_some();
        had_member || had_links
    }

    pub fn get(&self, pubkey: &str) -> Option<&Member> {
        self.members.get(pubkey)
    }

    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }

    /// Record `author`'s direct links if `seq` is newer than what we hold.
    pub fn set_links(&mut self, author: &str, seq: u64, direct: impl IntoIterator<Item = String>) -> bool {
        if let Some((known_seq, _)) = self.link_states.get(author) {
            if *known_seq >= seq {
                return false;
            }
        }
        let direct: BTreeSet<String> = direct.into_iter().filter(|p| p != author).collect();
        self.link_states.insert(author.to_string(), (seq, direct));
        true
    }

    pub fn links_of(&self, pubkey: &str) -> Option<&BTreeSet<String>> {
        self.link_states.get(pubkey).map(|(_, links)| links)
    }

    /// Whether `a` and `b` hold a direct link. When both have reported, both must agree,
    /// so a member that vanished keeps no edge once its peers drop it; until then a
    /// one-sided report is trusted.
    pub fn linked(&self, a: &str, b: &str) -> bool {
        let ab = self.links_of(a).map(|l| l.contains(b));
        let ba = self.links_of(b).map(|l| l.contains(a));
        match (ab, ba) {
            (Some(x), Some(y)) => x && y,
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => false,
        }
    }

    fn neighbors(&self, pubkey: &str) -> Vec<String> {
        let mut candidates: BTreeSet<&str> = self.link_states.keys().map(String::as_str).collect();
        if let Some(links) = self.links_of(pubkey) {
            candidates.extend(links.iter().map(String::as_str));
        }
        candidates
            .into_iter()
            .filter(|other| *other != pubkey && self.linked(pubkey, other))
            .map(str::to_string)
            .collect()
    }

    /// Everyone reachable from `me` over direct links, excluding `me`.
    pub fn reachable_from(&self, me: &str) -> HashSet<String> {
        let mut seen: HashSet<String> = HashSet::from([me.to_string()]);
        let mut queue: VecDeque<String> = VecDeque::from([me.to_string()]);
        while let Some(current) = queue.pop_front() {
            for next in self.neighbors(&current) {
                if seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
        seen.remove(me);
        seen
    }

    /// One of `me`'s direct neighbors through which `target` is reached (for "via X").
    /// Prefers a neighbor linked to `target` directly, in pubkey order.
    pub fn relay_for(&self, me: &str, target: &str) -> Option<String> {
        let neighbors = self.neighbors(me);
        if let Some(direct) = neighbors.iter().find(|n| self.linked(n, target)) {
            return Some(direct.clone());
        }
        neighbors.into_iter().find(|n| {
            let mut sub = Roster::default();
            sub.link_states = self.link_states.clone();
            sub.link_states.remove(me);
            sub.reachable_from(n).contains(target)
        })
    }

    /// Direct neighbors of `me` that should receive a copy of a message written by
    /// `author` and received from `from`: those without their own direct link to the author.
    pub fn forward_targets(&self, me: &str, author: &str, from: &str) -> Vec<String> {
        self.neighbors(me)
            .into_iter()
            .filter(|n| n != from && n != author && !self.linked(n, author))
            .collect()
    }

    /// Members of `present` that must leave so at most `cap` remain (`None` = unlimited).
    /// Admins always stay and count toward the cap; the remaining seats go to non-admins
    /// in (join time, pubkey) order, so the latest joiner loses a race.
    pub fn evicted(&self, present: &HashSet<String>, cap: Option<usize>) -> Vec<String> {
        let present_members: Vec<&Member> = self
            .members
            .values()
            .filter(|m| present.contains(&m.pubkey))
            .collect();
        let admins = present_members.iter().filter(|m| m.is_admin).count();
        let others = present_members
            .iter()
            .filter(|m| !m.is_admin)
            .map(|m| (m.pubkey.clone(), m.join_ts));
        latest_beyond_cap(others, cap.map(|c| c.saturating_sub(admins)))
    }
}

/// Of `(pubkey, since)` entries, the ones beyond the first `cap` in (since, pubkey) order:
/// the deterministic "latest loses" rule shared by the member, voice and video caps.
pub fn latest_beyond_cap(entries: impl IntoIterator<Item = (String, u64)>, cap: Option<usize>) -> Vec<String> {
    let Some(cap) = cap else {
        return Vec::new();
    };
    let mut entries: Vec<(String, u64)> = entries.into_iter().collect();
    entries.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    entries.into_iter().skip(cap).map(|(pubkey, _)| pubkey).collect()
}

/// Bounded memory of message ids already seen, so gossip copies are processed once.
#[derive(Debug)]
pub struct GossipDedup {
    seen: HashSet<String>,
    order: VecDeque<String>,
    capacity: usize,
}

impl GossipDedup {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.seen.contains(id)
    }

    /// Record `id`; returns `true` the first time it is seen.
    pub fn insert(&mut self, id: &str) -> bool {
        if self.seen.contains(id) {
            return false;
        }
        if self.order.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(id.to_string());
        self.order.push_back(id.to_string());
        true
    }
}

/// At most this many uploads run at once per author; further requests wait in line.
pub const MAX_CONCURRENT_UPLOADS: usize = 2;

/// An upload of `file_id` to `peer`.
pub type Upload = (String, String);

/// What happened to a download request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueDecision {
    Start,
    /// 1-based place in line.
    Queued(usize),
    /// The same peer already asked for the same file.
    Duplicate,
}

/// The author's FIFO of uploads, running at most `max_active` at once.
#[derive(Debug)]
pub struct UploadQueue {
    max_active: usize,
    active: Vec<Upload>,
    waiting: VecDeque<Upload>,
}

impl UploadQueue {
    pub fn new(max_active: usize) -> Self {
        Self {
            max_active: max_active.max(1),
            active: Vec::new(),
            waiting: VecDeque::new(),
        }
    }

    pub fn request(&mut self, file_id: &str, peer: &str) -> QueueDecision {
        let upload = (file_id.to_string(), peer.to_string());
        if self.active.contains(&upload) || self.waiting.contains(&upload) {
            return QueueDecision::Duplicate;
        }
        if self.active.len() < self.max_active {
            self.active.push(upload);
            QueueDecision::Start
        } else {
            self.waiting.push_back(upload);
            QueueDecision::Queued(self.waiting.len())
        }
    }

    /// Remove every upload matching `pred` and promote waiting ones into freed slots.
    /// Returns the uploads that must start now.
    fn remove_where(&mut self, pred: impl Fn(&Upload) -> bool) -> Vec<Upload> {
        self.active.retain(|u| !pred(u));
        self.waiting.retain(|u| !pred(u));
        let mut started = Vec::new();
        while self.active.len() < self.max_active {
            let Some(next) = self.waiting.pop_front() else {
                break;
            };
            self.active.push(next.clone());
            started.push(next);
        }
        started
    }

    /// An upload finished or was cancelled.
    pub fn finish(&mut self, file_id: &str, peer: &str) -> Vec<Upload> {
        self.remove_where(|(f, p)| f == file_id && p == peer)
    }

    /// The peer left: drop all its uploads.
    pub fn remove_peer(&mut self, peer: &str) -> Vec<Upload> {
        self.remove_where(|(_, p)| p == peer)
    }

    /// The file was withdrawn: drop all its uploads.
    pub fn remove_file(&mut self, file_id: &str) -> Vec<Upload> {
        self.remove_where(|(f, _)| f == file_id)
    }

    pub fn is_active(&self, file_id: &str, peer: &str) -> bool {
        self.active.iter().any(|(f, p)| f == file_id && p == peer)
    }

    /// Waiting uploads with their 1-based positions, to tell each requester.
    pub fn positions(&self) -> Vec<(Upload, usize)> {
        self.waiting.iter().cloned().zip(1..).collect()
    }

    /// (active, waiting) uploads of one file, for the author's card.
    pub fn counts(&self, file_id: &str) -> (usize, usize) {
        let active = self.active.iter().filter(|(f, _)| f == file_id).count();
        let waiting = self.waiting.iter().filter(|(f, _)| f == file_id).count();
        (active, waiting)
    }
}

/// Parse a cap parameter: absent or invalid uses `default`; `0` / `unlimited` means no cap.
pub fn parse_cap(value: Option<&str>, default: usize) -> Option<usize> {
    match value.map(str::trim) {
        None => Some(default),
        Some("0") | Some("unlimited") | Some("inf") => None,
        Some(v) => Some(v.parse::<usize>().ok().filter(|n| *n > 0).unwrap_or(default)),
    }
}

/// Serialize a cap for the fragment (`0` = unlimited).
pub fn format_cap(cap: Option<usize>) -> String {
    cap.map(|c| c.to_string()).unwrap_or_else(|| "0".into())
}

/// Room settings carried in the URL fragment next to `room` and `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomParams {
    /// x-only pubkey whose signatures mark a session as admin (`adm`).
    pub admin_pubkey: Option<String>,
    /// Admin secret key, present only in the admin link (`admsk`).
    pub admin_secret: Option<String>,
    /// Member cap (`max`), `None` = unlimited.
    pub member_cap: Option<usize>,
    /// How many members may be in the voice lounge at once (`maxa`).
    pub voice_cap: Option<usize>,
    /// How many lounge members may send video (camera or screen) at once (`maxv`).
    pub video_cap: Option<usize>,
    /// TURN server URLs (`turn`, comma-separated) and credentials.
    pub turn_urls: Vec<String>,
    pub turn_user: Option<String>,
    pub turn_pass: Option<String>,
}

impl RoomParams {
    pub fn from_fragment(params: &FragmentParams) -> Self {
        let owned = |key: &str| params.get(key).map(str::to_string);
        Self {
            admin_pubkey: owned("adm"),
            admin_secret: owned("admsk"),
            member_cap: parse_cap(params.get("max"), DEFAULT_MEMBER_CAP),
            voice_cap: parse_cap(params.get("maxa"), DEFAULT_VOICE_CAP),
            video_cap: parse_cap(params.get("maxv"), DEFAULT_VIDEO_CAP),
            turn_urls: params
                .get("turn")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|u| !u.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            turn_user: owned("turnuser"),
            turn_pass: owned("turnpass"),
        }
    }
}

/// The shareable invite: the same fragment without the admin secret.
pub fn invite_fragment(params: &FragmentParams) -> FragmentParams {
    let mut invite = params.clone();
    invite.remove("admsk");
    invite
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(pk: &str, join_ts: u64, is_admin: bool) -> Member {
        Member {
            pubkey: pk.into(),
            name: pk.to_uppercase(),
            join_ts,
            is_admin,
        }
    }

    fn present(pks: &[&str]) -> HashSet<String> {
        pks.iter().map(|p| p.to_string()).collect()
    }

    fn links(pks: &[&str]) -> Vec<String> {
        pks.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn test_upsert_and_remove() {
        let mut roster = Roster::default();
        assert!(roster.upsert(member("a", 1, false)));
        assert!(!roster.upsert(member("a", 1, false)));
        assert!(roster.upsert(member("a", 1, true)));
        roster.set_links("a", 1, links(&["b"]));
        assert!(roster.remove("a"));
        assert!(roster.get("a").is_none());
        assert!(roster.links_of("a").is_none());
        assert!(!roster.remove("a"));
    }

    #[test]
    fn test_link_state_sequence_and_self_links() {
        let mut roster = Roster::default();
        assert!(roster.set_links("a", 2, links(&["b", "a"])));
        assert!(!roster.set_links("a", 1, links(&["c"])));
        assert!(!roster.set_links("a", 2, links(&["c"])));
        assert_eq!(roster.links_of("a").unwrap().iter().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn test_linked_requires_agreement_once_both_reported() {
        let mut roster = Roster::default();
        roster.set_links("a", 1, links(&["b"]));
        assert!(roster.linked("a", "b"), "one-sided report is trusted");
        roster.set_links("b", 1, links(&[]));
        assert!(!roster.linked("a", "b"), "b dropped a, so the edge is gone");
        roster.set_links("b", 2, links(&["a"]));
        assert!(roster.linked("b", "a"));
    }

    #[test]
    fn test_reachability_and_relay_choice() {
        // a - b - c, plus d isolated; a reports no link to c.
        let mut roster = Roster::default();
        roster.set_links("a", 1, links(&["b"]));
        roster.set_links("b", 1, links(&["a", "c"]));
        roster.set_links("c", 1, links(&["b"]));
        roster.set_links("d", 1, links(&[]));

        assert_eq!(roster.reachable_from("a"), present(&["b", "c"]));
        assert_eq!(roster.relay_for("a", "c"), Some("b".into()));
        assert_eq!(roster.relay_for("a", "d"), None);

        // A two-hop chain a - b - c - e: e is reached via b.
        roster.set_links("c", 2, links(&["b", "e"]));
        roster.set_links("e", 1, links(&["c"]));
        assert_eq!(roster.relay_for("a", "e"), Some("b".into()));
    }

    #[test]
    fn test_forward_targets_skip_peers_linked_to_author() {
        // Full mesh among a, b, d; c is only linked to b.
        let mut roster = Roster::default();
        roster.set_links("a", 1, links(&["b", "d"]));
        roster.set_links("b", 1, links(&["a", "c", "d"]));
        roster.set_links("c", 1, links(&["b"]));
        roster.set_links("d", 1, links(&["a", "b"]));

        // b got a's message from a: only c lacks a direct link to a.
        assert_eq!(roster.forward_targets("b", "a", "a"), vec!["c".to_string()]);
        // b got c's message from c: a and d both lack a link to c.
        assert_eq!(roster.forward_targets("b", "c", "c"), vec!["a".to_string(), "d".to_string()]);
        // a got c's message relayed by b: d lacks a link to c, but b already forwards to d too.
        assert_eq!(roster.forward_targets("a", "c", "b"), vec!["d".to_string()]);
    }

    #[test]
    fn test_eviction_latest_joiner_loses() {
        let mut roster = Roster::default();
        roster.upsert(member("a", 10, false));
        roster.upsert(member("b", 20, false));
        roster.upsert(member("c", 30, false));
        roster.upsert(member("d", 30, false));
        let all = present(&["a", "b", "c", "d"]);

        assert!(roster.evicted(&all, None).is_empty());
        assert!(roster.evicted(&all, Some(4)).is_empty());
        // c and d tie on join time; pubkey order breaks the tie.
        assert_eq!(roster.evicted(&all, Some(3)), vec!["d".to_string()]);
        assert_eq!(roster.evicted(&all, Some(2)), vec!["c".to_string(), "d".to_string()]);
        // Absent members do not take seats.
        assert!(roster.evicted(&present(&["a", "c"]), Some(2)).is_empty());
    }

    #[test]
    fn test_admin_takes_seat_from_latest_non_admin() {
        let mut roster = Roster::default();
        roster.upsert(member("a", 10, false));
        roster.upsert(member("b", 20, false));
        roster.upsert(member("z", 99, true));
        let all = present(&["a", "b", "z"]);
        assert_eq!(roster.evicted(&all, Some(2)), vec!["b".to_string()]);
        // Admins are never evicted, even beyond the cap.
        roster.upsert(member("y", 100, true));
        let all = present(&["a", "b", "y", "z"]);
        assert_eq!(roster.evicted(&all, Some(1)), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_latest_beyond_cap() {
        let entries = || vec![("b".to_string(), 5), ("a".to_string(), 5), ("c".to_string(), 1)];
        assert_eq!(latest_beyond_cap(entries(), Some(1)), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(latest_beyond_cap(entries(), Some(2)), vec!["b".to_string()]);
        assert!(latest_beyond_cap(entries(), Some(3)).is_empty());
        assert!(latest_beyond_cap(entries(), None).is_empty());
        assert_eq!(latest_beyond_cap(entries(), Some(0)).len(), 3);
    }

    fn up(f: &str, p: &str) -> Upload {
        (f.to_string(), p.to_string())
    }

    #[test]
    fn test_upload_queue_fifo_with_two_slots() {
        let mut q = UploadQueue::new(MAX_CONCURRENT_UPLOADS);
        assert_eq!(q.request("f", "a"), QueueDecision::Start);
        assert_eq!(q.request("f", "b"), QueueDecision::Start);
        assert_eq!(q.request("f", "c"), QueueDecision::Queued(1));
        assert_eq!(q.request("g", "d"), QueueDecision::Queued(2));
        assert_eq!(q.request("f", "c"), QueueDecision::Duplicate);
        assert_eq!(q.request("f", "a"), QueueDecision::Duplicate);
        assert_eq!(q.counts("f"), (2, 1));

        // a finishes: c (first in line) starts, d moves up.
        assert_eq!(q.finish("f", "a"), vec![up("f", "c")]);
        assert!(q.is_active("f", "c"));
        assert_eq!(q.positions(), vec![(up("g", "d"), 1)]);

        // Finishing something unknown changes nothing.
        assert!(q.finish("zzz", "a").is_empty());
    }

    #[test]
    fn test_upload_queue_peer_and_file_removal() {
        let mut q = UploadQueue::new(2);
        q.request("f", "a");
        q.request("g", "a");
        q.request("f", "b");
        q.request("f", "c");
        // a leaves: both its uploads go, b and c start.
        assert_eq!(q.remove_peer("a"), vec![up("f", "b"), up("f", "c")]);
        assert!(q.positions().is_empty());
        // The file is withdrawn: nothing left.
        assert!(q.remove_file("f").is_empty());
        assert_eq!(q.counts("f"), (0, 0));
    }

    #[test]
    fn test_gossip_dedup_is_bounded() {
        let mut dedup = GossipDedup::new(2);
        assert!(dedup.insert("1"));
        assert!(dedup.contains("1"));
        assert!(!dedup.insert("1"));
        assert!(dedup.insert("2"));
        assert!(dedup.insert("3"));
        assert!(dedup.insert("1"), "oldest id was evicted");
        assert!(!dedup.insert("3"));
    }

    #[test]
    fn test_cap_parsing() {
        assert_eq!(parse_cap(None, 25), Some(25));
        assert_eq!(parse_cap(Some("10"), 25), Some(10));
        assert_eq!(parse_cap(Some("100"), 25), Some(100), "no hard ceiling");
        assert_eq!(parse_cap(Some("0"), 25), None);
        assert_eq!(parse_cap(Some("unlimited"), 25), None);
        assert_eq!(parse_cap(Some("abc"), 25), Some(25));
        assert_eq!(format_cap(None), "0");
        assert_eq!(format_cap(Some(7)), "7");
    }

    #[test]
    fn test_room_params_and_invite() {
        let fragment = FragmentParams::parse(
            "#room=r&key=k&adm=pub&admsk=sec&max=3&maxa=2&maxv=0&turn=turn:a.example:3478, turns:b.example&turnuser=u&turnpass=p",
        );
        let params = RoomParams::from_fragment(&fragment);
        assert_eq!(params.admin_pubkey.as_deref(), Some("pub"));
        assert_eq!(params.admin_secret.as_deref(), Some("sec"));
        assert_eq!(params.member_cap, Some(3));
        assert_eq!(params.voice_cap, Some(2));
        assert_eq!(params.video_cap, None);
        assert_eq!(params.turn_urls, vec!["turn:a.example:3478", "turns:b.example"]);
        assert_eq!(params.turn_user.as_deref(), Some("u"));
        assert_eq!(params.turn_pass.as_deref(), Some("p"));

        let invite = invite_fragment(&fragment);
        assert_eq!(invite.get("admsk"), None);
        assert_eq!(invite.get("adm"), Some("pub"));
        assert_eq!(invite.get("key"), Some("k"));

        let defaults = RoomParams::from_fragment(&FragmentParams::parse("#room=r&key=k"));
        assert_eq!(defaults.member_cap, Some(DEFAULT_MEMBER_CAP));
        assert_eq!(defaults.voice_cap, Some(DEFAULT_VOICE_CAP));
        assert_eq!(defaults.video_cap, Some(DEFAULT_VIDEO_CAP));
        assert!(defaults.turn_urls.is_empty());
    }
}
