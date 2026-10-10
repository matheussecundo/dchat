//! History sync. On every new link both members pull what they lack from the other's chat
//! log (`protocol::chat_log`): a summary of day windows, asks down to hours and minutes where
//! the logs differ, then the missing entries in batches. A member pulls from one peer at a
//! time (the others wait their turn and then find little left to fetch), and a member serving
//! a pull sends only while the chat channel has room, so live messages slip in between.
//! Every sync message is direct-only; every synced envelope is verified before it is merged.

use super::history::Mode;
use super::RoomSession;
use crate::names::pubkey_tag;
use leptos::prelude::*;
use protocol::chat_log::{DiffFrame, Merge, PullRound, PullStep, SyncQueue, SYNC_WANT_MAX};
use protocol::{RoomBody, RoomEnvelope, SyncWindow};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Sync traffic waits while this much is queued on the chat channel.
const SYNC_HIGH_WATER: u32 = 256 * 1024;
/// Envelope bytes per batch (a larger envelope goes alone).
const SYNC_BATCH_BYTES: usize = 32 * 1024;
/// A pull that heard nothing for this long is set aside and tried again later.
const SYNC_STALL_MS: f64 = 20_000.0;
const SYNC_RETRY_MS: f64 = 5_000.0;

struct Active {
    peer: String,
    round: PullRound,
    last_heard: f64,
}

#[derive(Default)]
pub(super) struct SyncState {
    queue: RefCell<SyncQueue>,
    active: RefCell<Option<Active>>,
    /// Peers we serve: the authors whose Hello they already got in their current round.
    hellos_sent: RefCell<HashMap<String, HashSet<String>>>,
    /// Peers whose want is being served now (an honest puller has one in flight at a time).
    serving: RefCell<HashSet<String>>,
    #[cfg(feature = "e2e-hooks")]
    stats: RefCell<(usize, usize, usize)>,
    #[cfg(feature = "e2e-hooks")]
    throttle_ms: std::cell::Cell<i32>,
}

fn now() -> f64 {
    js_sys::Date::now()
}

impl RoomSession {
    // ---- Puller -----------------------------------------------------------------------

    /// A link opened: pull from that member when its turn comes.
    pub(super) fn sync_link_opened(&self, peer: &str) {
        self.inner.sync.queue.borrow_mut().enqueue(peer, now() as u64);
        self.sync_pump();
    }

    /// The link to `peer` is gone: its pull (if running) ends with what arrived.
    pub(super) fn sync_peer_lost(&self, peer: &str) {
        self.inner.sync.queue.borrow_mut().remove(peer);
        self.inner.sync.hellos_sent.borrow_mut().remove(peer);
        let was_active = self.inner.sync.active.borrow().as_ref().is_some_and(|a| a.peer == peer);
        if was_active {
            self.end_pull();
        }
    }

    pub(super) fn sync_tick(&self) {
        let stalled = self
            .inner
            .sync
            .active
            .borrow()
            .as_ref()
            .filter(|a| now() - a.last_heard > SYNC_STALL_MS)
            .map(|a| a.peer.clone());
        if let Some(peer) = stalled {
            log::info!("History sync with {} stalled; trying again later", pubkey_tag(&peer));
            *self.inner.sync.active.borrow_mut() = None;
            self.inner.sync.queue.borrow_mut().defer(&peer, (now() + SYNC_RETRY_MS) as u64);
            self.inner.signals.history_loading.set(false);
        }
        self.sync_pump();
    }

    /// Start the next pull, if none is running and a peer is waiting.
    fn sync_pump(&self) {
        if self.inner.closed.get() {
            return;
        }
        loop {
            let next = self.inner.sync.queue.borrow_mut().next(now() as u64);
            let Some(peer) = next else {
                return;
            };
            let (round, horizon, windows) = PullRound::start(&self.inner.log.borrow());
            *self.inner.sync.active.borrow_mut() = Some(Active { peer: peer.clone(), round, last_heard: now() });
            if self.send_direct(&peer, RoomBody::SyncSummary { to: peer.clone(), horizon, windows }) {
                return;
            }
            *self.inner.sync.active.borrow_mut() = None;
            self.inner.sync.queue.borrow_mut().remove(&peer);
        }
    }

    fn end_pull(&self) {
        *self.inner.sync.active.borrow_mut() = None;
        self.inner.sync.queue.borrow_mut().finish();
        self.inner.signals.history_loading.set(false);
        #[cfg(feature = "e2e-hooks")]
        {
            self.inner.sync.stats.borrow_mut().0 += 1;
        }
        self.sync_pump();
    }

    fn end_pull_if_done(&self) {
        let done = self.inner.sync.active.borrow().as_ref().is_some_and(|a| a.round.is_done());
        if done {
            self.end_pull();
        }
    }

    fn send_pull_steps(&self, peer: &str, steps: Vec<PullStep>) {
        let horizon = self.inner.log.borrow().horizon();
        for step in steps {
            let body = match step {
                PullStep::Ask { level, starts } => RoomBody::SyncAsk { to: peer.to_string(), horizon, level, starts },
                PullStep::Want(ids) => {
                    self.inner.signals.history_loading.set(true);
                    RoomBody::SyncWant { to: peer.to_string(), ids }
                }
            };
            self.send_direct(peer, body);
        }
    }

    pub(super) fn on_sync_diff(&self, from: &str, ids: &[String], level: u8, windows: &[SyncWindow], last: bool) {
        let steps = {
            let mut active = self.inner.sync.active.borrow_mut();
            let Some(active) = active.as_mut().filter(|a| a.peer == from) else {
                return;
            };
            active.last_heard = now();
            active.round.on_diff(&self.inner.log.borrow(), ids, level, windows, last)
        };
        self.send_pull_steps(from, steps);
        self.end_pull_if_done();
    }

    pub(super) fn on_sync_batch(&self, from: &str, envelopes: &[RoomEnvelope], last: bool) {
        let accepted: Vec<&RoomEnvelope> = {
            let mut active = self.inner.sync.active.borrow_mut();
            let Some(active) = active.as_mut().filter(|a| a.peer == from) else {
                return;
            };
            active.last_heard = now();
            envelopes.iter().filter(|e| active.round.accepts(e)).collect()
        };
        let mut effects = Vec::new();
        for envelope in accepted {
            // Signed by its author for this room (`adm`), whoever relays it.
            if !envelope.verify(self.adm()) {
                log::warn!("Dropping a synced message with an invalid signature");
                continue;
            }
            if let Merge::Added(found) = self.log_merge(envelope) {
                effects.extend(found);
                #[cfg(feature = "e2e-hooks")]
                {
                    self.inner.sync.stats.borrow_mut().1 += 1;
                }
            }
        }
        self.show_effects(effects, Mode::History);
        let step = {
            let mut active = self.inner.sync.active.borrow_mut();
            active.as_mut().filter(|a| a.peer == from).and_then(|a| a.round.on_batch(last))
        };
        self.send_pull_steps(from, step.into_iter().collect());
        self.end_pull_if_done();
    }

    // ---- Responder --------------------------------------------------------------------

    pub(super) fn on_sync_summary(&self, from: &str, horizon: Option<u64>, windows: &[SyncWindow]) {
        self.inner.sync.hellos_sent.borrow_mut().insert(from.to_string(), HashSet::new());
        let frames = self.inner.log.borrow().answer_summary(windows, horizon);
        self.send_diff_frames(from, frames);
    }

    pub(super) fn on_sync_ask(&self, from: &str, horizon: Option<u64>, level: u8, starts: &[u64]) {
        let frames = self.inner.log.borrow().answer_ask(level, starts, horizon);
        self.send_diff_frames(from, frames);
    }

    fn send_diff_frames(&self, to: &str, frames: Vec<DiffFrame>) {
        let bodies = frames
            .into_iter()
            .map(|f| RoomBody::SyncDiff { to: to.to_string(), ids: f.ids, level: f.level, windows: f.windows, last: f.last })
            .collect();
        self.send_paced(to, bodies, false);
    }

    pub(super) fn on_sync_want(&self, from: &str, ids: &[String]) {
        if ids.len() > SYNC_WANT_MAX || !self.inner.sync.serving.borrow_mut().insert(from.to_string()) {
            return;
        }
        #[cfg(feature = "e2e-hooks")]
        let max_bytes = if self.inner.sync.throttle_ms.get() > 0 { 1 } else { SYNC_BATCH_BYTES };
        #[cfg(not(feature = "e2e-hooks"))]
        let max_bytes = SYNC_BATCH_BYTES;
        let batches = {
            let mut hellos_sent = self.inner.sync.hellos_sent.borrow_mut();
            let sent = hellos_sent.entry(from.to_string()).or_default();
            self.inner.log.borrow().batches(ids, max_bytes, sent)
        };
        #[cfg(feature = "e2e-hooks")]
        {
            self.inner.sync.stats.borrow_mut().2 += batches.iter().map(Vec::len).sum::<usize>();
        }
        let count = batches.len();
        let bodies: Vec<RoomBody> = if count == 0 {
            vec![RoomBody::SyncBatch { to: from.to_string(), envelopes: Vec::new(), last: true }]
        } else {
            batches
                .into_iter()
                .enumerate()
                .map(|(i, envelopes)| RoomBody::SyncBatch { to: from.to_string(), envelopes, last: i + 1 == count })
                .collect()
        };
        self.send_paced(from, bodies, true);
    }

    /// Send `bodies` to `to` in order, each once the chat channel has room. `serve`: this
    /// answers a want, and the next one is taken once it is all sent.
    fn send_paced(&self, to: &str, bodies: Vec<RoomBody>, serve: bool) {
        let Some(link) = self.link(to) else {
            if serve {
                self.inner.sync.serving.borrow_mut().remove(to);
            }
            return;
        };
        let s = self.clone();
        let to = to.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            for body in bodies {
                if s.inner.closed.get() || !link.wait_for_chat_room(SYNC_HIGH_WATER).await {
                    break;
                }
                #[cfg(feature = "e2e-hooks")]
                {
                    let delay = s.inner.sync.throttle_ms.get();
                    if delay > 0 && matches!(body, RoomBody::SyncBatch { .. }) {
                        crate::media::sleep_ms(delay).await;
                    }
                }
                if !s.send_direct(&to, body) {
                    break;
                }
            }
            if serve {
                s.inner.sync.serving.borrow_mut().remove(&to);
            }
        });
    }

    /// `window.__dchat` probes for the history specs (e2e builds only).
    #[cfg(feature = "e2e-hooks")]
    pub(super) fn install_sync_hooks(&self, hooks: &js_sys::Object) {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsValue;

        // Serve one envelope per batch, this many ms apart (0: normal).
        let s = self.clone();
        let throttle = Closure::wrap(Box::new(move |ms: i32| {
            s.inner.sync.throttle_ms.set(ms);
        }) as Box<dyn Fn(i32)>);
        let _ = js_sys::Reflect::set(hooks, &"throttleSync".into(), throttle.as_ref());
        throttle.forget();

        // `{rounds, pulled, served}`: pulls finished, envelopes merged from pulls, envelopes served.
        let s = self.clone();
        let stats = Closure::wrap(Box::new(move || {
            let (rounds, pulled, served) = *s.inner.sync.stats.borrow();
            JsValue::from_str(&serde_json::json!({ "rounds": rounds, "pulled": pulled, "served": served }).to_string())
        }) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(hooks, &"syncStats".into(), stats.as_ref());
        stats.forget();

        // The signed original of the logged chat message with this text (JSON), or "".
        let s = self.clone();
        let export = Closure::wrap(Box::new(move |text: String| {
            let log = s.inner.log.borrow();
            let found = log.roots().into_iter().find_map(|target| {
                let view = log.view(&target)?;
                matches!(&view.root.body, RoomBody::Chat { text: t } if *t == text).then(|| view.root.clone())
            });
            JsValue::from_str(&found.and_then(|e| serde_json::to_string(&e).ok()).unwrap_or_default())
        }) as Box<dyn Fn(String) -> JsValue>);
        let _ = js_sys::Reflect::set(hooks, &"exportEnvelope".into(), export.as_ref());
        export.forget();

        // Send a raw envelope (JSON) to `pubkey` over our link, as a member replaying it would.
        let s = self.clone();
        let inject = Closure::wrap(Box::new(move |pubkey: String, json: String| {
            let Ok(envelope) = serde_json::from_str::<RoomEnvelope>(&json) else {
                return;
            };
            if let (Some(link), Some(frame)) = (s.link(&pubkey), s.encode(&envelope)) {
                link.send(&frame);
            }
        }) as Box<dyn Fn(String, String)>);
        let _ = js_sys::Reflect::set(hooks, &"injectEnvelope".into(), inject.as_ref());
        inject.forget();
    }
}
