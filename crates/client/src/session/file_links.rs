//! Extra file links. A member uploading to another can open up to
//! `MAX_FILE_CONNECTIONS` − 1 more connections to them, one at a time while each raises the
//! confirmed rate (`protocol::transfer::LinkGrowth`), and spreads chunks over all of them.
//! They are negotiated over the open main link (never through the relays), use the room's
//! ICE settings, carry chunks only, and close once uploads to that member have been idle
//! for a while. They end with the main link.

use super::RoomSession;
use crate::mesh::{ChunkRoute, FileLink, FileLinkEvent, FileLinkEventHandler, FileLinkSignalOut};
use crate::names::pubkey_tag;
use protocol::transfer::MAX_FILE_CONNECTIONS;
use protocol::{RoomBody, SignalPayload};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// Our links to a member close once no upload to them has run for this long.
const IDLE_CLOSE_MS: i32 = 15_000;

type LinkMap = RefCell<HashMap<String, BTreeMap<u32, Rc<FileLink>>>>;

pub(super) struct FileLinks {
    /// Links we dialed, by member and number: we send on them.
    dialed: LinkMap,
    /// Links members dialed to us: we only receive on them.
    answered: LinkMap,
    next_id: Cell<u32>,
    /// Per member, bumped whenever one of our links to them is lost along with the chunks
    /// still queued on it.
    lost: RefCell<HashMap<String, u64>>,
    last_upload_end: RefCell<HashMap<String, f64>>,
    /// Most connections one upload may use, the main link included (a local setting).
    cap: Cell<usize>,
    /// Test hook: open this many file links when an upload starts, and never more or fewer.
    #[cfg(feature = "e2e-hooks")]
    forced: Cell<Option<usize>>,
    #[cfg(feature = "e2e-hooks")]
    chunks_received: Cell<u32>,
    /// Test hook: chunks arriving over file links vanish, as if lost with the link.
    #[cfg(feature = "e2e-hooks")]
    discard: Cell<bool>,
}

impl Default for FileLinks {
    fn default() -> Self {
        Self {
            dialed: RefCell::new(HashMap::new()),
            answered: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
            lost: RefCell::new(HashMap::new()),
            last_upload_end: RefCell::new(HashMap::new()),
            cap: Cell::new(MAX_FILE_CONNECTIONS),
            #[cfg(feature = "e2e-hooks")]
            forced: Cell::new(None),
            #[cfg(feature = "e2e-hooks")]
            chunks_received: Cell::new(0),
            #[cfg(feature = "e2e-hooks")]
            discard: Cell::new(false),
        }
    }
}

impl RoomSession {
    /// Most connections a transfer of ours may use, the main link included (1 = the main
    /// link only), in both directions: our uploads open no more, and we accept no more from
    /// a member uploading to us. It applies at once: running uploads stop using links past
    /// it, and links members opened to us past it close (their sender resends what was on them).
    pub fn set_file_connections(&self, cap: usize) {
        let cap = cap.clamp(1, MAX_FILE_CONNECTIONS);
        let files = &self.inner.file_links;
        files.cap.set(cap);
        let mut excess = Vec::new();
        for (remote, links) in files.answered.borrow().iter() {
            excess.extend(links.keys().rev().take(links.len().saturating_sub(cap - 1)).map(|id| (remote.clone(), *id)));
        }
        for (remote, id) in excess {
            self.drop_file_link(&remote, id, false);
        }
    }

    pub(super) fn file_connection_cap(&self) -> usize {
        self.inner.file_links.cap.get()
    }

    /// Where chunks to `peer` can go now: the main link, then our open file links in use,
    /// `cap` routes at most.
    pub(super) fn chunk_routes(&self, peer: &str, cap: usize) -> Vec<ChunkRoute> {
        let mut routes: Vec<ChunkRoute> = self.link(peer).map(ChunkRoute::Main).into_iter().collect();
        if let Some(links) = self.inner.file_links.dialed.borrow().get(peer) {
            let usable = links.values().filter(|l| l.is_open() && !l.set_aside.get()).cloned();
            routes.extend(usable.take(cap.saturating_sub(1)).map(ChunkRoute::Extra));
        }
        routes
    }

    /// Bytes still queued on our connections to `peer`: the main link's file channel and every
    /// open file link we dialed, set aside or not.
    pub(super) fn queued_to(&self, peer: &str) -> u64 {
        let main = self.link(peer).and_then(|l| l.file_buffered_amount()).unwrap_or(0) as u64;
        let dialed = self.inner.file_links.dialed.borrow();
        let extra = dialed.get(peer).map_or(0, |links| {
            links.values().filter_map(|l| ChunkRoute::Extra(l.clone()).buffered()).map(u64::from).sum()
        });
        main + extra
    }

    /// Changes whenever a file link to `peer` is lost with chunks on it: an upload then sends
    /// again from the first chunk not yet confirmed.
    pub(super) fn file_links_lost(&self, peer: &str) -> u64 {
        self.inner.file_links.lost.borrow().get(peer).copied().unwrap_or(0)
    }

    /// Take back a link to `peer` set aside earlier, or dial one more.
    pub(super) fn add_file_link(&self, peer: &str) {
        let dialed = self.inner.file_links.dialed.borrow();
        let links = dialed.get(peer);
        if let Some(link) = links.and_then(|l| l.values().find(|l| l.is_open() && l.set_aside.get())) {
            link.set_aside.set(false);
            return;
        }
        if links.map_or(0, BTreeMap::len) + 1 >= MAX_FILE_CONNECTIONS {
            return;
        }
        drop(dialed);
        self.open_file_link(peer, None);
    }

    /// The newest link to `peer` did not raise the rate: give it no new chunks. It stays open
    /// (what it holds still arrives) until uploads to `peer` go idle.
    pub(super) fn set_aside_file_link(&self, peer: &str) {
        if let Some(link) = self
            .inner
            .file_links
            .dialed
            .borrow()
            .get(peer)
            .and_then(|links| links.values().rev().find(|l| l.is_open() && !l.set_aside.get()))
        {
            link.set_aside.set(true);
        }
    }

    /// Dial file link `id` to `remote` (a new number when `None`), or create our end of one
    /// they dialed (`Some`).
    fn open_file_link(&self, remote: &str, answering: Option<u32>) -> Option<Rc<FileLink>> {
        if !self.link(remote).is_some_and(|l| l.is_open()) {
            return None;
        }
        let dialed = answering.is_none();
        let files = &self.inner.file_links;
        let id = answering.unwrap_or_else(|| {
            let id = files.next_id.get();
            files.next_id.set(id + 1);
            id
        });
        let signal_out: FileLinkSignalOut = {
            let s = self.clone();
            let remote = remote.to_string();
            Rc::new(move |signal| {
                s.send_direct(&remote, RoomBody::FileLinkSignal { to: remote.clone(), link: id, from_dialer: dialed, signal });
            })
        };
        let on_event: FileLinkEventHandler = {
            let s = self.clone();
            Rc::new(move |remote, id, dialed, event| s.on_file_link_event(remote, id, dialed, event))
        };
        match FileLink::new(remote, id, dialed, &self.inner.rtc_config, signal_out, on_event) {
            Ok(link) => {
                let map = if dialed { &files.dialed } else { &files.answered };
                map.borrow_mut().entry(remote.to_string()).or_default().insert(id, link.clone());
                Some(link)
            }
            Err(err) => {
                log::warn!("Failed to create a file link: {:?}", err);
                None
            }
        }
    }

    /// A file link's offer, answer or ICE, straight from `author` over our main link.
    pub(super) fn on_file_link_signal(&self, author: &str, id: u32, from_dialer: bool, signal: &SignalPayload) {
        let files = &self.inner.file_links;
        // The link is ours to answer when `author` dialed it.
        let map = if from_dialer { &files.answered } else { &files.dialed };
        let existing = map.borrow().get(author).and_then(|links| links.get(&id)).cloned();
        match signal {
            SignalPayload::Offer { sdp, .. } if from_dialer && existing.is_none() => {
                // Our own cap counts too (it is never above `MAX_FILE_CONNECTIONS`).
                let count = files.answered.borrow().get(author).map_or(0, BTreeMap::len);
                if count + 1 >= files.cap.get() {
                    log::info!("Refusing a file link from {}: at our cap of {} connections", pubkey_tag(author), files.cap.get());
                    return;
                }
                if let Some(link) = self.open_file_link(author, Some(id)) {
                    let sdp = sdp.clone();
                    wasm_bindgen_futures::spawn_local(async move { link.handle_description(true, sdp).await });
                }
            }
            SignalPayload::Answer { sdp, .. } if !from_dialer => {
                if let Some(link) = existing {
                    let sdp = sdp.clone();
                    wasm_bindgen_futures::spawn_local(async move { link.handle_description(false, sdp).await });
                }
            }
            SignalPayload::IceBatch { candidates, .. } => {
                if let Some(link) = existing {
                    let candidates = candidates.clone();
                    wasm_bindgen_futures::spawn_local(async move { link.add_candidates(candidates).await });
                }
            }
            _ => {}
        }
    }

    fn on_file_link_event(&self, remote: &str, id: u32, dialed: bool, event: FileLinkEvent) {
        let files = &self.inner.file_links;
        let map = if dialed { &files.dialed } else { &files.answered };
        // Late events from a link already closed are ignored.
        if self.inner.closed.get() || !map.borrow().get(remote).is_some_and(|links| links.contains_key(&id)) {
            return;
        }
        match event {
            FileLinkEvent::Open => log::info!("File link {id} open with {}", pubkey_tag(remote)),
            FileLinkEvent::Closed => self.drop_file_link(remote, id, dialed),
            FileLinkEvent::Chunk(packet) => {
                #[cfg(feature = "e2e-hooks")]
                {
                    files.chunks_received.set(files.chunks_received.get() + 1);
                    if files.discard.get() {
                        return;
                    }
                }
                self.on_file_chunk(remote, Some((dialed, id)), &packet);
            }
        }
    }

    /// Close one file link. Losing one of ours loses the chunks still queued on it.
    fn drop_file_link(&self, remote: &str, id: u32, dialed: bool) {
        let files = &self.inner.file_links;
        let map = if dialed { &files.dialed } else { &files.answered };
        let Some(link) = map.borrow_mut().get_mut(remote).and_then(|links| links.remove(&id)) else {
            return;
        };
        link.close();
        if dialed {
            *files.lost.borrow_mut().entry(remote.to_string()).or_default() += 1;
        }
    }

    /// An upload to `peer` ended: close our links to them once no upload has run for a while.
    pub(super) fn file_links_idle_later(&self, peer: &str) {
        let now = js_sys::Date::now();
        self.inner.file_links.last_upload_end.borrow_mut().insert(peer.to_string(), now);
        let s = self.clone();
        let peer = peer.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            crate::media::sleep_ms(IDLE_CLOSE_MS).await;
            let files = &s.inner.file_links;
            let ended = files.last_upload_end.borrow().get(&peer).copied();
            if s.uploading_to(&peer) || ended != Some(now) {
                return;
            }
            let links = files.dialed.borrow_mut().remove(&peer);
            for link in links.into_iter().flat_map(BTreeMap::into_values) {
                link.close();
            }
        });
    }

    /// The main link to `remote` is gone, or so are they: every file link with them goes too.
    pub(super) fn close_file_links(&self, remote: &str) {
        let files = &self.inner.file_links;
        let dialed = files.dialed.borrow_mut().remove(remote);
        let answered = files.answered.borrow_mut().remove(remote);
        for link in dialed.into_iter().chain(answered).flat_map(BTreeMap::into_values) {
            link.close();
        }
        files.lost.borrow_mut().remove(remote);
        files.last_upload_end.borrow_mut().remove(remote);
    }

    pub(super) fn close_all_file_links(&self) {
        let files = &self.inner.file_links;
        let dialed = std::mem::take(&mut *files.dialed.borrow_mut());
        let answered = std::mem::take(&mut *files.answered.borrow_mut());
        for link in dialed.into_values().chain(answered.into_values()).flat_map(BTreeMap::into_values) {
            link.close();
        }
    }

    /// Test hook: with a forced count, open that many links to `peer` now and report that
    /// the upload must not grow or shrink them.
    #[cfg(feature = "e2e-hooks")]
    pub(super) fn forced_file_links(&self, peer: &str) -> bool {
        let Some(count) = self.inner.file_links.forced.get() else {
            return false;
        };
        let have = self.inner.file_links.dialed.borrow().get(peer).map_or(0, BTreeMap::len);
        for _ in have..count.min(MAX_FILE_CONNECTIONS - 1) {
            self.open_file_link(peer, None);
        }
        true
    }

    #[cfg(not(feature = "e2e-hooks"))]
    pub(super) fn forced_file_links(&self, _peer: &str) -> bool {
        false
    }

    /// `window.__dchat` probes for file links (see `install_e2e_hooks`).
    #[cfg(feature = "e2e-hooks")]
    pub(super) fn install_file_link_hooks(&self, hooks: &js_sys::Object) {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsValue;

        // Every upload opens exactly this many file links (a negative count: back to growth).
        let s = self.clone();
        let force = Closure::wrap(Box::new(move |count: i32| {
            s.inner.file_links.forced.set(usize::try_from(count).ok());
        }) as Box<dyn Fn(i32)>);
        let _ = js_sys::Reflect::set(hooks, &"forceFileLinks".into(), force.as_ref());
        force.forget();

        // Chunks this tab received over file links.
        let s = self.clone();
        let received = Closure::wrap(Box::new(move || JsValue::from(s.inner.file_links.chunks_received.get())) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(hooks, &"fileLinkChunks".into(), received.as_ref());
        received.forget();

        // Drop every chunk that arrives over a file link (still counted above).
        let s = self.clone();
        let discard = Closure::wrap(Box::new(move |on: bool| s.inner.file_links.discard.set(on)) as Box<dyn Fn(bool)>);
        let _ = js_sys::Reflect::set(hooks, &"discardFileLinkChunks".into(), discard.as_ref());
        discard.forget();

        // Open file links of this tab, both directions.
        let s = self.clone();
        let open = Closure::wrap(Box::new(move || {
            let files = &s.inner.file_links;
            let count = |map: &LinkMap| map.borrow().values().flat_map(BTreeMap::values).filter(|l| l.is_open()).count();
            JsValue::from((count(&files.dialed) + count(&files.answered)) as u32)
        }) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(hooks, &"openFileLinks".into(), open.as_ref());
        open.forget();

        // Lose one of our open file links, with whatever it still holds; returns whether one was.
        let s = self.clone();
        let drop_one = Closure::wrap(Box::new(move || {
            let victim = s
                .inner
                .file_links
                .dialed
                .borrow()
                .iter()
                .find_map(|(remote, links)| links.iter().find(|(_, l)| l.is_open()).map(|(id, _)| (remote.clone(), *id)));
            if let Some((remote, id)) = &victim {
                s.drop_file_link(remote, *id, true);
            }
            JsValue::from(victim.is_some())
        }) as Box<dyn Fn() -> JsValue>);
        let _ = js_sys::Reflect::set(hooks, &"dropFileLink".into(), drop_one.as_ref());
        drop_one.forget();
    }
}
