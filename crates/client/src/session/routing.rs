//! First handshakes over the mesh (`RoomBody::RoutedSignal`): a member the mesh already
//! reaches is dialed through it, so only the introducers' handshakes use the relays. Each
//! one is sealed to its recipient and passed hop by hop toward it (`Roster::next_hop`),
//! never flooded and never back where it came from.

use super::RoomSession;
use protocol::{seal_json, EncryptedPayload, RelaySignal, RoomBody, RoomEnvelope, SignalPayload};

impl RoomSession {
    /// The open link to hand a message for `to` to: `to`'s own, or a neighbor toward it (not
    /// `came_from`), or the link the author's last routed message came in on.
    fn next_hop(&self, to: &str, came_from: Option<&str>) -> Option<String> {
        let open = |pk: &str| self.link(pk).is_some_and(|l| l.is_open());
        if open(to) && came_from != Some(to) {
            return Some(to.to_string());
        }
        let hop = self.inner.roster.borrow().next_hop(&self.inner.me, to, came_from);
        hop.filter(|h| open(h))
            .or_else(|| self.inner.route_hints.borrow().get(to).filter(|h| open(h) && came_from != Some(h)).cloned())
    }

    /// Send a handshake signal for `to` over the mesh; false when the mesh doesn't reach it.
    pub(super) fn send_routed(&self, to: &str, signal: &SignalPayload) -> bool {
        if !self.inner.present.borrow().contains(to) {
            return false;
        }
        let Some(hop) = self.next_hop(to, None) else {
            return false;
        };
        let Ok(sealed) = seal_json(&self.inner.identity, to, signal) else {
            return false;
        };
        let body = RoomBody::RoutedSignal { to: to.to_string(), sealed };
        let Ok(envelope) = RoomEnvelope::sign(&self.inner.identity, self.adm(), js_sys::Date::now() as u64, body) else {
            return false;
        };
        self.inner.dedup.borrow_mut().insert(&envelope.id);
        self.encode(&envelope).is_some_and(|frame| self.link(&hop).is_some_and(|link| link.send(&frame)))
    }

    /// A routed handshake that came in over `from`'s link (verified, not seen before): ours,
    /// or passed on toward its recipient.
    pub(super) fn on_routed(&self, from: &str, frame: &str, envelope: &RoomEnvelope, to: &str, sealed: &EncryptedPayload) {
        let author = envelope.author.as_str();
        // Replies to the author go back the way this came, when nothing else is known.
        self.inner.route_hints.borrow_mut().insert(author.to_string(), from.to_string());
        if to == self.inner.me {
            let relayed = RelaySignal::Sealed { to: to.to_string(), sealed: sealed.clone() };
            match relayed.open(&self.inner.identity, author) {
                Some(signal @ (SignalPayload::Offer { .. } | SignalPayload::Answer { .. } | SignalPayload::IceBatch { .. })) => {
                    self.on_handshake(author.to_string(), signal);
                }
                Some(_) => log::warn!("Ignoring a routed signal that isn't a handshake"),
                None => log::warn!("Dropping a routed signal that does not open"),
            }
            return;
        }
        if !self.inner.route_budget.borrow_mut().allow(author, js_sys::Date::now() as u64) {
            log::warn!("Dropping a routed signal: its author sends too many");
            return;
        }
        if let Some(link) = self.next_hop(to, Some(from)).and_then(|hop| self.link(&hop)) {
            link.send(frame);
        }
    }
}
