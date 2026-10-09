//! Parallel file transfer: one upload can use the main link's file channel and extra file
//! links (separate `RTCPeerConnection`s between the same two members, chunks only).
//!
//! Why: one WebRTC connection's SCTP congestion control halves its window on every lost
//! packet and grows it back once per round trip, so over the internet one connection fills
//! only a few MB/s of a much faster line. Each extra connection has its own window, like the
//! parallel TCP streams speed tests use.
//!
//! Rules every member applies alike:
//! - The downloader confirms progress (every chunk before `next` arrived) at least every
//!   [`ACK_EVERY_CHUNKS`] chunks or [`ACK_EVERY_MS`] ([`AckPacer`]), over the main link's
//!   file channel. An acknowledgement is a chunk packet (`encrypt_chunk`) about the author's
//!   own file, with `chunk_index = next` and no data: chunks only ever go to downloaders,
//!   so a packet about a file we offered can only be one. No signature: like chunks, it is
//!   trusted for the link it arrives on, and it only steers the sender's transfer to its author.
//!   They are rare on purpose: packets flowing back on the connection slow its SCTP down
//!   (one every 16 chunks cost a 20 MB/s transfer about a third of its speed in Chrome).
//! - The author sends chunk `i` only while `i < next + SEND_WINDOW_CHUNKS` ([`in_send_window`]).
//! - The downloader holds chunks that arrive early (over another connection) until the gap
//!   before them fills ([`Reorder`]), and drops stale ones and anything past the window.
//! - Each member dials at most [`MAX_FILE_CONNECTIONS`] − 1 file links to another; more are refused.
//!
//! How many connections an upload uses is the author's choice ([`LinkGrowth`]): it starts on
//! the main link and adds one at a time while each addition raises the rate by at least
//! [`MIN_GAIN`], up to the author's own cap. The rate is what the connections take from the
//! author's send queues: in step with what the network carries, and much finer than the
//! acknowledgements.

use std::collections::{BTreeMap, VecDeque};

/// Connections one upload may use: the main link and up to 7 file links.
pub const MAX_FILE_CONNECTIONS: usize = 8;
/// Chunks the author may send past the last confirmed one (32 MB of 64 KB chunks). It also
/// bounds what the downloader holds out of order.
pub const SEND_WINDOW_CHUNKS: u32 = 512;
/// The downloader confirms progress every this many chunks (8 MB)...
pub const ACK_EVERY_CHUNKS: u32 = 128;
/// ...or this often while chunks arrive.
pub const ACK_EVERY_MS: f64 = 1000.0;

/// The rate is measured over this much time.
pub const RATE_WINDOW_MS: f64 = 2000.0;
/// First look at the rate this long after the upload starts, then once a second until it
/// stops rising by more than [`MIN_GAIN`] (the main link's ramp-up must not look like a
/// gain from the first new link)...
const FIRST_CHECK_MS: f64 = 2000.0;
const CHECK_EVERY_MS: f64 = 1000.0;
/// ...but no longer than this.
const WARMUP_MAX_MS: f64 = 8000.0;
/// A new link is judged this long after it opens (its ramp-up, then a rate window).
const SETTLE_MS: f64 = 3000.0;
/// A link that has not opened by then ends the growth.
const OPEN_TIMEOUT_MS: f64 = 8000.0;
/// A new link stays only if the rate rose by more than this much (10 %).
pub const MIN_GAIN: f64 = 0.10;
/// No new link for less than this much left...
const MIN_GROW_REMAINING: u64 = 4 * 1024 * 1024;
/// ...or for less than this many seconds of transfer at the current rate.
const GROW_HORIZON_S: f64 = 5.0;

/// Author side: whether chunk `index` may go out while `acked` chunks are confirmed.
pub fn in_send_window(index: u32, acked: u32) -> bool {
    index < acked.saturating_add(SEND_WINDOW_CHUNKS)
}

/// Downloader side: puts chunks that arrive over several connections back in order.
#[derive(Debug)]
pub struct Reorder<T> {
    next: u32,
    ahead: BTreeMap<u32, T>,
}

impl<T> Default for Reorder<T> {
    fn default() -> Self {
        Self { next: 0, ahead: BTreeMap::new() }
    }
}

impl<T> Reorder<T> {
    /// Index of the first chunk not yet delivered in order.
    pub fn next(&self) -> u32 {
        self.next
    }

    /// Chunks held because one before them is missing.
    pub fn held(&self) -> usize {
        self.ahead.len()
    }

    /// Take chunk `index`; returns the chunks now in order: this one and the held ones right
    /// after it. Stale chunks (`index < next`), duplicates and chunks past the send window
    /// are dropped.
    pub fn accept(&mut self, index: u32, chunk: T) -> Vec<T> {
        if index < self.next || index - self.next >= SEND_WINDOW_CHUNKS {
            return Vec::new();
        }
        if index != self.next {
            self.ahead.entry(index).or_insert(chunk);
            return Vec::new();
        }
        let mut ready = vec![chunk];
        self.next = self.next.saturating_add(1);
        while let Some(chunk) = self.ahead.remove(&self.next) {
            ready.push(chunk);
            self.next = self.next.saturating_add(1);
        }
        ready
    }
}

/// Downloader side: when to confirm progress to the author.
#[derive(Debug, Default)]
pub struct AckPacer {
    acked: u32,
    at_ms: f64,
}

impl AckPacer {
    /// Whether to confirm `next` now; `done` once the last chunk arrived.
    pub fn due(&mut self, now_ms: f64, next: u32, done: bool) -> bool {
        let due = next > self.acked
            && (done || next - self.acked >= ACK_EVERY_CHUNKS || now_ms - self.at_ms >= ACK_EVERY_MS);
        if due {
            self.acked = next;
            self.at_ms = now_ms;
        }
        due
    }
}

/// Bytes per second over a sliding window (by default [`RATE_WINDOW_MS`]), from a running
/// byte count.
#[derive(Debug)]
pub struct RateMeter {
    window_ms: f64,
    samples: VecDeque<(f64, u64)>,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new(RATE_WINDOW_MS)
    }
}

impl RateMeter {
    pub fn new(window_ms: f64) -> Self {
        Self { window_ms, samples: VecDeque::new() }
    }

    /// Record the running total `bytes` at `now_ms`.
    pub fn record(&mut self, now_ms: f64, bytes: u64) {
        let len = self.samples.len();
        match self.samples.back_mut() {
            // Samples closer than 50 ms apart merge, so the queue stays short.
            Some(last) if len > 1 && now_ms - last.0 < 50.0 => *last = (now_ms, bytes),
            _ => self.samples.push_back((now_ms, bytes)),
        }
        // Keep one sample at or before the window's start as its anchor.
        while self.samples.len() > 2 && self.samples[1].0 <= now_ms - self.window_ms {
            self.samples.pop_front();
        }
    }

    /// `None` until half a window has been seen.
    pub fn rate(&self) -> Option<f64> {
        let (first, last) = (self.samples.front()?, self.samples.back()?);
        let span = last.0 - first.0;
        (span >= self.window_ms / 2.0).then(|| last.1.saturating_sub(first.1) as f64 * 1000.0 / span)
    }
}

/// What [`LinkGrowth`] asks of the author.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Growth {
    Hold,
    /// Open one more file link (or bring back one set aside).
    AddLink,
    /// The newest link did not help: stop sending on it.
    DropLink,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Probe {
    /// The rate before the link was added.
    baseline: f64,
    /// Connections once it opened.
    connections: usize,
    /// Measured once already without a gain: one loss on any connection can hide it, so a
    /// link gets a second look before it goes.
    second_look: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting to judge the link just added (`probe`), or before the first one for the main
    /// link's rate to settle; then deciding on the next one.
    Measuring { since: f64, check_at: f64, prev: Option<f64>, probe: Option<Probe> },
    Opening { baseline: f64, connections: usize, since: f64 },
    Done,
}

/// Author side, one per upload: how many connections to use. Starts on what is open (the
/// main link, plus file links kept from an earlier upload to the same member) and adds one
/// link at a time while each raises the settled rate by more than [`MIN_GAIN`], up to `cap`.
#[derive(Debug)]
pub struct LinkGrowth {
    cap: usize,
    meter: RateMeter,
    phase: Phase,
}

impl LinkGrowth {
    /// `cap`: most connections to use, the main link included (1 = the main link only).
    pub fn new(cap: usize, now_ms: f64) -> Self {
        Self {
            cap: cap.clamp(1, MAX_FILE_CONNECTIONS),
            meter: RateMeter::default(),
            phase: Phase::Measuring { since: now_ms, check_at: now_ms + FIRST_CHECK_MS, prev: None, probe: None },
        }
    }

    /// Bytes per second lately, once known.
    pub fn rate(&self) -> Option<f64> {
        self.meter.rate()
    }

    /// `sent`: bytes the connections took from our send queues so far; `connections`: open
    /// connections carrying this upload (the main link included); `remaining`: bytes left.
    pub fn update(&mut self, now_ms: f64, sent: u64, connections: usize, remaining: u64) -> Growth {
        self.meter.record(now_ms, sent);
        match self.phase {
            Phase::Measuring { since, check_at, prev, probe } => {
                // A link dropped meanwhile: the comparison would mean nothing.
                if probe.is_some_and(|p| connections < p.connections) {
                    self.phase = Phase::Done;
                    return Growth::Hold;
                }
                let Some(rate) = self.meter.rate().filter(|_| now_ms >= check_at) else {
                    return Growth::Hold;
                };
                let rising = prev.is_none_or(|prev| rate > prev * (1.0 + MIN_GAIN));
                if probe.is_none() && rising && now_ms - since < WARMUP_MAX_MS {
                    self.phase = Phase::Measuring { since, check_at: now_ms + CHECK_EVERY_MS, prev: Some(rate), probe };
                    return Growth::Hold;
                }
                if let Some(p) = probe.filter(|p| rate <= p.baseline * (1.0 + MIN_GAIN)) {
                    if p.second_look {
                        self.phase = Phase::Done;
                        return Growth::DropLink;
                    }
                    let probe = Some(Probe { second_look: true, ..p });
                    self.phase = Phase::Measuring { since: now_ms, check_at: now_ms + RATE_WINDOW_MS, prev: Some(rate), probe };
                    return Growth::Hold;
                }
                let worth = remaining >= MIN_GROW_REMAINING && remaining as f64 >= rate * GROW_HORIZON_S;
                if connections >= self.cap || !worth {
                    self.phase = Phase::Done;
                    return Growth::Hold;
                }
                self.phase = Phase::Opening { baseline: rate, connections, since: now_ms };
                Growth::AddLink
            }
            Phase::Opening { baseline, connections: before, since } => {
                if connections > before {
                    let probe = Some(Probe { baseline, connections, second_look: false });
                    self.phase = Phase::Measuring { since: now_ms, check_at: now_ms + SETTLE_MS, prev: None, probe };
                } else if now_ms - since >= OPEN_TIMEOUT_MS {
                    self.phase = Phase::Done;
                }
                Growth::Hold
            }
            Phase::Done => Growth::Hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: f64 = 1024.0 * 1024.0;

    #[test]
    fn test_reorder_releases_chunks_in_order() {
        let mut r = Reorder::default();
        assert_eq!(r.accept(0, 'a'), vec!['a']);
        assert_eq!(r.accept(2, 'c'), Vec::<char>::new());
        assert_eq!(r.accept(3, 'd'), Vec::<char>::new());
        assert_eq!(r.held(), 2);
        assert_eq!(r.accept(1, 'b'), vec!['b', 'c', 'd']);
        assert_eq!((r.next(), r.held()), (4, 0));
    }

    #[test]
    fn test_reorder_drops_stale_duplicate_and_far_chunks() {
        let mut r = Reorder::default();
        r.accept(0, 0);
        assert!(r.accept(0, 99).is_empty(), "stale");
        r.accept(5, 5);
        r.accept(5, 99);
        assert!(r.accept(1 + SEND_WINDOW_CHUNKS, 99).is_empty(), "past the window");
        assert_eq!(r.held(), 1);
        for i in 1..5 {
            r.accept(i, i);
        }
        assert_eq!(r.next(), 6, "the first copy of 5 was kept");
        // The window moves with `next`.
        assert!(r.accept(6 + SEND_WINDOW_CHUNKS - 1, 1).is_empty());
        assert_eq!(r.held(), 1);
    }

    #[test]
    fn test_send_window_matches_what_the_downloader_holds() {
        assert!(in_send_window(SEND_WINDOW_CHUNKS - 1, 0));
        assert!(!in_send_window(SEND_WINDOW_CHUNKS, 0));
        assert!(in_send_window(u32::MAX - 1, u32::MAX - 1));
        // Whatever the author may send, the downloader keeps.
        let mut r = Reorder::default();
        assert!(r.accept(SEND_WINDOW_CHUNKS - 1, ()).is_empty());
        assert_eq!(r.held(), 1);
    }

    #[test]
    fn test_acks_every_128_chunks_every_second_and_at_the_end() {
        let mut p = AckPacer::default();
        assert!(!p.due(0.0, 0, false), "nothing new");
        assert!(p.due(1000.0, 1, false), "time");
        assert!(!p.due(1100.0, 128, false));
        assert!(p.due(1200.0, 129, false), "128 more chunks");
        assert!(!p.due(1300.0, 130, false));
        assert!(p.due(1400.0, 131, true), "the last chunk");
        assert!(!p.due(9000.0, 131, true), "nothing new");
        // Between two acknowledgements, the author still has room to send.
        const { assert!(ACK_EVERY_CHUNKS * 2 < SEND_WINDOW_CHUNKS) };
    }

    #[test]
    fn test_rate_meter_measures_the_last_window() {
        let mut m = RateMeter::default();
        m.record(0.0, 0);
        assert_eq!(m.rate(), None);
        for t in 1..=100 {
            // 1 MB/s for 5 s, then 4 MB/s.
            let ms = t as f64 * 100.0;
            let bytes = if ms <= 5000.0 { ms / 1000.0 * MB } else { 5.0 * MB + (ms - 5000.0) / 1000.0 * 4.0 * MB };
            m.record(ms, bytes as u64);
        }
        let rate = m.rate().unwrap() / MB;
        assert!((rate - 4.0).abs() < 0.01, "{rate}");
        assert!(m.samples.len() <= 25);
        // A longer window still sees part of the slower start.
        let mut long = RateMeter::new(10_000.0);
        for t in 0..=100 {
            let ms = t as f64 * 100.0;
            let bytes = if ms <= 5000.0 { ms / 1000.0 * MB } else { 5.0 * MB + (ms - 5000.0) / 1000.0 * 4.0 * MB };
            long.record(ms, bytes as u64);
        }
        let rate = long.rate().unwrap() / MB;
        assert!((rate - 2.5).abs() < 0.01, "{rate}");
    }

    /// Run the controller for `seconds` against `rate_at(t_s, connections)` MB/s, links
    /// opening 500 ms after they are asked for, on a large file. Returns the connections in
    /// use at the end and each decision with its time.
    fn simulate(cap: usize, seconds: f64, rate_at: impl Fn(f64, usize) -> f64) -> (usize, Vec<(f64, Growth)>) {
        let mut growth = LinkGrowth::new(cap, 0.0);
        let mut connections = 1usize;
        let mut opening_at: Option<f64> = None;
        let mut sent = 0.0;
        let mut decisions = Vec::new();
        let mut t = 0.0;
        while t < seconds * 1000.0 {
            t += 100.0;
            if opening_at.is_some_and(|at| t >= at) {
                connections += 1;
                opening_at = None;
            }
            sent += rate_at(t / 1000.0, connections) * MB / 10.0;
            let decision = growth.update(t, sent as u64, connections, u64::MAX / 2);
            match decision {
                Growth::Hold => continue,
                Growth::AddLink => opening_at = Some(t + 500.0),
                Growth::DropLink => connections -= 1,
            }
            decisions.push((t / 1000.0, decision));
        }
        (connections, decisions)
    }

    /// Each connection carries `per_link` MB/s, up to `line` MB/s in all.
    fn steady(per_link: f64, line: f64) -> impl Fn(f64, usize) -> f64 {
        move |_, connections| (per_link * connections as f64).min(line)
    }

    fn kinds(decisions: &[(f64, Growth)]) -> Vec<Growth> {
        decisions.iter().map(|d| d.1).collect()
    }

    #[test]
    fn test_growth_adds_links_until_the_line_is_full() {
        // 5 MB/s per connection on a 30 MB/s line: six connections fill it; a seventh is
        // tried, does not help and is dropped.
        let (connections, decisions) = simulate(8, 60.0, steady(5.0, 30.0));
        assert_eq!(connections, 6);
        assert_eq!(kinds(&decisions).iter().filter(|d| **d == Growth::AddLink).count(), 6);
        assert_eq!(decisions.last().map(|d| d.1), Some(Growth::DropLink));
        // The first decision waits for a settled rate: two looks, 1 s apart.
        assert_eq!(decisions[0].0, 3.0);
    }

    #[test]
    fn test_growth_stops_at_the_cap() {
        assert_eq!(simulate(4, 60.0, steady(5.0, 30.0)).0, 4);
        assert_eq!(simulate(1, 60.0, steady(5.0, 30.0)), (1, vec![]), "cap 1: the main link only");
        assert_eq!(LinkGrowth::new(50, 0.0).cap, MAX_FILE_CONNECTIONS);
        // Each new link must add more than 10 %: the eighth would add 1/7.
        assert_eq!(simulate(8, 120.0, steady(1.0, 100.0)).0, MAX_FILE_CONNECTIONS);
        assert_eq!(simulate(8, 120.0, steady(1.0, 7.5)).0, 7);
    }

    #[test]
    fn test_growth_tries_once_when_one_connection_fills_the_line() {
        let (connections, decisions) = simulate(8, 30.0, steady(20.0, 20.0));
        assert_eq!(connections, 1);
        assert_eq!(kinds(&decisions), vec![Growth::AddLink, Growth::DropLink]);
    }

    #[test]
    fn test_a_link_hidden_by_a_dip_gets_a_second_look() {
        // The second link's first seconds coincide with a loss on the main link: no gain
        // shows at first, then it does.
        let dip = |t: f64, connections: usize| match connections {
            1 => 5.0,
            _ if t < 8.0 => 5.0,
            n => 5.0 * n as f64,
        };
        let (connections, decisions) = simulate(2, 40.0, dip);
        assert_eq!((connections, kinds(&decisions)), (2, vec![Growth::AddLink]), "{decisions:?}");
    }

    #[test]
    fn test_a_ramp_up_is_not_taken_for_a_gain() {
        // The main link ramps up to the full 20 MB/s line over 6 s: nothing to gain from a
        // second connection, and the ramp must not look like one.
        let ramp = |t: f64, connections: usize| (20.0 * connections as f64).min(20.0) * (t / 6.0).min(1.0);
        let (connections, decisions) = simulate(8, 40.0, ramp);
        assert_eq!(connections, 1);
        assert_eq!(kinds(&decisions), vec![Growth::AddLink, Growth::DropLink]);
        assert!(decisions[0].0 >= 6.0, "{decisions:?}");
        // A new link that stalls before it helps is judged once it has settled.
        let stall = |t: f64, connections: usize| match connections {
            1 => 5.0,
            _ if t < 7.0 => 2.0,
            n => 5.0 * n as f64,
        };
        let (connections, decisions) = simulate(2, 40.0, stall);
        assert_eq!((connections, kinds(&decisions)), (2, vec![Growth::AddLink]), "{decisions:?}");
    }

    #[test]
    fn test_no_growth_during_warmup_or_near_the_end() {
        let mut g = LinkGrowth::new(8, 0.0);
        for t in 1..30 {
            assert_eq!(g.update(t as f64 * 100.0, t * 100_000, 1, u64::MAX), Growth::Hold, "warmup");
        }
        // 1 MB/s with 3 MB left: not worth a new link.
        let mut g = LinkGrowth::new(8, 0.0);
        for t in 1..=60 {
            let decision = g.update(t as f64 * 100.0, t * MB as u64 / 10, 1, 3 * MB as u64);
            assert_eq!(decision, Growth::Hold);
        }
        assert_eq!(g.phase, Phase::Done);
    }

    #[test]
    fn test_growth_ends_when_a_link_never_opens_or_one_drops() {
        let mut g = LinkGrowth::new(8, 0.0);
        let mut t = 0.0;
        let mut step = |g: &mut LinkGrowth, connections| {
            t += 100.0;
            g.update(t, (t * 1000.0) as u64, connections, u64::MAX / 2)
        };
        while step(&mut g, 1) != Growth::AddLink {}
        for _ in 0..80 {
            assert_eq!(step(&mut g, 1), Growth::Hold);
        }
        assert_eq!(g.phase, Phase::Done);

        let mut g = LinkGrowth::new(8, 0.0);
        while step(&mut g, 1) != Growth::AddLink {}
        step(&mut g, 2);
        assert!(matches!(g.phase, Phase::Measuring { probe: Some(_), .. }));
        assert_eq!(step(&mut g, 1), Growth::Hold);
        assert_eq!(g.phase, Phase::Done);
    }
}
