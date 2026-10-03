//! Opt-in history for late joiners (`&hist=1`): members keep the last messages whose
//! authors marked them shareable, as signed originals, and hand them to newcomers who ask.

use super::RoomSession;
use crate::state::{ChatMessageUi, Notice};
use leptos::*;
use protocol::{RoomBody, RoomEnvelope};
use std::collections::HashSet;

/// Envelopes per history message, to stay well under data-channel message limits.
const HISTORY_BATCH: usize = 40;
/// Ask at most this many neighbors (duplicates are dropped by id).
const HISTORY_SOURCES: usize = 2;

impl RoomSession {
    pub fn history_enabled(&self) -> bool {
        self.inner.params.history
    }

    pub(super) fn record_history(&self, envelope: &RoomEnvelope) {
        if self.inner.params.history {
            self.inner.history.borrow_mut().record(envelope);
        }
    }

    pub(super) fn maybe_request_history(&self, remote: &str) {
        let requests = self.inner.history_requests.get();
        if !self.inner.params.history || requests >= HISTORY_SOURCES {
            return;
        }
        if self.send_direct(remote, RoomBody::HistoryRequest { to: remote.to_string() }) {
            self.inner.history_requests.set(requests + 1);
        }
    }

    pub(super) fn on_history_request(&self, from: &str) {
        if !self.inner.params.history {
            return;
        }
        let batches = self.inner.history.borrow().batches(HISTORY_BATCH);
        if batches.is_empty() {
            return;
        }
        // Authors' signed Hellos first, so the newcomer can name members who already left.
        let authors: HashSet<String> = batches.iter().flatten().map(|e| e.author.clone()).collect();
        let hellos: Vec<RoomEnvelope> = self
            .inner
            .hello_archive
            .borrow()
            .iter()
            .filter(|(pk, _)| authors.contains(*pk))
            .map(|(_, e)| e.clone())
            .collect();
        for envelopes in std::iter::once(hellos).chain(batches) {
            if !envelopes.is_empty() {
                self.send_direct(from, RoomBody::HistoryChunk { to: from.to_string(), envelopes });
            }
        }
    }

    pub(super) fn on_history_chunk(&self, envelopes: &[RoomEnvelope]) {
        if !self.inner.params.history {
            return;
        }
        let mut shown = 0;
        for envelope in envelopes {
            if self.inner.dedup.borrow().contains(&envelope.id) || !envelope.verify() {
                continue;
            }
            match &envelope.body {
                // Only for labels: the author may be gone, so the roster is left alone (and
                // the id is not marked seen, in case the same Hello arrives live later).
                RoomBody::Hello { name, .. } => {
                    let known = self.inner.names.borrow().contains_key(&envelope.author);
                    if !known {
                        self.remember_name(&envelope.author, name);
                    }
                }
                RoomBody::Chat { text, shareable: true } => {
                    self.inner.dedup.borrow_mut().insert(&envelope.id);
                    self.record_history(envelope);
                    self.insert_message_by_time(ChatMessageUi {
                        id: envelope.id.clone(),
                        author: envelope.author.clone(),
                        is_self: false,
                        text: text.clone(),
                        time: clock_time(envelope.ts),
                        ts: envelope.ts,
                        notice: None,
                        file: None,
                    });
                    shown += 1;
                }
                _ => {}
            }
        }
        if shown > 0 && !self.inner.history_noted.replace(true) {
            self.insert_message_by_time(ChatMessageUi {
                id: uuid::Uuid::new_v4().to_string(),
                author: String::new(),
                is_self: false,
                text: String::new(),
                time: clock_time(self.inner.join_ts),
                ts: self.inner.join_ts,
                notice: Some(Notice::HistoryShown),
                file: None,
            });
        }
    }

    /// Insert after every message with an earlier or equal timestamp.
    fn insert_message_by_time(&self, message: ChatMessageUi) {
        self.inner.signals.messages.update(|msgs| {
            let index = msgs.iter().position(|m| m.ts > message.ts).unwrap_or(msgs.len());
            msgs.insert(index, message);
        });
    }
}

/// `HH:MM:SS` local time for a millisecond timestamp.
fn clock_time(ts: u64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    format!("{:02}:{:02}:{:02}", date.get_hours(), date.get_minutes(), date.get_seconds())
}
