//! The room's chat log: the signed originals every member keeps and syncs with the others,
//! so whoever joins (or reconnects) gets the same conversation. Pure Rust, no browser APIs.
//!
//! The log is a set of envelopes. Each rule below gives the same log whatever order the
//! envelopes arrive in, so two members holding the same envelopes hold the same log:
//! - A message (`Chat`) or file card (`FileOffer`, keyed by its `file_id`) is a thread root,
//!   written once.
//! - Its dependents: the root author's latest `Edit` (chat) or withdrawal (`FileCancel` with
//!   no recipient, file card), and every member's latest toggle per reaction emoji. The latest
//!   `(ts, id)` wins. A dependent that arrives before its root waits for it.
//! - A `Delete` by the root's author drops the root and its dependents and stays as a
//!   tombstone, so no other member can bring the message back. One by anyone else is ignored.
//! - Each author's latest `Hello`, to name them after they left (outside the budget).
//! - At most `CHAT_LOG_BUDGET` bytes: the oldest threads by `(ts, id)` go first, and nothing
//!   older than the last one dropped (the horizon) is taken back.
//!
//! Members reconcile their logs with time-window digests (`summary`, `answer_summary`,
//! `answer_ask`, `PullRound`): windows are aligned to the epoch at three levels (day, hour,
//! minute), each holding the count and the XOR of its entries' fingerprints, so a pair that
//! is already in sync exchanges one summary and an empty answer.
//!
//! The caller verifies every envelope's signature (with its own `adm`) before merging it.

use crate::messages::{RoomBody, RoomEnvelope, SyncWindow};
use crate::room::{GossipDedup, REACTIONS};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// The most every member keeps: serialized envelopes, Hellos not counted.
pub const CHAT_LOG_BUDGET: usize = 32 * 1024 * 1024;
/// Envelopes dated further ahead of the receiver's clock are not logged (yet): the budget
/// and the windows go by time, so a far-future date would pin an entry for good.
pub const MAX_FUTURE_SKEW_MS: u64 = 5 * 60 * 1000;
/// A larger envelope is shown live but never logged or synced.
pub const MAX_ENTRY_BYTES: usize = 64 * 1024;
/// Longer envelope ids and targets are refused (ours are UUIDs).
pub const MAX_ID_CHARS: usize = 64;
/// Window spans of the three summary levels: day, hour, minute.
pub const SYNC_SPANS: [u64; 3] = [86_400_000, 3_600_000, 60_000];
/// A differing window with at most this many entries is answered with its ids, a larger one
/// with its non-empty windows one level down (the minute level always lists ids).
pub const SYNC_LIST_MAX: usize = 256;
/// Ids per `SyncWant`.
pub const SYNC_WANT_MAX: usize = 512;
/// Ids or windows per `SyncDiff` frame, to stay well under data-channel message limits.
pub const SYNC_FRAME_ITEMS: usize = 1000;
/// Dependents waiting for their root (oldest dropped first: a later sync fetches them again).
const PENDING_LIMIT: usize = 1024;
/// Ids refused for good (superseded, deleted, forged…), so syncs don't ask for them again.
const DROPPED_CAPACITY: usize = 65_536;

/// The fingerprint of an entry in the window digests.
pub fn entry_fingerprint(id: &str) -> u64 {
    let digest = Sha256::new().chain_update(b"dchat:sync:v1\0").chain_update(id.as_bytes()).finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

/// Where a thread sits: its root's (or tombstone's) time and id.
type Key = (u64, String);

#[derive(Debug, Clone)]
struct Held {
    env: RoomEnvelope,
    bytes: usize,
}

impl Held {
    fn key(&self) -> Key {
        (self.env.ts, self.env.id.clone())
    }
}

// Nearly every thread is a root: boxing it would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Thread {
    Root {
        target: String,
        root: Held,
        edit: Option<Held>,
        withdraw: Option<Held>,
        /// By (member, emoji).
        reactions: BTreeMap<(String, String), Held>,
    },
    Tombstone {
        target: String,
        delete: Held,
    },
}

impl Thread {
    /// The root (or tombstone) first, then its dependents.
    fn entries(&self) -> Vec<&Held> {
        match self {
            Thread::Root { root, edit, withdraw, reactions, .. } => {
                let mut all = vec![root];
                all.extend(edit.iter());
                all.extend(withdraw.iter());
                all.extend(reactions.values());
                all
            }
            Thread::Tombstone { delete, .. } => vec![delete],
        }
    }

    fn find(&self, id: &str) -> Option<&Held> {
        self.entries().into_iter().find(|h| h.env.id == id)
    }
}

/// What an envelope is to the log.
#[derive(Clone, Copy)]
enum Kind<'a> {
    Root(&'a str),
    Edit(&'a str),
    Withdraw(&'a str),
    Reaction(&'a str, &'a str),
    Delete(&'a str),
    Hello,
    NotLogged,
}

fn kind(envelope: &RoomEnvelope) -> Kind<'_> {
    match &envelope.body {
        RoomBody::Chat { .. } => Kind::Root(&envelope.id),
        RoomBody::FileOffer { file_id, .. } => Kind::Root(file_id),
        RoomBody::Edit { target, .. } => Kind::Edit(target),
        RoomBody::FileCancel { to: None, file_id } => Kind::Withdraw(file_id),
        RoomBody::Reaction { target, emoji, .. } => Kind::Reaction(target, emoji),
        RoomBody::Delete { target } => Kind::Delete(target),
        RoomBody::Hello { .. } => Kind::Hello,
        _ => Kind::NotLogged,
    }
}

/// What changed for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A message or file card appeared (`target`), with whatever edit, reactions or
    /// withdrawal it already has: build its row from `view`.
    Message(String),
    Edited(String),
    Reacted(String),
    Withdrawn(String),
    /// The message is gone.
    Deleted(String),
    /// A (newer) name for this author.
    Named(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// Already in the log (or waiting for its root).
    Held,
    /// Refused before (superseded, deleted, forged…).
    Dropped,
    /// A newer edit, toggle, withdrawal or Hello is held.
    Superseded,
    /// Its author deleted it.
    Deleted,
    /// Older than what the budget keeps.
    BelowHorizon,
    /// An edit, withdrawal or deletion by someone other than the author, or of the wrong kind.
    NotAuthor,
    /// Another root holds the same target.
    Conflict,
    /// Dated too far ahead of our clock: not logged for now.
    Future,
    TooLarge,
    BadReaction,
    /// Not part of the log (typing, DMs, voice state, direct-only messages…).
    NotLogged,
}

impl Ignored {
    /// Refused for good: never asked for again.
    fn is_final(self) -> bool {
        !matches!(self, Ignored::Held | Ignored::Dropped | Ignored::Future | Ignored::NotLogged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    Added(Vec<Effect>),
    /// A dependent whose root has not arrived (yet).
    Pending,
    Ignored(Ignored),
}

/// A thread as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadView<'a> {
    pub root: &'a RoomEnvelope,
    pub edit: Option<&'a RoomEnvelope>,
    pub withdrawn: bool,
    /// `(emoji, members)` in `REACTIONS` order, members sorted, empty ones skipped.
    pub reactions: Vec<(String, Vec<String>)>,
}

/// One frame of an answer to a summary or an ask (a `SyncDiff`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFrame {
    pub ids: Vec<String>,
    pub level: u8,
    pub windows: Vec<SyncWindow>,
    pub last: bool,
}

#[derive(Debug)]
pub struct ChatLog {
    budget: usize,
    threads: BTreeMap<Key, Thread>,
    /// Root target (message id or file id) → its thread.
    targets: HashMap<String, Key>,
    /// Target → tombstone threads deleting it.
    tombstones: HashMap<String, BTreeSet<Key>>,
    /// Every held envelope id → its thread.
    entries: HashMap<String, Key>,
    /// (thread time, id) of every held envelope, for listing a window's ids.
    by_time: BTreeSet<(u64, String)>,
    /// Per level: window start → (count, XOR of fingerprints).
    digests: [BTreeMap<u64, (u32, u64)>; 3],
    hellos: HashMap<String, RoomEnvelope>,
    pending: VecDeque<(String, Held)>,
    pending_ids: HashSet<String>,
    dropped: GossipDedup,
    bytes: usize,
    horizon: Option<Key>,
}

impl Default for ChatLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatLog {
    pub fn new() -> Self {
        Self::with_budget(CHAT_LOG_BUDGET)
    }

    pub fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            threads: BTreeMap::new(),
            targets: HashMap::new(),
            tombstones: HashMap::new(),
            entries: HashMap::new(),
            by_time: BTreeSet::new(),
            digests: Default::default(),
            hellos: HashMap::new(),
            pending: VecDeque::new(),
            pending_ids: HashSet::new(),
            dropped: GossipDedup::new(DROPPED_CAPACITY),
            bytes: 0,
            horizon: None,
        }
    }

    /// Forget everything (the tab left the room for good).
    pub fn clear(&mut self) {
        *self = Self::with_budget(self.budget);
    }

    /// Add a verified envelope; `now` is the local clock (ms).
    pub fn merge(&mut self, envelope: &RoomEnvelope, now: u64) -> Merge {
        let kind = kind(envelope);
        if matches!(kind, Kind::NotLogged) {
            return Merge::Ignored(Ignored::NotLogged);
        }
        if envelope.ts > now.saturating_add(MAX_FUTURE_SKEW_MS) {
            return Merge::Ignored(Ignored::Future);
        }
        if self.entries.contains_key(&envelope.id) || self.pending_ids.contains(&envelope.id) {
            return Merge::Ignored(Ignored::Held);
        }
        if self.dropped.contains(&envelope.id) {
            return Merge::Ignored(Ignored::Dropped);
        }
        let target_too_long = match kind {
            Kind::Root(t) | Kind::Edit(t) | Kind::Withdraw(t) | Kind::Reaction(t, _) | Kind::Delete(t) => {
                t.len() > MAX_ID_CHARS
            }
            Kind::Hello | Kind::NotLogged => false,
        };
        let bytes = serde_json::to_vec(envelope).map_or(usize::MAX, |v| v.len());
        let held = Held { env: envelope.clone(), bytes };
        let result = if envelope.id.len() > MAX_ID_CHARS || target_too_long || bytes > MAX_ENTRY_BYTES {
            Merge::Ignored(Ignored::TooLarge)
        } else {
            match kind {
                Kind::Hello => return self.merge_hello(envelope),
                Kind::Root(target) => self.merge_root(target.to_string(), held),
                Kind::Delete(target) => self.merge_delete(target.to_string(), held),
                Kind::Reaction(_, emoji) if !REACTIONS.contains(&emoji) => Merge::Ignored(Ignored::BadReaction),
                Kind::Edit(target) | Kind::Withdraw(target) | Kind::Reaction(target, _) => {
                    self.merge_dependent(target.to_string(), held)
                }
                Kind::NotLogged => Merge::Ignored(Ignored::NotLogged),
            }
        };
        if let Merge::Ignored(reason) = result {
            if reason.is_final() {
                self.dropped.insert(&envelope.id);
            }
        }
        self.evict();
        result
    }

    fn merge_hello(&mut self, envelope: &RoomEnvelope) -> Merge {
        let newer = self
            .hellos
            .get(&envelope.author)
            .is_none_or(|known| (known.ts, &known.id) < (envelope.ts, &envelope.id));
        if !newer {
            return Merge::Ignored(Ignored::Superseded);
        }
        if serde_json::to_vec(envelope).map_or(true, |v| v.len() > MAX_ENTRY_BYTES) {
            return Merge::Ignored(Ignored::TooLarge);
        }
        self.hellos.insert(envelope.author.clone(), envelope.clone());
        Merge::Added(vec![Effect::Named(envelope.author.clone())])
    }

    fn merge_root(&mut self, target: String, held: Held) -> Merge {
        let key = held.key();
        if self.horizon.as_ref().is_some_and(|horizon| key <= *horizon) {
            return Merge::Ignored(Ignored::BelowHorizon);
        }
        // A tombstone by the same author keeps it deleted; anyone else's was forged.
        if let Some(keys) = self.tombstones.get(&target).cloned() {
            let mut deleted = false;
            for tombstone in keys {
                let by_author = matches!(
                    self.threads.get(&tombstone),
                    Some(Thread::Tombstone { delete, .. }) if delete.env.author == held.env.author
                );
                if by_author {
                    deleted = true;
                } else {
                    self.remove_thread(&tombstone, true);
                }
            }
            if deleted {
                self.drop_pending(&target);
                return Merge::Ignored(Ignored::Deleted);
            }
        }
        if self.targets.contains_key(&target) {
            return Merge::Ignored(Ignored::Conflict);
        }
        self.index(&key, &held);
        self.targets.insert(target.clone(), key.clone());
        self.threads.insert(
            key,
            Thread::Root {
                target: target.clone(),
                root: held,
                edit: None,
                withdraw: None,
                reactions: BTreeMap::new(),
            },
        );
        for waiting in self.take_pending(&target) {
            let id = waiting.env.id.clone();
            if let Merge::Ignored(reason) = self.merge_dependent(target.clone(), waiting) {
                if reason.is_final() {
                    self.dropped.insert(&id);
                }
            }
        }
        Merge::Added(vec![Effect::Message(target)])
    }

    fn merge_dependent(&mut self, target: String, held: Held) -> Merge {
        let Some(key) = self.targets.get(&target).cloned() else {
            // The root has not arrived (or was dropped by the budget, or deleted): wait.
            self.push_pending(target, held);
            return Merge::Pending;
        };
        let Some(Thread::Root { root, edit, withdraw, reactions, .. }) = self.threads.get(&key) else {
            return Merge::Ignored(Ignored::NotAuthor);
        };
        let author = held.env.author.clone();
        let by_root_author = author == root.env.author;
        let (current, effect) = match &held.env.body {
            RoomBody::Edit { .. } if by_root_author && matches!(root.env.body, RoomBody::Chat { .. }) => {
                (edit.as_ref(), Effect::Edited(target.clone()))
            }
            RoomBody::FileCancel { .. } if by_root_author && matches!(root.env.body, RoomBody::FileOffer { .. }) => {
                (withdraw.as_ref(), Effect::Withdrawn(target.clone()))
            }
            RoomBody::Reaction { emoji, .. } => {
                (reactions.get(&(author.clone(), emoji.clone())), Effect::Reacted(target.clone()))
            }
            _ => return Merge::Ignored(Ignored::NotAuthor),
        };
        if current.is_some_and(|c| c.key() >= held.key()) {
            return Merge::Ignored(Ignored::Superseded);
        }
        self.index(&key, &held);
        let Some(Thread::Root { edit, withdraw, reactions, .. }) = self.threads.get_mut(&key) else {
            return Merge::Ignored(Ignored::NotAuthor);
        };
        let replaced = match &held.env.body {
            RoomBody::Edit { .. } => edit.replace(held),
            RoomBody::FileCancel { .. } => withdraw.replace(held),
            RoomBody::Reaction { emoji, .. } => reactions.insert((author, emoji.clone()), held),
            _ => None,
        };
        if let Some(old) = replaced {
            self.unindex(&key, &old);
            self.dropped.insert(&old.env.id);
        }
        Merge::Added(vec![effect])
    }

    fn merge_delete(&mut self, target: String, held: Held) -> Merge {
        let mut effects = Vec::new();
        if let Some(root_key) = self.targets.get(&target).cloned() {
            let allowed = matches!(
                self.threads.get(&root_key),
                Some(Thread::Root { root, .. })
                    if root.env.author == held.env.author && matches!(root.env.body, RoomBody::Chat { .. })
            );
            if !allowed {
                return Merge::Ignored(Ignored::NotAuthor);
            }
            self.remove_thread(&root_key, true);
            effects.push(Effect::Deleted(target.clone()));
        }
        let key = held.key();
        if self.horizon.as_ref().is_some_and(|horizon| key <= *horizon) {
            return if effects.is_empty() { Merge::Ignored(Ignored::BelowHorizon) } else { Merge::Added(effects) };
        }
        self.index(&key, &held);
        self.tombstones.entry(target.clone()).or_default().insert(key.clone());
        self.threads.insert(key, Thread::Tombstone { target, delete: held });
        Merge::Added(effects)
    }

    /// Drop the oldest threads until the log fits its budget.
    fn evict(&mut self) {
        while self.bytes > self.budget {
            let Some(key) = self.threads.keys().next().cloned() else {
                break;
            };
            // Not marked dropped: the horizon refuses them, and lowers never.
            self.remove_thread(&key, false);
            if self.horizon.as_ref().is_none_or(|h| key > *h) {
                self.horizon = Some(key);
            }
        }
    }

    fn remove_thread(&mut self, key: &Key, drop_ids: bool) {
        let Some(thread) = self.threads.remove(key) else {
            return;
        };
        for held in thread.entries() {
            self.unindex(key, held);
            if drop_ids {
                self.dropped.insert(&held.env.id);
            }
        }
        match thread {
            Thread::Root { target, .. } => {
                if self.targets.get(&target) == Some(key) {
                    self.targets.remove(&target);
                }
            }
            Thread::Tombstone { target, .. } => {
                if let Some(keys) = self.tombstones.get_mut(&target) {
                    keys.remove(key);
                    if keys.is_empty() {
                        self.tombstones.remove(&target);
                    }
                }
            }
        }
    }

    fn index(&mut self, key: &Key, held: &Held) {
        let id = &held.env.id;
        self.entries.insert(id.clone(), key.clone());
        self.by_time.insert((key.0, id.clone()));
        let fingerprint = entry_fingerprint(id);
        for (level, span) in SYNC_SPANS.iter().enumerate() {
            let digest = self.digests[level].entry(key.0 - key.0 % span).or_insert((0, 0));
            digest.0 += 1;
            digest.1 ^= fingerprint;
        }
        self.bytes += held.bytes;
    }

    fn unindex(&mut self, key: &Key, held: &Held) {
        let id = &held.env.id;
        if self.entries.remove(id).is_none() {
            return;
        }
        self.by_time.remove(&(key.0, id.clone()));
        let fingerprint = entry_fingerprint(id);
        for (level, span) in SYNC_SPANS.iter().enumerate() {
            let start = key.0 - key.0 % span;
            if let Some(digest) = self.digests[level].get_mut(&start) {
                digest.0 -= 1;
                digest.1 ^= fingerprint;
                if digest.0 == 0 {
                    self.digests[level].remove(&start);
                }
            }
        }
        self.bytes -= held.bytes;
    }

    fn push_pending(&mut self, target: String, held: Held) {
        if self.pending.len() >= PENDING_LIMIT {
            if let Some((_, oldest)) = self.pending.pop_front() {
                self.pending_ids.remove(&oldest.env.id);
            }
        }
        self.pending_ids.insert(held.env.id.clone());
        self.pending.push_back((target, held));
    }

    fn take_pending(&mut self, target: &str) -> Vec<Held> {
        let (mine, rest): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|(t, _)| t == target);
        self.pending = rest.into();
        mine.into_iter()
            .map(|(_, held)| {
                self.pending_ids.remove(&held.env.id);
                held
            })
            .collect()
    }

    fn drop_pending(&mut self, target: &str) {
        for held in self.take_pending(target) {
            self.dropped.insert(&held.env.id);
        }
    }

    // ---- Queries ----------------------------------------------------------------------

    /// Whether the envelope `id` is held (pending dependents are not).
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// Whether a sync should fetch `id`: not held, not waiting for its root, never refused.
    pub fn wants(&self, id: &str) -> bool {
        id.len() <= MAX_ID_CHARS && !self.entries.contains_key(id) && !self.pending_ids.contains(id) && !self.dropped.contains(id)
    }

    /// A held envelope by id.
    pub fn get(&self, id: &str) -> Option<&RoomEnvelope> {
        let key = self.entries.get(id)?;
        self.threads.get(key)?.find(id).map(|held| &held.env)
    }

    /// How many envelopes are held (Hellos and pending dependents not counted).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The time below which the log keeps nothing (`None` while it fits its budget).
    pub fn horizon(&self) -> Option<u64> {
        self.horizon.as_ref().map(|(ts, _)| *ts)
    }

    /// The latest Hello of `author`.
    pub fn hello(&self, author: &str) -> Option<&RoomEnvelope> {
        self.hellos.get(author)
    }

    fn root(&self, target: &str) -> Option<&Thread> {
        self.threads.get(self.targets.get(target)?)
    }

    /// The author of chat message `target` (only they may edit or delete it).
    pub fn chat_author(&self, target: &str) -> Option<&str> {
        match self.root(target)? {
            Thread::Root { root, .. } if matches!(root.env.body, RoomBody::Chat { .. }) => Some(&root.env.author),
            _ => None,
        }
    }

    /// Whether `member`'s latest toggle of `emoji` on `target` is on.
    pub fn has_reaction(&self, target: &str, emoji: &str, member: &str) -> bool {
        match self.root(target) {
            Some(Thread::Root { reactions, .. }) => reactions
                .get(&(member.to_string(), emoji.to_string()))
                .is_some_and(|held| matches!(held.env.body, RoomBody::Reaction { on: true, .. })),
            _ => false,
        }
    }

    /// `(emoji, members)` reacting to `target`, in `REACTIONS` order, members sorted.
    pub fn tally(&self, target: &str) -> Vec<(String, Vec<String>)> {
        let Some(Thread::Root { reactions, .. }) = self.root(target) else {
            return Vec::new();
        };
        REACTIONS
            .iter()
            .filter_map(|emoji| {
                let members: Vec<String> = reactions
                    .iter()
                    .filter(|((_, e), held)| e == emoji && matches!(held.env.body, RoomBody::Reaction { on: true, .. }))
                    .map(|((member, _), _)| member.clone())
                    .collect();
                (!members.is_empty()).then(|| (emoji.to_string(), members))
            })
            .collect()
    }

    /// The message or file card `target` as the UI shows it.
    pub fn view(&self, target: &str) -> Option<ThreadView<'_>> {
        match self.root(target)? {
            Thread::Root { root, edit, withdraw, .. } => Some(ThreadView {
                root: &root.env,
                edit: edit.as_ref().map(|held| &held.env),
                withdrawn: withdraw.is_some(),
                reactions: self.tally(target),
            }),
            Thread::Tombstone { .. } => None,
        }
    }

    /// Every file card held, with whether its author withdrew it.
    pub fn file_offers(&self) -> Vec<(&RoomEnvelope, bool)> {
        self.threads
            .values()
            .filter_map(|thread| match thread {
                Thread::Root { root, withdraw, .. } if matches!(root.env.body, RoomBody::FileOffer { .. }) => {
                    Some((&root.env, withdraw.is_some()))
                }
                _ => None,
            })
            .collect()
    }

    /// Ids of every held message and file card, oldest first.
    pub fn roots(&self) -> Vec<String> {
        self.threads
            .values()
            .filter_map(|thread| match thread {
                Thread::Root { target, .. } => Some(target.clone()),
                Thread::Tombstone { .. } => None,
            })
            .collect()
    }

    // ---- Sync -------------------------------------------------------------------------

    /// The non-empty day windows: what a pull round starts with.
    pub fn summary(&self) -> Vec<SyncWindow> {
        self.windows_in(0, 0, u64::MAX)
    }

    /// `(count, hash)` of a window, `None` when empty.
    pub fn window(&self, level: u8, start: u64) -> Option<(u32, u64)> {
        self.digests.get(level as usize)?.get(&start).copied()
    }

    fn windows_in(&self, level: usize, from: u64, to: u64) -> Vec<SyncWindow> {
        self.digests[level]
            .range(from..to)
            .rev()
            .map(|(start, (count, hash))| SyncWindow { start: *start, count: *count, hash: *hash })
            .collect()
    }

    /// Answer a puller's summary: describe our day windows that differ from theirs (and that
    /// end after their horizon), newest first.
    pub fn answer_summary(&self, theirs: &[SyncWindow], horizon: Option<u64>) -> Vec<DiffFrame> {
        let theirs: HashMap<u64, (u32, u64)> = theirs.iter().map(|w| (w.start, (w.count, w.hash))).collect();
        let starts: Vec<u64> = self.digests[0]
            .iter()
            .rev()
            .filter(|(start, digest)| theirs.get(start) != Some(digest))
            .map(|(start, _)| *start)
            .collect();
        self.describe(0, &starts, horizon)
    }

    /// Answer a puller's ask: describe these windows of `level`.
    pub fn answer_ask(&self, level: u8, starts: &[u64], horizon: Option<u64>) -> Vec<DiffFrame> {
        if level as usize >= SYNC_SPANS.len() {
            return frames(Vec::new(), level, Vec::new());
        }
        self.describe(level as usize, starts, horizon)
    }

    /// The ids held in each small window, the non-empty windows one level down of each
    /// large one; nothing at or below the puller's horizon.
    fn describe(&self, level: usize, starts: &[u64], horizon: Option<u64>) -> Vec<DiffFrame> {
        let span = SYNC_SPANS[level];
        let floor = horizon.unwrap_or(0);
        let mut ids = Vec::new();
        let mut children = Vec::new();
        for &start in starts {
            let Some(&(count, _)) = self.digests[level].get(&start) else {
                continue;
            };
            let end = start.saturating_add(span);
            if end <= floor {
                continue;
            }
            if count as usize <= SYNC_LIST_MAX || level + 1 == SYNC_SPANS.len() {
                let range = (start.max(floor), String::new())..(end, String::new());
                ids.extend(self.by_time.range(range).rev().map(|(_, id)| id.clone()));
            } else {
                children.extend(self.windows_in(level + 1, start.max(floor - floor % SYNC_SPANS[level + 1]), end));
            }
        }
        frames(ids, (level + 1).min(SYNC_SPANS.len() - 1) as u8, children)
    }

    /// The held envelopes among `ids`, ready to send: newest threads first, each root before
    /// its dependents, an author's Hello ahead of their first entry (once per serve: the
    /// caller keeps `hellos_sent`). Batches stay under `max_bytes` (a larger envelope goes alone).
    pub fn batches(&self, ids: &[String], max_bytes: usize, hellos_sent: &mut HashSet<String>) -> Vec<Vec<RoomEnvelope>> {
        let mut by_thread: BTreeMap<&Key, Vec<&Held>> = BTreeMap::new();
        for id in ids {
            let Some(key) = self.entries.get(id) else {
                continue;
            };
            if let Some(held) = self.threads.get(key).and_then(|t| t.find(id)) {
                by_thread.entry(key).or_default().push(held);
            }
        }
        let mut batcher = Batcher { max_bytes, done: Vec::new(), current: Vec::new(), bytes: 0 };
        for (key, mut held) in by_thread.into_iter().rev() {
            // Root (its id is the thread's) first.
            held.sort_by_key(|h| (h.env.id != key.1, h.key()));
            for h in held {
                if hellos_sent.insert(h.env.author.clone()) {
                    if let Some(hello) = self.hellos.get(&h.env.author) {
                        let bytes = serde_json::to_vec(hello).map_or(0, |v| v.len());
                        batcher.push(hello, bytes);
                    }
                }
                batcher.push(&h.env, h.bytes);
            }
        }
        batcher.finish()
    }
}

struct Batcher {
    max_bytes: usize,
    done: Vec<Vec<RoomEnvelope>>,
    current: Vec<RoomEnvelope>,
    bytes: usize,
}

impl Batcher {
    fn push(&mut self, envelope: &RoomEnvelope, bytes: usize) {
        if !self.current.is_empty() && self.bytes + bytes > self.max_bytes {
            self.done.push(std::mem::take(&mut self.current));
            self.bytes = 0;
        }
        self.current.push(envelope.clone());
        self.bytes += bytes;
    }

    fn finish(mut self) -> Vec<Vec<RoomEnvelope>> {
        if !self.current.is_empty() {
            self.done.push(self.current);
        }
        self.done
    }
}

fn frames(ids: Vec<String>, level: u8, windows: Vec<SyncWindow>) -> Vec<DiffFrame> {
    let mut out: Vec<DiffFrame> = ids
        .chunks(SYNC_FRAME_ITEMS)
        .map(|chunk| DiffFrame { ids: chunk.to_vec(), level, windows: Vec::new(), last: false })
        .chain(windows.chunks(SYNC_FRAME_ITEMS).map(|chunk| DiffFrame {
            ids: Vec::new(),
            level,
            windows: chunk.to_vec(),
            last: false,
        }))
        .collect();
    if out.is_empty() {
        out.push(DiffFrame { ids: Vec::new(), level, windows: Vec::new(), last: false });
    }
    if let Some(final_frame) = out.last_mut() {
        final_frame.last = true;
    }
    out
}

/// What the puller sends next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullStep {
    Ask { level: u8, starts: Vec<u64> },
    Want(Vec<String>),
}

/// One member pulling what it lacks from one peer: summary, asks down the levels, then
/// wants (one in flight at a time).
#[derive(Debug, Default)]
pub struct PullRound {
    open_answers: usize,
    queue: VecDeque<String>,
    in_flight: bool,
    wanted: HashSet<String>,
}

impl PullRound {
    /// A new round and its summary to send: `(round, horizon, day windows)`.
    pub fn start(log: &ChatLog) -> (Self, Option<u64>, Vec<SyncWindow>) {
        let round = Self { open_answers: 1, ..Default::default() };
        (round, log.horizon(), log.summary())
    }

    /// A frame of the peer's answer: ids to fetch, windows to look into.
    pub fn on_diff(&mut self, log: &ChatLog, ids: &[String], level: u8, windows: &[SyncWindow], last: bool) -> Vec<PullStep> {
        let mut steps = Vec::new();
        for id in ids {
            if log.wants(id) && self.wanted.insert(id.clone()) {
                self.queue.push_back(id.clone());
            }
        }
        let differing: Vec<u64> = windows
            .iter()
            .filter(|w| log.window(level, w.start) != Some((w.count, w.hash)))
            .map(|w| w.start)
            .collect();
        if !differing.is_empty() && (level as usize) < SYNC_SPANS.len() {
            self.open_answers += 1;
            steps.push(PullStep::Ask { level, starts: differing });
        }
        if last {
            self.open_answers = self.open_answers.saturating_sub(1);
        }
        steps.extend(self.next_want());
        steps
    }

    /// Whether a received envelope was asked for in this round (Hellos come along).
    pub fn accepts(&self, envelope: &RoomEnvelope) -> bool {
        matches!(envelope.body, RoomBody::Hello { .. }) || self.wanted.contains(&envelope.id)
    }

    /// A batch arrived; `last` ends the answer to the want in flight.
    pub fn on_batch(&mut self, last: bool) -> Option<PullStep> {
        if last {
            self.in_flight = false;
        }
        self.next_want()
    }

    fn next_want(&mut self) -> Option<PullStep> {
        if self.in_flight || self.queue.is_empty() {
            return None;
        }
        let n = self.queue.len().min(SYNC_WANT_MAX);
        self.in_flight = true;
        Some(PullStep::Want(self.queue.drain(..n).collect()))
    }

    /// Whether anything was asked for (the UI shows a loading line meanwhile).
    pub fn fetching(&self) -> bool {
        !self.wanted.is_empty()
    }

    pub fn is_done(&self) -> bool {
        self.open_answers == 0 && !self.in_flight && self.queue.is_empty()
    }
}

/// Which peer a member pulls from: one at a time, the others wait their turn.
#[derive(Debug, Default)]
pub struct SyncQueue {
    waiting: Vec<(String, u64)>,
    active: Option<String>,
}

impl SyncQueue {
    /// Pull from `peer` when its turn comes (no-op if already queued or active).
    pub fn enqueue(&mut self, peer: &str, now: u64) {
        if self.active.as_deref() == Some(peer) || self.waiting.iter().any(|(p, _)| p == peer) {
            return;
        }
        self.waiting.push((peer.to_string(), now));
    }

    /// The next peer to pull from, if none is active and one is ready.
    pub fn next(&mut self, now: u64) -> Option<String> {
        if self.active.is_some() {
            return None;
        }
        let index = self.waiting.iter().position(|(_, ready)| *ready <= now)?;
        let (peer, _) = self.waiting.remove(index);
        self.active = Some(peer.clone());
        Some(peer)
    }

    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// The active pull ended.
    pub fn finish(&mut self) {
        self.active = None;
    }

    /// Try `peer` again from `until`.
    pub fn defer(&mut self, peer: &str, until: u64) {
        if self.active.as_deref() == Some(peer) {
            self.active = None;
        }
        self.waiting.retain(|(p, _)| p != peer);
        self.waiting.push((peer.to_string(), until));
    }

    /// `peer` is gone.
    pub fn remove(&mut self, peer: &str) {
        if self.active.as_deref() == Some(peer) {
            self.active = None;
        }
        self.waiting.retain(|(p, _)| p != peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::SeedableRng;

    const NOW: u64 = 1_700_000_000_000;

    /// The log never checks signatures (its caller does), so tests build envelopes directly.
    fn env(id: &str, author: &str, ts: u64, body: RoomBody) -> RoomEnvelope {
        RoomEnvelope { id: id.into(), author: author.into(), ts, body, sig: "sig".into() }
    }

    fn chat(id: &str, author: &str, ts: u64, text: &str) -> RoomEnvelope {
        env(id, author, ts, RoomBody::Chat { text: text.into() })
    }

    fn edit(id: &str, author: &str, ts: u64, target: &str, text: &str) -> RoomEnvelope {
        env(id, author, ts, RoomBody::Edit { target: target.into(), text: text.into() })
    }

    fn delete(id: &str, author: &str, ts: u64, target: &str) -> RoomEnvelope {
        env(id, author, ts, RoomBody::Delete { target: target.into() })
    }

    fn react(id: &str, author: &str, ts: u64, target: &str, emoji: &str, on: bool) -> RoomEnvelope {
        env(id, author, ts, RoomBody::Reaction { target: target.into(), emoji: emoji.into(), on })
    }

    fn offer(id: &str, author: &str, ts: u64, file_id: &str) -> RoomEnvelope {
        env(
            id,
            author,
            ts,
            RoomBody::FileOffer {
                file_id: file_id.into(),
                name: "a.txt".into(),
                size: 3,
                mime_type: "text/plain".into(),
                caption: None,
            },
        )
    }

    fn withdraw(id: &str, author: &str, ts: u64, file_id: &str) -> RoomEnvelope {
        env(id, author, ts, RoomBody::FileCancel { to: None, file_id: file_id.into() })
    }

    fn hello(id: &str, author: &str, ts: u64, name: &str) -> RoomEnvelope {
        env(id, author, ts, RoomBody::Hello { name: name.into(), join_ts: ts, admin_proof: None })
    }

    fn merge_all(log: &mut ChatLog, envelopes: &[RoomEnvelope]) {
        for e in envelopes {
            log.merge(e, NOW);
        }
    }

    /// Everything observable about a log, for comparing two of them.
    fn snapshot(log: &ChatLog) -> String {
        let mut ids: Vec<&String> = log.entries.keys().collect();
        ids.sort();
        let views: Vec<String> = log
            .roots()
            .iter()
            .map(|t| {
                let v = log.view(t).unwrap();
                format!("{t}:{:?}:{}:{:?}", v.edit.map(|e| &e.id), v.withdrawn, v.reactions)
            })
            .collect();
        let mut hellos: Vec<String> = log.hellos.values().map(|h| h.id.clone()).collect();
        hellos.sort();
        let digests: Vec<Vec<SyncWindow>> = (0..3).map(|l| log.windows_in(l, 0, u64::MAX)).collect();
        format!("{ids:?}\n{views:?}\n{hellos:?}\n{digests:?}\n{:?} {}", log.horizon, log.bytes)
    }

    /// A corpus touching every rule: roots, edits (several, and a forged one), deletes
    /// (real and forged), reactions with toggles and ties, offers, withdrawals, Hellos.
    fn corpus() -> Vec<RoomEnvelope> {
        vec![
            hello("h-ana", "ana", NOW - 9000, "Ana"),
            hello("h-ana2", "ana", NOW - 100, "Ana B"),
            hello("h-bo", "bo", NOW - 9000, "Bo"),
            chat("m1", "ana", NOW - 5000, "one"),
            chat("m2", "bo", NOW - 4000, "two"),
            chat("m3", "ana", NOW - 3000, "three"),
            edit("e1", "ana", NOW - 2900, "m1", "one!"),
            edit("e2", "ana", NOW - 2800, "m1", "one!!"),
            edit("e-forged", "bo", NOW - 2700, "m1", "pwned"),
            delete("d3", "ana", NOW - 2000, "m3"),
            delete("d-forged", "bo", NOW - 1900, "m1"),
            react("r1", "bo", NOW - 1500, "m1", "👍", true),
            react("r2", "bo", NOW - 1400, "m1", "👍", false),
            react("r3", "ana", NOW - 1400, "m1", "👍", true),
            react("r4", "ana", NOW - 1300, "m2", "🎉", true),
            react("r5", "cy", NOW - 1300, "m3", "🎉", true),
            offer("o1", "bo", NOW - 1200, "f1"),
            withdraw("w1", "bo", NOW - 1100, "f1"),
            withdraw("w-forged", "ana", NOW - 1000, "f1"),
            react("r6", "ana", NOW - 900, "f1", "❤️", true),
            offer("o2", "ana", NOW - 800, "f2"),
        ]
    }

    #[test]
    fn test_merge_is_idempotent_and_order_independent() {
        let mut reference = ChatLog::new();
        merge_all(&mut reference, &corpus());
        let expected = snapshot(&reference);
        merge_all(&mut reference, &corpus());
        assert_eq!(snapshot(&reference), expected, "merging twice changes nothing");

        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..20 {
            let mut shuffled = corpus();
            shuffled.shuffle(&mut rng);
            let mut log = ChatLog::new();
            merge_all(&mut log, &shuffled);
            assert_eq!(snapshot(&log), expected);
        }

        // The rules applied: latest real edit, forged ones ignored, deleted message gone.
        let m1 = reference.view("m1").unwrap();
        assert_eq!(m1.edit.map(|e| e.id.as_str()), Some("e2"));
        assert_eq!(m1.reactions, vec![("👍".to_string(), vec!["ana".to_string()])]);
        assert!(reference.view("m3").is_none());
        assert!(reference.view("f1").unwrap().withdrawn);
        assert!(!reference.view("f2").unwrap().withdrawn);
        assert_eq!(reference.hello("ana").map(|h| h.id.as_str()), Some("h-ana2"));
        assert_eq!(reference.chat_author("m2"), Some("bo"));
        assert_eq!(reference.chat_author("f1"), None, "file cards are not chat messages");
        assert!(reference.has_reaction("f1", "❤️", "ana"));
        assert!(!reference.has_reaction("m1", "👍", "bo"), "bo's latest toggle is off");
    }

    #[test]
    fn test_union_is_symmetric() {
        let all = corpus();
        let (a_part, b_part) = all.split_at(all.len() / 2);
        let mut ab = ChatLog::new();
        merge_all(&mut ab, a_part);
        merge_all(&mut ab, b_part);
        let mut ba = ChatLog::new();
        merge_all(&mut ba, b_part);
        merge_all(&mut ba, a_part);
        assert_eq!(snapshot(&ab), snapshot(&ba));
    }

    #[test]
    fn test_merge_reports_what_changed() {
        let mut log = ChatLog::new();
        assert_eq!(log.merge(&react("r", "bo", NOW, "m", "👍", true), NOW), Merge::Pending);
        assert_eq!(log.merge(&react("r", "bo", NOW, "m", "👍", true), NOW), Merge::Ignored(Ignored::Held));
        assert_eq!(log.merge(&chat("m", "ana", NOW - 10, "hi"), NOW), Merge::Added(vec![Effect::Message("m".into())]));
        assert_eq!(log.tally("m"), vec![("👍".to_string(), vec!["bo".to_string()])], "the waiting reaction attached");
        assert_eq!(log.merge(&chat("m", "ana", NOW - 10, "hi"), NOW), Merge::Ignored(Ignored::Held));
        assert_eq!(log.merge(&edit("e", "ana", NOW, "m", "hey"), NOW), Merge::Added(vec![Effect::Edited("m".into())]));
        assert_eq!(log.merge(&edit("e0", "ana", NOW - 5, "m", "old"), NOW), Merge::Ignored(Ignored::Superseded));
        assert_eq!(log.merge(&edit("ex", "bo", NOW + 1, "m", "x"), NOW), Merge::Ignored(Ignored::NotAuthor));
        assert_eq!(log.merge(&delete("dx", "bo", NOW + 1, "m"), NOW), Merge::Ignored(Ignored::NotAuthor));
        assert_eq!(log.merge(&delete("d", "ana", NOW + 1, "m"), NOW), Merge::Added(vec![Effect::Deleted("m".into())]));
        assert_eq!(log.merge(&chat("m", "ana", NOW - 10, "hi"), NOW), Merge::Ignored(Ignored::Dropped), "can't come back");
        assert_eq!(log.merge(&hello("h", "ana", NOW, "Ana"), NOW), Merge::Added(vec![Effect::Named("ana".into())]));
        assert_eq!(log.merge(&env("t", "ana", NOW, RoomBody::Typing), NOW), Merge::Ignored(Ignored::NotLogged));
        assert_eq!(log.merge(&react("rz", "bo", NOW, "m2", "💩", true), NOW), Merge::Ignored(Ignored::BadReaction));
        assert_eq!(log.merge(&chat("m9", "bo", NOW, "x"), NOW), Merge::Added(vec![Effect::Message("m9".into())]));
        let squat = offer("o9", "cy", NOW, "m9");
        assert_eq!(log.merge(&squat, NOW), Merge::Ignored(Ignored::Conflict), "first root held wins");
    }

    #[test]
    fn test_tombstones_in_either_order() {
        // Delete before the message: the message never shows.
        let mut log = ChatLog::new();
        assert_eq!(log.merge(&delete("d", "ana", NOW, "m"), NOW), Merge::Added(vec![]));
        assert_eq!(log.merge(&react("r", "bo", NOW, "m", "👍", true), NOW), Merge::Pending);
        assert_eq!(log.merge(&chat("m", "ana", NOW - 5, "secret"), NOW), Merge::Ignored(Ignored::Deleted));
        assert!(log.view("m").is_none());
        assert!(!log.wants("r"), "the waiting reaction went with it");
        assert!(log.contains("d"));

        // A forged tombstone is dropped once the real author's message arrives.
        let mut log = ChatLog::new();
        log.merge(&delete("dx", "bo", NOW, "m"), NOW);
        assert!(log.contains("dx"));
        assert_eq!(log.merge(&chat("m", "ana", NOW - 5, "hi"), NOW), Merge::Added(vec![Effect::Message("m".into())]));
        assert!(!log.contains("dx"));
        assert!(!log.wants("dx"));
    }

    #[test]
    fn test_withdrawal_and_reactions_on_file_cards() {
        let mut log = ChatLog::new();
        assert_eq!(log.merge(&withdraw("w", "bo", NOW, "f"), NOW), Merge::Pending);
        assert_eq!(log.merge(&withdraw("wx", "ana", NOW, "f"), NOW), Merge::Pending);
        log.merge(&offer("o", "bo", NOW - 5, "f"), NOW);
        assert!(log.view("f").unwrap().withdrawn);
        assert!(!log.contains("wx") && !log.wants("wx"), "only the author withdraws");
        assert_eq!(log.merge(&edit("e", "bo", NOW, "f", "x"), NOW), Merge::Ignored(Ignored::NotAuthor), "cards aren't edited");
        assert_eq!(log.merge(&delete("d", "bo", NOW, "f"), NOW), Merge::Ignored(Ignored::NotAuthor), "nor deleted");
        assert_eq!(log.file_offers().len(), 1);
    }

    #[test]
    fn test_future_envelopes_wait_for_the_clock() {
        let mut log = ChatLog::new();
        let ahead = chat("m", "ana", NOW + MAX_FUTURE_SKEW_MS + 1, "early");
        assert_eq!(log.merge(&ahead, NOW), Merge::Ignored(Ignored::Future));
        assert!(log.wants("m"), "not refused for good");
        assert!(matches!(log.merge(&ahead, NOW + 2), Merge::Added(_)));
        assert!(matches!(log.merge(&chat("n", "ana", NOW + MAX_FUTURE_SKEW_MS, "ok"), NOW), Merge::Added(_)));
    }

    #[test]
    fn test_limits() {
        let mut log = ChatLog::new();
        let long_id = "x".repeat(MAX_ID_CHARS + 1);
        assert_eq!(log.merge(&chat(&long_id, "ana", NOW, "hi"), NOW), Merge::Ignored(Ignored::TooLarge));
        assert!(!log.wants(&long_id));
        let huge = chat("big", "ana", NOW, &"x".repeat(MAX_ENTRY_BYTES));
        assert_eq!(log.merge(&huge, NOW), Merge::Ignored(Ignored::TooLarge));
        let long_target = react("r", "ana", NOW, &"t".repeat(MAX_ID_CHARS + 1), "👍", true);
        assert_eq!(log.merge(&long_target, NOW), Merge::Ignored(Ignored::TooLarge));
        for body in [
            RoomBody::Typing,
            RoomBody::Leave,
            RoomBody::FileCancel { to: Some("b".into()), file_id: "f".into() },
            RoomBody::Dm { sealed: crate::crypto::EncryptedPayload { nonce: "n".into(), ciphertext: "c".into() } },
            RoomBody::SyncWant { to: "b".into(), ids: vec![] },
        ] {
            assert_eq!(log.merge(&env("x", "ana", NOW, body), NOW), Merge::Ignored(Ignored::NotLogged));
        }
        assert!(log.is_empty());
    }

    #[test]
    fn test_budget_keeps_the_newest_threads_whatever_the_order() {
        let entry_bytes = serde_json::to_vec(&chat("m00", "ana", NOW, "message")).unwrap().len();
        let all: Vec<RoomEnvelope> = (0..10)
            .flat_map(|i| {
                let id = format!("m{i:02}");
                let ts = NOW - 10_000 + i * 100;
                vec![chat(&id, "ana", ts, "message"), react(&format!("r{i:02}"), "bo", ts + 1, &id, "👍", true)]
            })
            .collect();
        let budget = entry_bytes * 8;
        let mut reference = ChatLog::with_budget(budget);
        merge_all(&mut reference, &all);
        assert!(reference.bytes() <= budget);
        assert!(reference.view("m09").is_some() && reference.view("m00").is_none());
        // Reactions leave with their message.
        assert!(!reference.contains("r00"));
        let horizon = reference.horizon().unwrap();
        assert_eq!(
            reference.merge(&chat("old", "cy", horizon - 1, "late"), NOW),
            Merge::Ignored(Ignored::BelowHorizon)
        );

        let mut rng = StdRng::seed_from_u64(11);
        for _ in 0..10 {
            let mut shuffled = all.clone();
            shuffled.shuffle(&mut rng);
            let mut log = ChatLog::with_budget(budget);
            merge_all(&mut log, &shuffled);
            assert_eq!(log.roots(), reference.roots());
            assert_eq!(log.horizon(), reference.horizon());
        }
    }

    #[test]
    fn test_digests_follow_inserts_and_removals() {
        let mut log = ChatLog::new();
        log.merge(&chat("a", "ana", NOW, "x"), NOW);
        log.merge(&chat("b", "ana", NOW + 1, "y"), NOW);
        let day = NOW - NOW % SYNC_SPANS[0];
        assert_eq!(log.window(0, day), Some((2, entry_fingerprint("a") ^ entry_fingerprint("b"))));
        log.merge(&delete("d", "ana", NOW + 2, "b"), NOW);
        assert_eq!(log.window(0, day), Some((2, entry_fingerprint("a") ^ entry_fingerprint("d"))));
        assert_eq!(log.summary().len(), 1);
        log.clear();
        assert!(log.summary().is_empty() && log.window(0, day).is_none());
    }

    /// Pull everything `puller` lacks from `source`, as the session does over a link;
    /// returns how many diff frames and batches crossed.
    fn pull(puller: &mut ChatLog, source: &ChatLog) -> (usize, usize) {
        let (mut round, horizon, windows) = PullRound::start(puller);
        let mut answers: VecDeque<DiffFrame> = source.answer_summary(&windows, horizon).into();
        let (mut frames_seen, mut batches_seen) = (0, 0);
        let mut steps: VecDeque<PullStep> = VecDeque::new();
        let mut hellos_sent = HashSet::new();
        loop {
            if let Some(frame) = answers.pop_front() {
                frames_seen += 1;
                steps.extend(round.on_diff(puller, &frame.ids, frame.level, &frame.windows, frame.last));
            } else if let Some(step) = steps.pop_front() {
                match step {
                    PullStep::Ask { level, starts } => answers.extend(source.answer_ask(level, &starts, horizon)),
                    PullStep::Want(ids) => {
                        let batches = source.batches(&ids, 32 * 1024, &mut hellos_sent);
                        let count = batches.len().max(1);
                        let mut batches = batches.into_iter();
                        for i in 0..count {
                            batches_seen += 1;
                            for e in batches.next().unwrap_or_default() {
                                assert!(round.accepts(&e), "only what was asked for");
                                puller.merge(&e, NOW);
                            }
                            steps.extend(round.on_batch(i + 1 == count));
                        }
                    }
                }
            } else {
                break;
            }
        }
        assert!(round.is_done());
        (frames_seen, batches_seen)
    }

    #[test]
    fn test_reconcile_converges_both_ways() {
        let all = corpus();
        let mut a = ChatLog::new();
        let mut b = ChatLog::new();
        merge_all(&mut a, &all[..14]);
        merge_all(&mut b, &all[7..]);
        merge_all(&mut b, &[chat("only-b", "cy", NOW - 50_000_000, "yesterday")]);
        pull(&mut b, &a);
        pull(&mut a, &b);
        assert_eq!(snapshot(&a), snapshot(&b));
        assert!(a.view("only-b").is_some());
        assert_eq!(a.hello("bo").map(|h| h.id.as_str()), Some("h-bo"), "Hellos come along");

        // Already in sync: one empty answer, nothing fetched.
        assert_eq!(pull(&mut a, &b), (1, 0));
    }

    #[test]
    fn test_reconcile_drills_down_large_windows() {
        let day = NOW - NOW % SYNC_SPANS[0];
        let mut source = ChatLog::new();
        // 300 messages in one hour (over the list limit), spread over 5 minutes, plus 50 in
        // another hour.
        let busy: Vec<RoomEnvelope> =
            (0..300).map(|i| chat(&format!("busy{i:03}"), "ana", day + 3_600_000 + (i % 5) * 60_000 + i, "x")).collect();
        let quiet: Vec<RoomEnvelope> =
            (0..50).map(|i| chat(&format!("quiet{i:02}"), "bo", day + 7_200_000 + i, "y")).collect();
        merge_all(&mut source, &busy);
        merge_all(&mut source, &quiet);
        let mut puller = ChatLog::new();
        merge_all(&mut puller, &busy[..280]);
        pull(&mut puller, &source);
        assert_eq!(snapshot(&puller), snapshot(&source));
    }

    #[test]
    fn test_answers_skip_what_the_puller_dropped() {
        let mut source = ChatLog::new();
        merge_all(&mut source, &[chat("old", "ana", NOW - 100_000_000, "x"), chat("new", "ana", NOW, "y")]);
        let frames = source.answer_summary(&[], Some(NOW - 1000));
        let ids: Vec<&String> = frames.iter().flat_map(|f| f.ids.iter()).collect();
        assert_eq!(ids, vec!["new"]);
    }

    #[test]
    fn test_batches_put_hellos_and_roots_first() {
        let mut log = ChatLog::new();
        merge_all(&mut log, &corpus());
        let ids: Vec<String> = log.entries.keys().cloned().collect();
        let mut sent = HashSet::new();
        let batches = log.batches(&ids, 300, &mut sent);
        assert!(batches.len() > 1, "small batches");
        let flat: Vec<RoomEnvelope> = batches.into_iter().flatten().collect();
        let position = |id: &str| flat.iter().position(|e| e.id == id).unwrap();
        assert!(position("m1") < position("e2") && position("m1") < position("r3"));
        assert!(position("h-ana2") < position("m1"));
        assert!(position("o2") < position("m1"), "newest threads first");
        let mut again = log.batches(&ids, usize::MAX, &mut sent).concat();
        again.retain(|e| matches!(e.body, RoomBody::Hello { .. }));
        assert!(again.is_empty(), "each Hello once per serve");

        // Merged in batch order, nothing waits for its root.
        let mut fresh = ChatLog::new();
        for e in &flat {
            assert_ne!(fresh.merge(e, NOW), Merge::Pending, "{}", e.id);
        }
    }

    #[test]
    fn test_sync_queue_pulls_one_peer_at_a_time() {
        let mut q = SyncQueue::default();
        q.enqueue("a", 0);
        q.enqueue("b", 0);
        q.enqueue("a", 0);
        assert_eq!(q.next(0).as_deref(), Some("a"));
        assert_eq!(q.next(0), None, "one active");
        q.enqueue("a", 0);
        q.finish();
        assert_eq!(q.next(0).as_deref(), Some("b"));
        q.defer("b", 100);
        assert_eq!(q.active(), None);
        assert_eq!(q.next(50), None, "not ready yet");
        assert_eq!(q.next(100).as_deref(), Some("b"));
        q.remove("b");
        assert_eq!(q.active(), None);
        assert_eq!(q.next(1000), None);
    }
}
