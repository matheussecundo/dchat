//! Chat extras on top of signed room messages: typing indicator, reactions, editing and
//! deleting one's own messages, private DMs (ECDH-sealed), and @mention detection.

use super::RoomSession;
use crate::state::{current_time_string, DmUi};
use leptos::*;
use protocol::{mentions, open_json, seal_json, DmContent, RoomBody, RoomEnvelope};
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
        let on = !self.inner.reactions.borrow().has(target, emoji, &self.inner.me);
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

    fn author_of(&self, message_id: &str) -> Option<String> {
        self.inner.message_authors.borrow().get(message_id).cloned()
    }

    /// Remember who wrote a chat message (edits and deletes must come from them) and
    /// whether it mentions us.
    pub(super) fn track_chat(&self, envelope: &RoomEnvelope, text: &str) -> bool {
        self.inner
            .message_authors
            .borrow_mut()
            .insert(envelope.id.clone(), envelope.author.clone());
        self.stop_typing(&envelope.author);
        let my_name = self.inner.names.borrow().get(&self.inner.me).cloned().unwrap_or_default();
        let mentioned = envelope.author != self.inner.me && mentions(text, &my_name);
        if mentioned {
            self.inner.signals.mention.update(|n| *n += 1);
        }
        mentioned
    }

    pub(super) fn on_chat_extra(&self, envelope: &RoomEnvelope) {
        let author = envelope.author.as_str();
        match &envelope.body {
            RoomBody::Typing => self.on_typing(author),
            RoomBody::Reaction { target, emoji, on } => {
                let changed = self.inner.reactions.borrow_mut().apply(target, emoji, author, envelope.ts, *on);
                if changed {
                    let tally = self.inner.reactions.borrow().tally(target);
                    self.update_message(target, |m| m.reactions = tally);
                }
            }
            RoomBody::Edit { target, text } => {
                if self.author_of(target).as_deref() != Some(author) {
                    return;
                }
                let newer = self.inner.last_edit.borrow().get(target).map_or(true, |ts| *ts < envelope.ts);
                if !newer {
                    return;
                }
                self.inner.last_edit.borrow_mut().insert(target.clone(), envelope.ts);
                // The signed original no longer matches: keep it out of history.
                self.inner.history.borrow_mut().remove(target);
                let my_name = self.inner.names.borrow().get(&self.inner.me).cloned().unwrap_or_default();
                let mentioned = author != self.inner.me && mentions(text, &my_name);
                self.update_message(target, |m| {
                    m.text = text.clone();
                    m.edited = true;
                    m.mentions_me = mentioned;
                });
            }
            RoomBody::Delete { target } => {
                if self.author_of(target).as_deref() != Some(author) {
                    return;
                }
                self.inner.history.borrow_mut().remove(target);
                self.inner.signals.messages.update(|msgs| msgs.retain(|m| m.id != *target));
            }
            RoomBody::Dm { .. } => {
                self.receive_dm(envelope);
            }
            _ => {}
        }
    }

    fn update_message(&self, id: &str, change: impl FnOnce(&mut crate::state::ChatMessageUi)) {
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
        });
        self.inner.signals.dm_unread.update(|unread| *unread.entry(author.to_string()).or_default() += 1);
        true
    }

    /// Send a private message: sealed to `to` with ECDH; over our direct link when there is
    /// one (so nobody else even sees the envelope), otherwise relayed through the room,
    /// where members see that we sent a private message but not to whom.
    pub fn send_dm(&self, to: &str, text: &str) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Empty message".into());
        }
        if !self.inner.present.borrow().contains(to) {
            return Err("That member is not in the room".into());
        }
        let sealed = seal_json(&self.inner.identity, to, &DmContent { text: text.to_string() }).map_err(|e| e.to_string())?;
        let body = RoomBody::Dm { sealed };
        let envelope = RoomEnvelope::sign(&self.inner.identity, js_sys::Date::now() as u64, body).map_err(|e| e.to_string())?;
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        let frame = self.encode(&envelope).ok_or("Failed to encrypt")?;
        let direct = self.link(to).filter(|l| l.is_open());
        let sent = match direct {
            Some(link) => link.send(&frame),
            None => self
                .inner
                .links
                .borrow()
                .values()
                .map(|l| l.send(&frame))
                .fold(false, |any, ok| any || ok),
        };
        if !sent {
            return Err("No route to that member".into());
        }
        self.push_dm(to, DmUi {
            id: envelope.id,
            from_me: true,
            text: text.to_string(),
            time: current_time_string(),
            notice: false,
        });
        Ok(())
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
        });
    }
}
