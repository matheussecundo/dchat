//! Chat extras on top of signed room messages: typing indicator, reactions, editing and
//! deleting one's own messages, private DMs (ECDH-sealed, waiting in RAM for a member who is
//! away), and @mention detection.

use super::RoomSession;
use crate::state::{current_time_string, DmDelivery, DmUi};
use leptos::prelude::*;
use protocol::{open_json, seal_json, DmContent, RoomBody, RoomEnvelope};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::window;

/// Send a Typing beacon at most this often while the user types.
const TYPING_SEND_EVERY_MS: f64 = 3000.0;
/// Forget a typist this long after their last beacon.
const TYPING_EXPIRE_MS: f64 = 4500.0;
const TYPING_TICK_MS: i32 = 1000;

impl RoomSession {
    // ---- Typing ------------------------------------------------------------------------

    /// Call on every keystroke in the message box; beacons are throttled.
    pub fn notify_typing(&self) {
        let now = js_sys::Date::now();
        if now - self.inner.last_typing_sent.get() < TYPING_SEND_EVERY_MS || !self.has_open_link() {
            return;
        }
        self.inner.last_typing_sent.set(now);
        self.publish(RoomBody::Typing);
    }

    fn on_typing(&self, author: &str) {
        if author == self.inner.me {
            return;
        }
        self.inner.typing.borrow_mut().insert(author.to_string(), js_sys::Date::now());
        self.refresh_typing();
    }

    pub(super) fn stop_typing(&self, author: &str) {
        if self.inner.typing.borrow_mut().remove(author).is_some() {
            self.refresh_typing();
        }
    }

    fn refresh_typing(&self) {
        let now = js_sys::Date::now();
        self.inner.typing.borrow_mut().retain(|_, at| now - *at < TYPING_EXPIRE_MS);
        let present = self.inner.present.borrow();
        let names = self.inner.names.borrow();
        let mut typists: Vec<String> = self
            .inner
            .typing
            .borrow()
            .keys()
            .filter(|pk| present.contains(*pk))
            .filter_map(|pk| names.get(pk).cloned())
            .collect();
        typists.sort();
        self.inner.signals.typing.set(typists);
    }

    pub(super) fn start_typing_ticker(&self) {
        let s = self.clone();
        let cb = Closure::wrap(Box::new(move || {
            if !s.inner.closed.get() && !s.inner.typing.borrow().is_empty() {
                s.refresh_typing();
            }
        }) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            let _ = w.set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), TYPING_TICK_MS);
        }
        cb.forget();
    }

    // ---- Reactions, edit, delete ---------------------------------------------------------

    /// Toggle our `emoji` reaction on message `target`.
    pub fn toggle_reaction(&self, target: &str, emoji: &str) {
        let on = !self.inner.log.borrow().has_reaction(target, emoji, &self.inner.me);
        self.publish(RoomBody::Reaction {
            target: target.to_string(),
            emoji: emoji.to_string(),
            on,
        });
    }

    /// Replace the text of one of our own messages.
    pub fn edit_message(&self, target: &str, text: &str) {
        if self.author_of(target).as_deref() == Some(self.inner.me.as_str()) && !text.trim().is_empty() {
            self.publish(RoomBody::Edit {
                target: target.to_string(),
                text: text.trim().to_string(),
            });
        }
    }

    /// Remove one of our own messages for everyone (best effort: honest clients comply).
    pub fn delete_message(&self, target: &str) {
        if self.author_of(target).as_deref() == Some(self.inner.me.as_str()) {
            self.publish(RoomBody::Delete { target: target.to_string() });
        }
    }

    /// Who wrote chat message `message_id` (only they may edit or delete it), per the log.
    fn author_of(&self, message_id: &str) -> Option<String> {
        self.inner.log.borrow().chat_author(message_id).map(str::to_string)
    }

    pub(super) fn on_chat_extra(&self, envelope: &RoomEnvelope) {
        match &envelope.body {
            RoomBody::Typing => self.on_typing(&envelope.author),
            // Only the author's edits and deletions count, and the latest toggle wins: the
            // log applies those rules the same way to live and synced envelopes.
            RoomBody::Reaction { .. } | RoomBody::Edit { .. } | RoomBody::Delete { .. } => self.on_logged(envelope),
            RoomBody::Dm { .. } => {
                self.receive_dm(envelope);
            }
            _ => {}
        }
    }

    pub(super) fn update_message(&self, id: &str, change: impl FnOnce(&mut crate::state::ChatMessageUi)) {
        self.inner.signals.messages.update(|msgs| {
            if let Some(msg) = msgs.iter_mut().find(|m| m.id == id) {
                change(msg);
                msg.rev += 1;
            }
        });
    }

    // ---- Direct messages ----------------------------------------------------------------

    /// Open a private message if it is for us (it names no recipient: only the two
    /// members' session keys open it). Returns whether it was ours.
    pub(super) fn receive_dm(&self, envelope: &RoomEnvelope) -> bool {
        let author = envelope.author.as_str();
        let RoomBody::Dm { sealed } = &envelope.body else {
            return false;
        };
        if author == self.inner.me {
            return false;
        }
        let Some(content) = open_json::<DmContent>(&self.inner.identity, author, sealed) else {
            return false;
        };
        self.push_dm(author, DmUi {
            id: envelope.id.clone(),
            from_me: false,
            text: content.text,
            time: current_time_string(),
            notice: false,
            delivery: DmDelivery::Sent,
        });
        self.inner.signals.dm_unread.update(|unread| *unread.entry(author.to_string()).or_default() += 1);
        true
    }

    /// Send a private message: sealed to `to` with ECDH; over our direct link when there is
    /// one (so nobody else even sees the envelope), otherwise relayed through the room,
    /// where members see that we sent a private message but not to whom. To a member who is
    /// away it waits in RAM, sealed, until they are back (`flush_dm_outbox`).
    pub fn send_dm(&self, to: &str, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Empty message".into());
        }
        let away = self.inner.absences.borrow().is_away(to);
        if !away && !self.inner.present.borrow().contains(to) {
            return Err("That member is not in the room".into());
        }
        let sealed = seal_json(&self.inner.identity, to, &DmContent { text: text.to_string() }).map_err(|e| e.to_string())?;
        let body = RoomBody::Dm { sealed };
        let envelope =
            RoomEnvelope::sign(&self.inner.identity, self.adm(), js_sys::Date::now() as u64, body).map_err(|e| e.to_string())?;
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        let frame = self.encode(&envelope).ok_or("Failed to encrypt")?;
        let delivery = if away {
            self.inner.dm_outbox.borrow_mut().entry(to.to_string()).or_default().push((envelope.id.clone(), frame));
            DmDelivery::Waiting
        } else if self.send_dm_frame(to, &frame) {
            DmDelivery::Sent
        } else {
            return Err("No route to that member".into());
        };
        self.push_dm(to, DmUi {
            id: envelope.id,
            from_me: true,
            text: text.to_string(),
            time: current_time_string(),
            notice: false,
            delivery,
        });
        Ok(())
    }

    /// Send a sealed DM frame: over the direct link to `to`, else to every neighbor.
    fn send_dm_frame(&self, to: &str, frame: &str) -> bool {
        match self.link(to).filter(|l| l.is_open()) {
            Some(link) => link.send(frame),
            None => self.inner.links.borrow().values().map(|l| l.send(frame)).fold(false, |any, ok| any || ok),
        }
    }

    /// `peer` is back: send the DMs that waited for them.
    pub(super) fn flush_dm_outbox(&self, peer: &str) {
        if !self.inner.present.borrow().contains(peer) {
            return;
        }
        let Some(waiting) = self.inner.dm_outbox.borrow_mut().remove(peer) else {
            return;
        };
        let mut sent = Vec::new();
        let mut unsent = Vec::new();
        for (id, frame) in waiting {
            if self.send_dm_frame(peer, &frame) {
                sent.push(id);
            } else {
                unsent.push((id, frame));
            }
        }
        if !unsent.is_empty() {
            self.inner.dm_outbox.borrow_mut().insert(peer.to_string(), unsent);
        }
        self.set_dm_delivery(peer, &sent, DmDelivery::Sent);
    }

    /// `peer` left (or stayed away too long): the DMs that waited for them never go out.
    pub(super) fn drop_dm_outbox(&self, peer: &str) {
        let Some(waiting) = self.inner.dm_outbox.borrow_mut().remove(peer) else {
            return;
        };
        let ids: Vec<String> = waiting.into_iter().map(|(id, _)| id).collect();
        self.set_dm_delivery(peer, &ids, DmDelivery::NotDelivered);
    }

    fn set_dm_delivery(&self, peer: &str, ids: &[String], delivery: DmDelivery) {
        if ids.is_empty() {
            return;
        }
        self.inner.signals.dms.update(|threads| {
            for line in threads.get_mut(peer).into_iter().flatten().filter(|l| ids.contains(&l.id)) {
                line.delivery = delivery;
            }
        });
    }

    fn push_dm(&self, peer: &str, dm: DmUi) {
        self.inner.dm_peers.borrow_mut().insert(peer.to_string());
        self.inner.signals.dms.update(|threads| threads.entry(peer.to_string()).or_default().push(dm));
    }

    /// Mark a departed member's DM thread as ended (their session key is gone for good).
    pub(super) fn end_dm_thread(&self, peer: &str) {
        let has_thread = self.inner.dm_peers.borrow().contains(peer);
        if !has_thread {
            return;
        }
        let name = self.inner.names.borrow().get(peer).cloned().unwrap_or_default();
        self.push_dm(peer, DmUi {
            id: uuid::Uuid::new_v4().to_string(),
            from_me: false,
            text: name,
            time: current_time_string(),
            notice: true,
            delivery: DmDelivery::Sent,
        });
    }
}
