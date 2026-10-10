//! The timeline from the room's chat log (`protocol::chat_log`): live messages, file cards,
//! edits, deletions and reactions go through the log, and history synced from other members
//! (`sync.rs`) is shown from it, placed by its author's time.

use super::{clean_remote_name, RoomSession};
use crate::state::{current_time_string, merge_by_time, ChatMessageUi, FileTransferStatus, Notice};
use leptos::prelude::*;
use protocol::chat_log::{Effect, Ignored, Merge};
use protocol::{mentions, RoomBody, RoomEnvelope};
use std::collections::HashSet;

/// How an entry reached us.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Sent just now: appended, may chime.
    Live,
    /// Synced from a member: placed by its author's time, silently.
    History,
}

impl RoomSession {
    /// Add a verified envelope to the log.
    pub(super) fn log_merge(&self, envelope: &RoomEnvelope) -> Merge {
        let now = js_sys::Date::now() as u64;
        self.inner.log.borrow_mut().merge(envelope, now)
    }

    /// A live chat message, file card, edit, deletion or reaction.
    pub(super) fn on_logged(&self, envelope: &RoomEnvelope) {
        match self.log_merge(envelope) {
            Merge::Added(effects) if !effects.is_empty() => {
                self.show_effects(effects, Mode::Live);
            }
            // Too large, or dated outside what the log keeps: shown, just never synced.
            Merge::Ignored(Ignored::TooLarge | Ignored::Future | Ignored::BelowHorizon) => self.show_unlogged(envelope),
            // An edit or deletion of a message the log does not hold (yet).
            Merge::Added(_) | Merge::Pending => self.apply_to_unlogged(envelope),
            Merge::Ignored(_) => {}
        }
    }

    /// Show what the log reports; returns how many rows were added.
    pub(super) fn show_effects(&self, effects: Vec<Effect>, mode: Mode) -> usize {
        let mut rows = Vec::new();
        for effect in effects {
            match effect {
                Effect::Message(target) => rows.extend(self.row_from_log(&target, mode)),
                Effect::Edited(target) => self.refresh_text(&target),
                Effect::Reacted(target) => {
                    let tally = self.inner.log.borrow().tally(&target);
                    // The row reads its reactions live, without being rebuilt: a video or
                    // voice message playing in it keeps playing.
                    self.inner.signals.messages.update(|msgs| {
                        if let Some(m) = msgs.iter_mut().find(|m| m.id == target) {
                            m.reactions = tally;
                        }
                    });
                }
                // Live withdrawals go through `on_file_message`, which also stops a download.
                Effect::Withdrawn(target) if mode == Mode::History => self.set_file_status(&target, FileTransferStatus::Withdrawn),
                Effect::Deleted(target) => self.inner.signals.messages.update(|msgs| msgs.retain(|m| m.id != target)),
                Effect::Named(author) if mode == Mode::History => self.name_from_log(&author),
                Effect::Withdrawn(_) | Effect::Named(_) => {}
            }
        }
        match mode {
            Mode::Live => {
                let added = rows.len();
                for row in rows {
                    self.push_message(row);
                }
                added
            }
            Mode::History => self.insert_history(rows),
        }
    }

    /// The row of message or file card `target`, as the log holds it now.
    fn row_from_log(&self, target: &str, mode: Mode) -> Option<ChatMessageUi> {
        let log = self.inner.log.borrow();
        let view = log.view(target)?;
        let root = view.root.clone();
        let edited = view.edit.and_then(|e| match &e.body {
            RoomBody::Edit { text, .. } => Some(text.clone()),
            _ => None,
        });
        let (withdrawn, reactions) = (view.withdrawn, view.reactions);
        drop(log);
        let author = root.author.clone();
        let time = match mode {
            Mode::Live => current_time_string(),
            Mode::History => clock_time(root.ts),
        };
        match &root.body {
            RoomBody::Chat { text } => {
                let text = edited.clone().unwrap_or_else(|| text.clone());
                let mentions_me = self.mentions_me(&author, &text);
                if mode == Mode::Live {
                    self.stop_typing(&author);
                    if mentions_me {
                        self.inner.signals.mention.update(|n| *n += 1);
                    }
                }
                Some(ChatMessageUi {
                    id: root.id.clone(),
                    is_self: author == self.inner.me,
                    author,
                    text,
                    time,
                    ts: root.ts,
                    reactions,
                    edited: edited.is_some(),
                    mentions_me,
                    ..Default::default()
                })
            }
            RoomBody::FileOffer { .. } => {
                let mut row = self.file_card_row(&root, withdrawn, mode == Mode::Live)?;
                row.time = time;
                row.reactions = reactions;
                Some(row)
            }
            _ => None,
        }
    }

    /// A live message the log refused (too large, or dated outside what it keeps).
    fn show_unlogged(&self, envelope: &RoomEnvelope) {
        match &envelope.body {
            RoomBody::Chat { text } => {
                let mentions_me = self.mentions_me(&envelope.author, text);
                self.stop_typing(&envelope.author);
                if mentions_me {
                    self.inner.signals.mention.update(|n| *n += 1);
                }
                self.push_message(ChatMessageUi {
                    id: envelope.id.clone(),
                    author: envelope.author.clone(),
                    is_self: envelope.author == self.inner.me,
                    text: text.clone(),
                    time: current_time_string(),
                    ts: envelope.ts,
                    mentions_me,
                    ..Default::default()
                });
            }
            RoomBody::FileOffer { .. } => {
                if let Some(row) = self.file_card_row(envelope, false, true) {
                    self.push_message(row);
                }
            }
            _ => self.apply_to_unlogged(envelope),
        }
    }

    /// An edit or deletion of a message that is on screen but not in the log (too large,
    /// or older than what the log keeps): applied when it comes from the message's author.
    fn apply_to_unlogged(&self, envelope: &RoomEnvelope) {
        let author = envelope.author.clone();
        match &envelope.body {
            RoomBody::Edit { target, text } => {
                let mentions_me = self.mentions_me(&author, text);
                self.inner.signals.messages.update(|msgs| {
                    if let Some(m) = msgs.iter_mut().find(|m| m.id == *target && m.author == author && m.file.is_none()) {
                        m.text = text.clone();
                        m.edited = true;
                        m.mentions_me = mentions_me;
                        m.rev += 1;
                    }
                });
            }
            RoomBody::Delete { target } => {
                self.inner
                    .signals
                    .messages
                    .update(|msgs| msgs.retain(|m| !(m.id == *target && m.author == author && m.file.is_none())));
            }
            _ => {}
        }
    }

    /// The latest edit of `target` is on its row.
    fn refresh_text(&self, target: &str) {
        let latest = self.inner.log.borrow().view(target).and_then(|view| match view.edit.map(|e| &e.body) {
            Some(RoomBody::Edit { text, .. }) => Some((text.clone(), view.root.author.clone())),
            _ => None,
        });
        let Some((text, author)) = latest else {
            return;
        };
        let mentions_me = self.mentions_me(&author, &text);
        self.update_message(target, |m| {
            m.text = text;
            m.edited = true;
            m.mentions_me = mentions_me;
        });
    }

    /// Label an author from their logged Hello (they may have left), unless already named.
    fn name_from_log(&self, author: &str) {
        if self.inner.names.borrow().contains_key(author) {
            return;
        }
        let name = match self.inner.log.borrow().hello(author).map(|h| &h.body) {
            Some(RoomBody::Hello { name, .. }) => clean_remote_name(name, author),
            _ => return,
        };
        self.remember_name(author, &name);
    }

    /// Insert synced rows by time (those already on screen are skipped); returns how many.
    /// The first rows from before we joined bring the "earlier messages" line.
    fn insert_history(&self, rows: Vec<ChatMessageUi>) -> usize {
        if rows.is_empty() {
            return 0;
        }
        let join_ts = self.inner.join_ts;
        let mut added = 0;
        let mut earlier = false;
        self.inner.signals.messages.update(|msgs| {
            let fresh: Vec<ChatMessageUi> = {
                let on_screen: HashSet<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
                rows.into_iter().filter(|r| !on_screen.contains(r.id.as_str())).collect()
            };
            added = fresh.len();
            earlier = fresh.iter().any(|r| r.ts < join_ts);
            merge_by_time(msgs, fresh);
        });
        if earlier && !self.inner.history_noted.replace(true) {
            let notice = ChatMessageUi {
                id: uuid::Uuid::new_v4().to_string(),
                time: clock_time(join_ts),
                ts: join_ts,
                notice: Some(Notice::HistoryShown),
                ..Default::default()
            };
            self.inner.signals.messages.update(|msgs| merge_by_time(msgs, vec![notice]));
        }
        added
    }

    /// Whether `text` by `author` @-mentions us.
    fn mentions_me(&self, author: &str, text: &str) -> bool {
        let my_name = self.inner.names.borrow().get(&self.inner.me).cloned().unwrap_or_default();
        author != self.inner.me && mentions(text, &my_name)
    }
}

/// `HH:MM:SS` local time for a millisecond timestamp.
fn clock_time(ts: u64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    format!("{:02}:{:02}:{:02}", date.get_hours(), date.get_minutes(), date.get_seconds())
}
