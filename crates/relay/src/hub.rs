//! Who is subscribed to what. RAM only: nothing is stored, events are forwarded and forgotten.

use protocol::{NostrEvent, NostrFilter};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

struct Connection {
    ip: String,
    tx: UnboundedSender<String>,
    subscriptions: HashMap<String, Vec<NostrFilter>>,
}

#[derive(Default)]
struct Inner {
    connections: HashMap<u64, Connection>,
    per_ip: HashMap<String, usize>,
    next_id: u64,
}

#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

impl Hub {
    /// Register a connection, or `None` if `ip` already has `max_per_ip` open.
    pub fn register(&self, ip: &str, tx: UnboundedSender<String>, max_per_ip: usize) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        let count = inner.per_ip.get(ip).copied().unwrap_or(0);
        if count >= max_per_ip {
            return None;
        }
        inner.per_ip.insert(ip.to_string(), count + 1);
        inner.next_id += 1;
        let id = inner.next_id;
        inner.connections.insert(
            id,
            Connection {
                ip: ip.to_string(),
                tx,
                subscriptions: HashMap::new(),
            },
        );
        Some(id)
    }

    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(conn) = inner.connections.remove(&id) {
            let remaining = inner.per_ip.get(&conn.ip).copied().unwrap_or(1) - 1;
            if remaining == 0 {
                inner.per_ip.remove(&conn.ip);
            } else {
                inner.per_ip.insert(conn.ip, remaining);
            }
        }
    }

    /// Add or replace a subscription; refuses a new one beyond `max_subscriptions`.
    pub fn subscribe(&self, id: u64, sub_id: &str, filters: Vec<NostrFilter>, max_subscriptions: usize) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(conn) = inner.connections.get_mut(&id) else {
            return false;
        };
        if !conn.subscriptions.contains_key(sub_id) && conn.subscriptions.len() >= max_subscriptions {
            return false;
        }
        conn.subscriptions.insert(sub_id.to_string(), filters);
        true
    }

    pub fn unsubscribe(&self, id: u64, sub_id: &str) {
        if let Some(conn) = self.inner.lock().unwrap().connections.get_mut(&id) {
            conn.subscriptions.remove(sub_id);
        }
    }

    /// Deliver `event` to every other connection with a matching subscription.
    pub fn publish(&self, from: u64, event: &NostrEvent) -> usize {
        let Ok(event_json) = serde_json::to_value(event) else {
            return 0;
        };
        let inner = self.inner.lock().unwrap();
        let mut delivered = 0;
        for (id, conn) in &inner.connections {
            if *id == from {
                continue;
            }
            for (sub_id, filters) in &conn.subscriptions {
                if filters.iter().any(|f| f.matches(event)) {
                    let msg = serde_json::json!(["EVENT", sub_id, event_json]).to_string();
                    if conn.tx.send(msg).is_ok() {
                        delivered += 1;
                    }
                }
            }
        }
        delivered
    }
}
