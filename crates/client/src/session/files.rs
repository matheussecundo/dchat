//! Room-wide file sharing. The author posts a file card to the room; every member who
//! wants the file pulls it from the author over their own direct link, and over extra file
//! links to them when those raise the rate (`file_links.rs`). Chunks are sealed with the
//! room key (ChaCha20-Poly1305, authenticated header) and never relayed; the downloader puts
//! them back in order and confirms progress (`protocol::transfer`). All transfer state lives
//! in RAM. A download whose link drops (one side switched apps) pauses and goes on from the
//! first missing chunk once the link is back; it ends when the sender leaves or stays away
//! past the grace (`protocol::presence`).

use super::RoomSession;
use crate::media::sleep_ms;
use crate::mesh::{wait_for_room, ChunkRoute};
use crate::state::{current_time_string, ChatMessageUi, DownloadSummary, FileOfferInfo, FileTransferStatus};
use leptos::prelude::*;
use protocol::media::{mime_from_name, preview_type, MediaInfo};
use protocol::transfer::{in_send_window, AckPacer, Growth, LinkGrowth, RateMeter, Reorder};
use protocol::{
    decrypt_chunk, encrypt_chunk, QueueDecision, RoomBody, RoomEnvelope, Upload, UploadQueue,
    CHUNK_SIZE, MAX_CONCURRENT_UPLOADS,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, HtmlAnchorElement};

/// Pause sending on a connection while this much is still queued on it.
const BACKPRESSURE_HIGH: u32 = 2 * 1024 * 1024;
/// How often an upload that has sent all it may checks for new confirmations.
const ACK_POLL_MS: i32 = 20;
/// The speed shown on the cards: averaged over this long, and changed at most this often,
/// so it reads calmly while connections ramp up and chunks arrive out of order.
const SPEED_WINDOW_MS: f64 = 5000.0;
const SPEED_REFRESH_MS: f64 = 1000.0;
/// Both cards count the connections that carried a chunk this recently, so they agree.
const CARRIED_RECENTLY_MS: f64 = 2000.0;

/// How many of `last_used` (when each route last carried a chunk) did so lately; at least 1.
fn recently_used<K>(last_used: &mut HashMap<K, f64>, now: f64) -> u8 {
    last_used.retain(|_, at| now - *at <= CARRIED_RECENTLY_MS);
    last_used.len().clamp(1, u8::MAX as usize) as u8
}
/// Batch size when reading file from disk into memory before chunking.
const READ_BATCH_SIZE: usize = 2 * 1024 * 1024;
/// Without a save picker, received chunks are merged into one Blob every this many bytes,
/// so finishing a download never holds the file twice.
const FOLD_BYTES: usize = 8 * 1024 * 1024;

struct Offer {
    author: String,
    name: String,
    size: u64,
    mime_type: String,
}

/// What a download is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Saved as a file (save picker, else the browser's downloads), as before previews.
    Save,
    /// Shown in the chat: held in RAM (`HeldMedia`), nothing saved.
    View,
    /// Shown in the chat, then saved (Download on media not loaded yet).
    ViewThenSave,
}

struct Download {
    author: String,
    name: String,
    /// For a preview, the allow-listed type (`preview_type`), never the declared one.
    mime_type: String,
    purpose: Purpose,
    /// In-memory fallback when the File System Access API is unavailable: the chunks
    /// received since the last fold, and everything before them as one Blob.
    pending: Vec<js_sys::Uint8Array>,
    pending_bytes: usize,
    folded: Option<web_sys::Blob>,
    /// `FileSystemWritableFileStream` when the user picked a save location.
    writable: Option<JsValue>,
    /// Sequential promise chain for disk writes.
    write_promise: Option<js_sys::Promise>,
    started: f64,
    last_ui_update: f64,
    received: u64,
    /// Chunks arrive over several connections: early ones wait here for the gap to fill.
    reorder: Reorder<Vec<u8>>,
    /// Chunk count, from the first chunk; every other must agree.
    total: Option<u32>,
    acks: AckPacer,
    /// Recent rate, for the progress label, and the speed it shows.
    meter: RateMeter,
    speed_kb: u64,
    speed_at: f64,
    /// When the first chunk arrived: the summary's clock starts there, not at the request,
    /// which may have waited in the sender's queue.
    first_chunk_at: Option<f64>,
    /// When each of the author's connections (`None`: the main link; else a file link)
    /// last delivered a chunk, and how many ever did.
    routes_seen: HashMap<Option<(bool, u32)>, f64>,
    most_routes: usize,
    /// Since when the link to the author is down (the download waits for it), and how long
    /// earlier pauses lasted (left out of the time and speed shown).
    paused_since: Option<f64>,
    paused_ms: f64,
}

impl Download {
    /// Merge the pending chunks into the Blob held so far and return it. A Blob made from
    /// another refers to its bytes instead of copying them: one copy of the file in RAM.
    fn fold(&mut self) -> Result<web_sys::Blob, JsValue> {
        let parts: js_sys::Array = self
            .folded
            .take()
            .map(JsValue::from)
            .into_iter()
            .chain(self.pending.drain(..).map(JsValue::from))
            .collect();
        self.pending_bytes = 0;
        let bag = web_sys::BlobPropertyBag::new();
        bag.set_type(blob_type(&self.mime_type));
        let blob = web_sys::Blob::new_with_blob_sequence_and_options(&parts, &bag)?;
        self.folded = Some(blob.clone());
        Ok(blob)
    }
}

struct ActiveUpload {
    /// Tells this run apart from a later request for the same file by the same member.
    id: u64,
    started: f64,
    /// Chunks the requester confirmed (see `protocol::transfer`).
    acked: u32,
    last_ui_update: f64,
    progress: u8,
    /// The speed shown on our card (see `SPEED_WINDOW_MS`).
    meter: RateMeter,
    speed_kb: u64,
    speed_at: f64,
    connections: u8,
}

/// The part of the file read into memory, `READ_BATCH_SIZE` at a time.
#[derive(Default)]
struct ReadBatch {
    first: u32,
    bytes: Vec<u8>,
}

impl ReadBatch {
    /// Chunk `index` of `file`, reading the batch that holds it when needed.
    async fn chunk(&mut self, file: &web_sys::File, index: u32) -> Option<&[u8]> {
        let held = self.bytes.len().div_ceil(CHUNK_SIZE) as u32;
        if index < self.first || index >= self.first + held {
            let start = index as f64 * CHUNK_SIZE as f64;
            let end = (start + READ_BATCH_SIZE as f64).min(file.size());
            let blob = file.slice_with_f64_and_f64(start, end).ok()?;
            let buffer = JsFuture::from(blob.array_buffer()).await.ok()?;
            self.first = index;
            self.bytes = js_sys::Uint8Array::new(&buffer).to_vec();
        }
        // An empty file is one empty chunk.
        let offset = (index - self.first) as usize * CHUNK_SIZE;
        self.bytes.get(offset..(offset + CHUNK_SIZE).min(self.bytes.len()))
    }
}

pub(super) struct Files {
    offers: RefCell<HashMap<String, Offer>>,
    /// Files this tab offered, still available to request.
    outgoing: RefCell<HashMap<String, web_sys::File>>,
    queue: RefCell<UploadQueue>,
    withdrawn: RefCell<HashSet<String>>,
    /// Completed uploads per offered file, for the author's card.
    delivered: RefCell<HashMap<String, usize>>,
    downloads: RefCell<HashMap<String, Download>>,
    /// Uploads streaming chunks. Removing one (requester cancelled, link lost) stops it
    /// at its next chunk.
    active_transfers: RefCell<HashMap<Upload, ActiveUpload>>,
    /// Where each requested upload starts, until it does (`FileRequest::from_chunk`).
    starts_at: RefCell<HashMap<Upload, u32>>,
    next_upload_id: Cell<u64>,
    /// Test-only pause between chunks, so queues and interruptions are observable.
    #[cfg(feature = "e2e-hooks")]
    pub chunk_delay_ms: std::cell::Cell<i32>,
    /// Test-only count of the files this tab asked for (a save from RAM asks for nothing).
    #[cfg(feature = "e2e-hooks")]
    pub requests_sent: std::cell::Cell<u32>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            offers: RefCell::new(HashMap::new()),
            outgoing: RefCell::new(HashMap::new()),
            queue: RefCell::new(UploadQueue::new(MAX_CONCURRENT_UPLOADS)),
            withdrawn: RefCell::new(HashSet::new()),
            delivered: RefCell::new(HashMap::new()),
            downloads: RefCell::new(HashMap::new()),
            active_transfers: RefCell::new(HashMap::new()),
            starts_at: RefCell::new(HashMap::new()),
            next_upload_id: Cell::new(0),
            #[cfg(feature = "e2e-hooks")]
            chunk_delay_ms: std::cell::Cell::new(0),
            #[cfg(feature = "e2e-hooks")]
            requests_sent: std::cell::Cell::new(0),
        }
    }
}

impl RoomSession {
    // ---- Author side ------------------------------------------------------------------

    /// Post a file card to the room; members then request it individually. `media`: what
    /// the card shows before anyone pulls the file (thumbnail, size, waveform). Returns the
    /// file id. Alone in the room, the card waits in the chat log for whoever joins, who can
    /// pull the file from us while we stay.
    pub fn share_file(&self, file: web_sys::File, caption: Option<String>, media: Option<MediaInfo>) -> Result<String, String> {
        let file_id = uuid::Uuid::new_v4().to_string();
        let declared = file.type_();
        let mime_type = if declared.is_empty() { mime_from_name(&file.name()).unwrap_or_default().to_string() } else { declared };
        // A preview members would refuse is left out rather than losing the whole card.
        let media = media.filter(|m| m.is_valid(&mime_type));
        let body = RoomBody::FileOffer {
            file_id: file_id.clone(),
            name: file.name(),
            size: file.size() as u64,
            mime_type,
            caption,
            media,
        };
        self.inner.files.outgoing.borrow_mut().insert(file_id.clone(), file);
        self.publish(body).map(|_| file_id).ok_or_else(|| "Failed to sign file offer".into())
    }

    /// Stop offering a file to everyone, including transfers in progress.
    pub fn withdraw_file(&self, file_id: &str) {
        let files = &self.inner.files;
        if files.outgoing.borrow_mut().remove(file_id).is_none() {
            return;
        }
        files.withdrawn.borrow_mut().insert(file_id.to_string());
        let started = files.queue.borrow_mut().remove_file(file_id);
        self.after_queue_change(started);
        self.publish(RoomBody::FileCancel {
            to: None,
            file_id: file_id.to_string(),
        });
    }

    fn on_file_request(&self, requester: &str, file_id: &str, from_chunk: u32) {
        let files = &self.inner.files;
        if !files.outgoing.borrow().contains_key(file_id) {
            // A card from history whose file this tab no longer has: say so, so the
            // requester's card doesn't wait forever.
            self.send_direct(requester, RoomBody::FileCancel { to: Some(requester.to_string()), file_id: file_id.to_string() });
            return;
        }
        let upload = (file_id.to_string(), requester.to_string());
        files.starts_at.borrow_mut().insert(upload.clone(), from_chunk);
        // Asked again while still sending (it lost our link and came back before we noticed):
        // the newest request says where it is, so the run starts over from there.
        if files.queue.borrow().is_active(file_id, requester) {
            files.active_transfers.borrow_mut().remove(&upload);
            self.start_upload(upload.0, upload.1);
            return;
        }
        let decision = files.queue.borrow_mut().request(file_id, requester);
        match decision {
            QueueDecision::Start => self.start_upload(file_id.to_string(), requester.to_string()),
            QueueDecision::Queued(position) => {
                self.send_direct(
                    requester,
                    RoomBody::FileQueued {
                        to: requester.to_string(),
                        file_id: file_id.to_string(),
                        position,
                    },
                );
            }
            QueueDecision::Duplicate => {}
        }
        self.refresh_sharing_card(file_id);
    }

    /// An upload ended (done, cancelled or link lost): free its slot, start whoever is
    /// next and tell the rest their new place in line.
    fn finish_upload(&self, file_id: &str, peer: &str) {
        let started = self.inner.files.queue.borrow_mut().finish(file_id, peer);
        self.after_queue_change(started);
        self.refresh_sharing_card(file_id);
    }

    fn after_queue_change(&self, started: Vec<Upload>) {
        for (file_id, peer) in started {
            self.start_upload(file_id.clone(), peer);
            self.refresh_sharing_card(&file_id);
        }
        let positions = self.inner.files.queue.borrow().positions();
        for ((file_id, peer), position) in positions {
            self.send_direct(&peer, RoomBody::FileQueued { to: peer.clone(), file_id, position });
        }
    }

    fn start_upload(&self, file_id: String, peer: String) {
        let files = &self.inner.files;
        let Some(file) = files.outgoing.borrow().get(&file_id).cloned() else {
            return;
        };
        let total_chunks = chunk_count(file.size());
        let first = files.starts_at.borrow_mut().remove(&(file_id.clone(), peer.clone())).unwrap_or(0).min(total_chunks);
        let id = files.next_upload_id.get();
        files.next_upload_id.set(id + 1);
        files.active_transfers.borrow_mut().insert(
            (file_id.clone(), peer.clone()),
            ActiveUpload {
                id,
                started: js_sys::Date::now(),
                acked: first,
                last_ui_update: js_sys::Date::now(),
                progress: 0,
                meter: RateMeter::new(SPEED_WINDOW_MS),
                speed_kb: 0,
                speed_at: js_sys::Date::now(),
                connections: 1,
            },
        );
        self.refresh_sharing_card(&file_id);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let completed = s.upload(&file, &file_id, &peer, id, first).await;
            s.file_links_idle_later(&peer);
            let upload = (file_id.clone(), peer.clone());
            let mut active = s.inner.files.active_transfers.borrow_mut();
            // A cancelled or lost upload was already taken out of the queue, and a new
            // request from the same member may be running under this key by now.
            if active.get(&upload).is_none_or(|a| a.id != id) {
                return;
            }
            active.remove(&upload);
            drop(active);
            if completed {
                *s.inner.files.delivered.borrow_mut().entry(file_id.clone()).or_default() += 1;
            }
            s.finish_upload(&file_id, &peer);
        });
    }

    /// Whether an upload to `peer` is running.
    pub(super) fn uploading_to(&self, peer: &str) -> bool {
        self.inner.files.active_transfers.borrow().keys().any(|(_, p)| p == peer)
    }

    /// Whether upload run `id` should keep sending (not cancelled, lost or withdrawn).
    fn upload_running(&self, upload: &Upload, id: u64) -> bool {
        let files = &self.inner.files;
        files.active_transfers.borrow().get(upload).is_some_and(|a| a.id == id) && !files.withdrawn.borrow().contains(&upload.0)
    }

    /// Stream `file` to `peer` in sealed chunks from chunk `first` on, over every connection
    /// in use; returns whether the peer confirmed them all.
    async fn upload(&self, file: &web_sys::File, file_id: &str, peer: &str, id: u64, first: u32) -> bool {
        let Ok(file_uuid) = uuid::Uuid::parse_str(file_id) else {
            return false;
        };
        let total_size = file.size();
        let total_chunks = chunk_count(total_size);
        // What the peer had before this run, for progress and growth.
        let had = (first as f64 * CHUNK_SIZE as f64).min(total_size) as u64;
        let upload = (file_id.to_string(), peer.to_string());
        let forced = self.forced_file_links(peer);
        let mut cap = self.file_connection_cap();
        let grow_to = |cap| if forced { 1 } else { cap };
        let mut growth = LinkGrowth::new(grow_to(cap), js_sys::Date::now());
        // When each route last carried a chunk, for the count on our card.
        let mut carried: HashMap<Option<u32>, f64> = HashMap::new();
        let mut batch = ReadBatch::default();
        let mut next: u32 = first;
        let mut turn = 0;
        let mut lost = self.file_links_lost(peer);
        // Bytes handed to the connections, resent ones included.
        let mut sent: u64 = 0;

        loop {
            if !self.upload_running(&upload, id) {
                return false;
            }
            let acked = self.inner.files.active_transfers.borrow().get(&upload).map_or(first, |a| a.acked.min(total_chunks));
            if acked == total_chunks {
                return true;
            }
            // A lost file link took the chunks queued on it along: send again from the
            // first one not confirmed.
            if self.file_links_lost(peer) != lost {
                lost = self.file_links_lost(peer);
                next = acked;
            }
            next = next.max(acked);
            // A new cap applies at once, and growth starts over from there.
            if self.file_connection_cap() != cap {
                cap = self.file_connection_cap();
                growth = LinkGrowth::new(grow_to(cap), js_sys::Date::now());
            }
            let routes = self.chunk_routes(peer, cap);
            let taken = sent.saturating_sub(self.queued_to(peer));
            let shown = recently_used(&mut carried, js_sys::Date::now());
            self.steer_upload(&upload, &mut growth, had, taken, total_size, routes.len(), shown);
            if next == total_chunks || !in_send_window(next, acked) {
                sleep_ms(ACK_POLL_MS).await;
                continue;
            }
            let Some(route) = wait_for_room(&routes, BACKPRESSURE_HIGH, turn).await else {
                return false;
            };
            turn = route + 1;
            let Some(chunk_slice) = batch.chunk(file, next).await else {
                return false;
            };
            // Checked after the last await: a stopped run sends nothing more.
            if !self.upload_running(&upload, id) {
                return false;
            }
            let Ok(packet) = encrypt_chunk(&self.inner.key, file_uuid.as_bytes(), next, total_chunks, chunk_slice) else {
                return false;
            };
            if routes[route].send(&packet) {
                sent += packet.len() as u64;
                carried.insert(routes[route].key(), js_sys::Date::now());
            } else {
                // A file link that just closed: the next round sends this chunk elsewhere.
                // Losing the main link ends the upload (`on_file_peer_lost`).
                if matches!(routes[route], ChunkRoute::Main(_)) {
                    return false;
                }
                continue;
            }
            next += 1;

            #[cfg(feature = "e2e-hooks")]
            {
                let delay = self.inner.files.chunk_delay_ms.get();
                if delay > 0 {
                    sleep_ms(delay).await;
                }
            }
        }
    }

    /// Grow or shrink the connections an upload uses, and refresh its line on our card.
    /// `had`: bytes the peer had before this run; `taken`: bytes the connections took from our
    /// send queues since; `connections`: the routes open for it; `shown`: those that carried
    /// chunks lately.
    #[allow(clippy::too_many_arguments)]
    fn steer_upload(&self, upload: &Upload, growth: &mut LinkGrowth, had: u64, taken: u64, total_size: f64, connections: usize, shown: u8) {
        let now = js_sys::Date::now();
        let total = total_size as u64;
        let decision = growth.update(now, taken, connections, total.saturating_sub(had + taken));
        if decision != Growth::Hold {
            let rate = growth.rate().unwrap_or(0.0) / 1_048_576.0;
            log::info!("Upload to {}: {decision:?} at {rate:.1} MB/s over {connections} connections", crate::names::pubkey_tag(&upload.1));
        }
        match decision {
            Growth::AddLink => self.add_file_link(&upload.1),
            Growth::DropLink => self.set_aside_file_link(&upload.1),
            Growth::Hold => {}
        }
        let mut refresh = false;
        if let Some(state) = self.inner.files.active_transfers.borrow_mut().get_mut(upload) {
            state.meter.record(now, taken);
            if now - state.speed_at >= SPEED_REFRESH_MS {
                state.speed_at = now;
                let average = taken as f64 / ((now - state.started) / 1000.0).max(0.001);
                state.speed_kb = (state.meter.rate().unwrap_or(average) / 1024.0) as u64;
            }
            state.progress = ((had + taken).min(total) as f64 / total.max(1) as f64 * 100.0) as u8;
            state.connections = shown;
            if now - state.last_ui_update >= 250.0 {
                state.last_ui_update = now;
                refresh = true;
            }
        }
        if refresh {
            self.refresh_sharing_card(&upload.0);
        }
    }

    fn refresh_sharing_card(&self, file_id: &str) {
        let files = &self.inner.files;
        if !files.outgoing.borrow().contains_key(file_id) {
            return;
        }
        let (active, waiting) = files.queue.borrow().counts(file_id);
        let done = files.delivered.borrow().get(file_id).copied().unwrap_or(0);

        let active_transfers = files.active_transfers.borrow();
        let active_peers: Vec<crate::state::PeerDownloadProgress> = active_transfers
            .iter()
            .filter(|((fid, _), _)| fid == file_id)
            .map(|((_, peer), state)| crate::state::PeerDownloadProgress {
                peer: peer.clone(),
                progress: state.progress,
                speed_kb: state.speed_kb,
                connections: state.connections,
            })
            .collect();

        let positions = files.queue.borrow().positions();
        let queued_peers: Vec<crate::state::PeerQueuedInfo> = positions
            .into_iter()
            .filter(|((fid, _), _)| fid == file_id)
            .map(|((_, peer), position)| crate::state::PeerQueuedInfo {
                peer,
                position,
            })
            .collect();

        self.set_file_status(
            file_id,
            FileTransferStatus::Sharing {
                active,
                waiting,
                done,
                active_peers,
                queued_peers,
            },
        );
    }

    // ---- Downloader side --------------------------------------------------------------

    /// Ask the author for the file (needs a direct link). Call from the Download click:
    /// the save-file picker requires the user gesture.
    /// Download a file to disk (save picker where there is one).
    pub fn download_file(&self, file_id: &str) {
        self.start_download(file_id, Purpose::Save);
    }

    /// Pull an image, video or audio file into RAM to show it in the chat (`then_save`: and
    /// save it once loaded). No save picker, so it needs no tap: cards load small media as
    /// soon as they are on screen.
    pub fn load_media(&self, file_id: &str, then_save: bool) {
        self.start_download(file_id, if then_save { Purpose::ViewThenSave } else { Purpose::View });
    }

    fn start_download(&self, file_id: &str, purpose: Purpose) {
        let Some((author, name, size, mime_type)) = self
            .inner
            .files
            .offers
            .borrow()
            .get(file_id)
            .map(|o| (o.author.clone(), o.name.clone(), o.size, o.mime_type.clone()))
        else {
            return;
        };
        if let Some(running) = self.inner.files.downloads.borrow_mut().get_mut(file_id) {
            // Download tapped while the card loads its preview: save it once loaded.
            if running.purpose == Purpose::View && purpose == Purpose::ViewThenSave {
                running.purpose = Purpose::ViewThenSave;
            }
            return;
        }
        // Previews are built only with an allow-listed type, and only up to the inline limit.
        let mime_type = match purpose {
            Purpose::Save => mime_type,
            Purpose::View | Purpose::ViewThenSave => match preview_type(&mime_type) {
                Some(preview) if size <= crate::held_media::INLINE_MAX_BYTES => preview.to_string(),
                _ => return,
            },
        };
        if !self.link(&author).is_some_and(|l| l.is_open()) {
            if purpose != Purpose::View {
                self.toast("file_unreachable");
            }
            return;
        }
        log::info!("Requesting {} ({} bytes)", name, size);
        let s = self.clone();
        let file_id = file_id.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let writable = match purpose {
                Purpose::Save => try_open_save_stream(&name).await,
                Purpose::View | Purpose::ViewThenSave => None,
            };
            s.inner.files.downloads.borrow_mut().insert(
                file_id.clone(),
                Download {
                    author: author.clone(),
                    name,
                    mime_type,
                    purpose,
                    pending: Vec::new(),
                    pending_bytes: 0,
                    folded: None,
                    writable,
                    write_promise: None,
                    started: js_sys::Date::now(),
                    last_ui_update: js_sys::Date::now(),
                    received: 0,
                    reorder: Reorder::default(),
                    total: None,
                    acks: AckPacer::default(),
                    meter: RateMeter::new(SPEED_WINDOW_MS),
                    speed_kb: 0,
                    speed_at: js_sys::Date::now(),
                    first_chunk_at: None,
                    routes_seen: HashMap::new(),
                    most_routes: 1,
                    paused_since: None,
                    paused_ms: 0.0,
                },
            );
            s.set_file_status(&file_id, FileTransferStatus::Downloading { progress: 0, speed_kb: 0, connections: 1 });
            #[cfg(feature = "e2e-hooks")]
            s.inner.files.requests_sent.set(s.inner.files.requests_sent.get() + 1);
            let request = RoomBody::FileRequest { to: author.clone(), file_id: file_id.clone(), from_chunk: 0 };
            if !s.send_direct(&author, request) {
                s.abort_download(&file_id, FileTransferStatus::Interrupted);
                s.toast("toast_file_request_failed");
            }
        });
    }

    /// Stop a queued or running download and tell the author.
    pub fn cancel_download(&self, file_id: &str) {
        let author = self.inner.files.downloads.borrow().get(file_id).map(|d| d.author.clone());
        if let Some(author) = author {
            self.send_direct(&author, RoomBody::FileCancel { to: Some(author.clone()), file_id: file_id.to_string() });
        }
        self.abort_download(file_id, FileTransferStatus::Cancelled);
    }

    fn abort_download(&self, file_id: &str, status: FileTransferStatus) {
        if let Some(download) = self.inner.files.downloads.borrow_mut().remove(file_id) {
            if let Some(writable) = download.writable {
                wasm_bindgen_futures::spawn_local(async move {
                    if let Some(p) = download.write_promise {
                        let _ = JsFuture::from(p).await;
                    }
                    let _ = call_stream_method(&writable, "abort", None).await;
                });
            }
        }
        self.set_file_status(file_id, status);
    }

    /// A packet on one of `from`'s file connections: a chunk of a file we download from
    /// them, or their confirmation of chunks of a file we offered.
    /// `route`: `None` for the main link, else `(dialed by us, number)` of a file link.
    pub(super) fn on_file_chunk(&self, from: &str, route: Option<(bool, u32)>, packet: &[u8]) {
        let Ok((header, plaintext)) = decrypt_chunk(&self.inner.key, packet) else {
            log::warn!("Dropping undecryptable file chunk");
            return;
        };
        let Ok(file_uuid) = uuid::Uuid::from_slice(&header.file_id) else {
            return;
        };
        let file_id = file_uuid.to_string();
        // About a file we offered: `from` confirms what it received so far.
        if self.inner.files.outgoing.borrow().contains_key(&file_id) {
            if let Some(state) = self.inner.files.active_transfers.borrow_mut().get_mut(&(file_id, from.to_string())) {
                state.acked = state.acked.max(header.chunk_index);
            }
            return;
        }
        let mut downloads = self.inner.files.downloads.borrow_mut();
        // Only the file's author may feed its download, over their own connections.
        let Some(download) = downloads.get_mut(&file_id).filter(|d| d.author == from) else {
            return;
        };
        let total = header.total_chunks;
        if total == 0 || header.chunk_index >= total || download.total.is_some_and(|t| t != total) {
            drop(downloads);
            self.abort_download(&file_id, FileTransferStatus::Interrupted);
            return;
        }
        download.total = Some(total);
        download.first_chunk_at.get_or_insert_with(js_sys::Date::now);
        download.routes_seen.insert(route, js_sys::Date::now());
        // Early chunks wait for the gap before them. Stale ones are dropped: duplicates, and
        // leftovers from a run we cancelled before asking again (same file, same bytes per index).
        let mut failed = false;
        for chunk in download.reorder.accept(header.chunk_index, plaintext) {
            download.received += chunk.len() as u64;
            match &download.writable {
                Some(writable) => {
                    let writable = writable.clone();
                    let bytes = js_sys::Uint8Array::from(&chunk[..]);
                    let prev = download.write_promise.take();
                    let next = wasm_bindgen_futures::future_to_promise(async move {
                        if let Some(p) = prev {
                            let _ = JsFuture::from(p).await;
                        }
                        let _ = call_stream_method(&writable, "write", Some(&bytes.into())).await;
                        Ok(JsValue::UNDEFINED)
                    });
                    download.write_promise = Some(next);
                }
                None => {
                    download.pending.push(js_sys::Uint8Array::from(&chunk[..]));
                    download.pending_bytes += chunk.len();
                    if download.pending_bytes >= FOLD_BYTES && download.fold().is_err() {
                        failed = true;
                        break;
                    }
                }
            }
        }
        if failed {
            drop(downloads);
            self.abort_download(&file_id, FileTransferStatus::Interrupted);
            return;
        }

        let next = download.reorder.next();
        let done = next == total;
        let now = js_sys::Date::now();
        let ack = download.acks.due(now, next, done);
        download.meter.record(now, download.received);
        let progress = (!done && now - download.last_ui_update >= 100.0).then(|| {
            download.last_ui_update = now;
            if now - download.speed_at >= SPEED_REFRESH_MS {
                download.speed_at = now;
                let elapsed = now - download.started - download.paused_ms;
                let average = download.received as f64 / (elapsed / 1000.0).max(0.001);
                download.speed_kb = (download.meter.rate().unwrap_or(average) / 1024.0) as u64;
            }
            let connections = recently_used(&mut download.routes_seen, now);
            download.most_routes = download.most_routes.max(connections as usize);
            ((next as f64 / total as f64 * 100.0) as u8, download.speed_kb, connections)
        });
        drop(downloads);
        if ack {
            let packet = encrypt_chunk(&self.inner.key, file_uuid.as_bytes(), next, total, &[]);
            if let (Ok(packet), Some(link)) = (packet, self.link(from)) {
                link.send_bytes(&packet);
            }
        }
        if !done {
            if let Some((progress, speed_kb, connections)) = progress {
                self.set_file_status(&file_id, FileTransferStatus::Downloading { progress, speed_kb, connections });
            }
            return;
        }

        let Some(mut done) = self.inner.files.downloads.borrow_mut().remove(&file_id) else {
            return;
        };
        let summary = DownloadSummary {
            bytes: done.received,
            millis: (now - done.first_chunk_at.unwrap_or(now) - done.paused_ms).max(0.0) as u64,
        };
        log::info!(
            "Downloaded {} bytes in {:.1} s ({:.2} MB/s, up to {} connections)",
            summary.bytes,
            summary.millis as f64 / 1000.0,
            summary.speed_kb() as f64 / 1024.0,
            done.most_routes
        );
        let Some(writable) = done.writable.take() else {
            self.finish_in_memory(&file_id, done, summary);
            return;
        };
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(p) = done.write_promise {
                let _ = JsFuture::from(p).await;
            }
            let _ = call_stream_method(&writable, "close", None).await;
            s.set_file_status(&file_id, FileTransferStatus::Completed { summary, withdrawn: false });
            s.toast("file_download_complete");
        });
    }

    /// The whole file is in RAM. On iOS it waits for a tap on Save: the share sheet needs
    /// that tap, and a download started unasked opens the file in another app, which
    /// suspends dchat and drops it from the room. Elsewhere the browser saves it right away.
    fn finish_in_memory(&self, file_id: &str, mut done: Download, summary: DownloadSummary) {
        let Ok(blob) = done.fold() else {
            self.set_file_status(file_id, FileTransferStatus::Interrupted);
            return;
        };
        if done.purpose != Purpose::Save {
            let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
                self.set_file_status(file_id, FileTransferStatus::Interrupted);
                return;
            };
            // Finished first, then shown: a picture that fails to decode finds its card done.
            self.set_file_status(file_id, FileTransferStatus::Completed { summary, withdrawn: false });
            self.inner.signals.held_media.update(|held| held.insert(file_id, blob.clone(), url, &done.name, false));
            if done.purpose == Purpose::ViewThenSave {
                // iOS needs a fresh tap for the share sheet: the card's Download saves it.
                if crate::pwa::is_ios() {
                    self.toast("toast_file_ready");
                } else {
                    let _ = download_blob(&blob, &done.name);
                }
            }
            return;
        }
        if crate::pwa::is_ios() {
            let bag = web_sys::FilePropertyBag::new();
            bag.set_type(blob_type(&done.mime_type));
            let Ok(file) = web_sys::File::new_with_blob_sequence_and_options(&js_sys::Array::of1(&blob), &done.name, &bag) else {
                self.set_file_status(file_id, FileTransferStatus::Interrupted);
                return;
            };
            self.inner.signals.ready_files.update_value(|ready| {
                ready.insert(file_id.to_string(), file);
            });
            self.set_file_status(file_id, FileTransferStatus::ReadyToSave { summary, withdrawn: false });
            self.toast("toast_file_ready");
        } else {
            let _ = download_blob(&blob, &done.name);
            self.set_file_status(file_id, FileTransferStatus::Completed { summary, withdrawn: false });
            self.toast("file_download_complete");
        }
    }

    // ---- Messages ---------------------------------------------------------------------

    /// Register a file card (once) and build its row. `live`: posted just now; otherwise it
    /// comes from history and offers Download only while its author is in the room.
    pub(super) fn file_card_row(&self, envelope: &RoomEnvelope, withdrawn: bool, live: bool) -> Option<ChatMessageUi> {
        let RoomBody::FileOffer { file_id, name, size, mime_type, caption, media } = &envelope.body else {
            return None;
        };
        if self.inner.files.offers.borrow().contains_key(file_id) {
            return None;
        }
        self.inner.files.offers.borrow_mut().insert(
            file_id.clone(),
            Offer {
                author: envelope.author.clone(),
                name: name.clone(),
                size: *size,
                mime_type: mime_type.clone(),
            },
        );
        let is_self = envelope.author == self.inner.me;
        let status = if withdrawn {
            FileTransferStatus::Withdrawn
        } else if is_self {
            FileTransferStatus::Sharing {
                active: 0,
                waiting: 0,
                done: 0,
                active_peers: Vec::new(),
                queued_peers: Vec::new(),
            }
        } else if live
            || self.inner.present.borrow().contains(&envelope.author)
            || self.inner.absences.borrow().is_away(&envelope.author)
        {
            FileTransferStatus::Offered
        } else {
            FileTransferStatus::SenderLeft
        };
        Some(ChatMessageUi {
            id: file_id.clone(),
            author: envelope.author.clone(),
            is_self,
            text: caption.clone().unwrap_or_default(),
            time: current_time_string(),
            ts: envelope.ts,
            file: Some(FileOfferInfo {
                file_id: file_id.clone(),
                name: name.clone(),
                size: *size,
                mime_type: mime_type.clone(),
                media: media.clone(),
                status,
            }),
            ..Default::default()
        })
    }

    /// After a rekey: the cards on screen came from the log, so their offers are known again,
    /// and the files we offered stay downloadable from us.
    pub(super) fn restore_files(&self, shared: HashMap<String, web_sys::File>) {
        let offers: Vec<(String, Offer)> = self
            .inner
            .log
            .borrow()
            .file_offers()
            .into_iter()
            .filter_map(|(envelope, _)| match &envelope.body {
                RoomBody::FileOffer { file_id, name, size, mime_type, .. } => Some((
                    file_id.clone(),
                    Offer { author: envelope.author.clone(), name: name.clone(), size: *size, mime_type: mime_type.clone() },
                )),
                _ => None,
            })
            .collect();
        self.inner.files.offers.borrow_mut().extend(offers);
        let mine: Vec<String> = shared.keys().cloned().collect();
        *self.inner.files.outgoing.borrow_mut() = shared;
        for file_id in mine {
            self.refresh_sharing_card(&file_id);
        }
    }

    /// Files this tab offers, for the next session after a rekey.
    pub(super) fn shared_files(&self) -> HashMap<String, web_sys::File> {
        self.inner.files.outgoing.borrow().clone()
    }

    /// Route a file message by its author and (direct-only) recipient.
    pub(super) fn on_file_message(&self, author: &str, body: &RoomBody) {
        match body {
            RoomBody::FileRequest { file_id, from_chunk, .. } => self.on_file_request(author, file_id, *from_chunk),
            RoomBody::FileQueued { file_id, position, .. } => {
                let ours = self.inner.files.downloads.borrow().get(file_id).is_some_and(|d| d.author == author);
                if ours {
                    self.set_file_status(file_id, FileTransferStatus::Queued { position: *position });
                }
            }
            RoomBody::FileCancel { to: None, file_id } => {
                let by_author = self.inner.files.offers.borrow().get(file_id).is_some_and(|o| o.author == author);
                if by_author {
                    self.abort_download(file_id, FileTransferStatus::Withdrawn);
                }
            }
            RoomBody::FileCancel { to: Some(_), file_id } => {
                if self.inner.files.outgoing.borrow().contains_key(file_id) {
                    // A requester stopped its download: halt a running upload at its next
                    // chunk (a queued one just leaves the line). They may ask again.
                    self.inner.files.active_transfers.borrow_mut().remove(&(file_id.clone(), author.to_string()));
                    self.finish_upload(file_id, author);
                } else if self.inner.files.downloads.borrow().get(file_id).is_some_and(|d| d.author == author) {
                    self.abort_download(file_id, FileTransferStatus::Cancelled);
                }
            }
            _ => {}
        }
    }

    /// The direct link to `remote` is gone: our uploads to them stop (they ask again from
    /// where they are), and our downloads from them pause until the link is back.
    pub(super) fn on_file_peer_lost(&self, remote: &str) {
        self.close_file_links(remote);
        let files = &self.inner.files;
        files.active_transfers.borrow_mut().retain(|(_, peer), _| peer != remote);
        files.starts_at.borrow_mut().retain(|(_, peer), _| peer != remote);
        let now = js_sys::Date::now();
        let paused: Vec<(String, u8)> = files
            .downloads
            .borrow_mut()
            .iter_mut()
            .filter(|(_, d)| d.author == remote)
            .map(|(id, d)| {
                d.paused_since.get_or_insert(now);
                let progress = d.total.map_or(0, |total| (d.reorder.next() as f64 / total as f64 * 100.0) as u8);
                (id.clone(), progress)
            })
            .collect();
        for (file_id, progress) in paused {
            self.set_file_status(&file_id, FileTransferStatus::Paused { progress });
        }
        let started = files.queue.borrow_mut().remove_peer(remote);
        self.after_queue_change(started);
        let mine: Vec<String> = files.outgoing.borrow().keys().cloned().collect();
        for file_id in mine {
            self.refresh_sharing_card(&file_id);
        }
    }

    /// The link to `remote` is open again: paused downloads from them go on from the first
    /// chunk we lack.
    pub(super) fn resume_downloads(&self, remote: &str) {
        let now = js_sys::Date::now();
        let resumed: Vec<(String, u32, u8)> = self
            .inner
            .files
            .downloads
            .borrow_mut()
            .iter_mut()
            .filter(|(_, d)| d.author == remote)
            .filter_map(|(id, d)| {
                let since = d.paused_since.take()?;
                d.paused_ms += now - since;
                d.last_ui_update = now;
                let next = d.reorder.next();
                let progress = d.total.map_or(0, |total| (next as f64 / total as f64 * 100.0) as u8);
                Some((id.clone(), next, progress))
            })
            .collect();
        for (file_id, from_chunk, progress) in resumed {
            log::info!("Resuming a download from chunk {from_chunk}");
            self.set_file_status(&file_id, FileTransferStatus::Downloading { progress, speed_kb: 0, connections: 1 });
            let request = RoomBody::FileRequest { to: remote.to_string(), file_id: file_id.clone(), from_chunk };
            if !self.send_direct(remote, request) {
                if let Some(d) = self.inner.files.downloads.borrow_mut().get_mut(&file_id) {
                    d.paused_since = Some(now);
                }
                self.set_file_status(&file_id, FileTransferStatus::Paused { progress });
            }
        }
    }

    /// `author` is back (from away, or after counting as gone): their cards can be
    /// downloaded again.
    pub(super) fn on_file_author_back(&self, author: &str) {
        let offers = self.inner.files.offers.borrow();
        self.inner.signals.messages.update(|msgs| {
            for msg in msgs.iter_mut() {
                let theirs = msg.file.as_ref().is_some_and(|f| offers.get(&f.file_id).is_some_and(|o| o.author == author));
                if let Some(file) = msg.file.as_mut().filter(|f| theirs && f.status == FileTransferStatus::SenderLeft) {
                    file.status = FileTransferStatus::Offered;
                    msg.rev += 1;
                }
            }
        });
    }

    /// `author` is no longer in the room (left, or away past the grace): their pending
    /// offers can no longer be downloaded, and downloads paused waiting for them end.
    pub(super) fn on_file_author_gone(&self, author: &str) {
        let waiting: Vec<String> = self
            .inner
            .files
            .downloads
            .borrow()
            .iter()
            .filter(|(_, d)| d.author == author)
            .map(|(id, _)| id.clone())
            .collect();
        for file_id in waiting {
            self.abort_download(&file_id, FileTransferStatus::SenderLeft);
        }
        let theirs: Vec<String> = self
            .inner
            .files
            .offers
            .borrow()
            .iter()
            .filter(|(_, o)| o.author == author)
            .map(|(id, _)| id.clone())
            .collect();
        if theirs.is_empty() {
            return;
        }
        self.inner.signals.messages.update(|msgs| {
            for msg in msgs.iter_mut() {
                if let Some(file) = msg.file.as_mut().filter(|f| theirs.contains(&f.file_id)) {
                    if matches!(file.status, FileTransferStatus::Offered | FileTransferStatus::Queued { .. }) {
                        file.status = FileTransferStatus::SenderLeft;
                        msg.rev += 1;
                    }
                }
            }
        });
    }

    pub(super) fn set_file_status(&self, file_id: &str, status: FileTransferStatus) {
        self.inner.signals.messages.update(|msgs| {
            if let Some(msg) = msgs.iter_mut().find(|m| m.id == file_id) {
                if let Some(file) = msg.file.as_mut() {
                    use FileTransferStatus::*;
                    let next = match (&file.status, status) {
                        // A finished transfer stays finished: a withdrawal only means it
                        // can't be downloaded again, and a new download starts over.
                        (Completed { summary, .. }, Withdrawn) => Completed { summary: *summary, withdrawn: true },
                        (ReadyToSave { summary, .. }, Withdrawn) => ReadyToSave { summary: *summary, withdrawn: true },
                        (Completed { .. }, status @ Downloading { .. }) => status,
                        (Completed { .. } | ReadyToSave { .. }, status @ Sharing { .. }) => status,
                        (Completed { .. } | ReadyToSave { .. }, _) => return,
                        (_, status) => status,
                    };
                    // New numbers for the same state don't rebuild the card (its buttons would
                    // be replaced under the pointer several times a second); the card reads them.
                    let same_state = matches!(
                        (&file.status, &next),
                        (Downloading { .. }, Downloading { .. }) | (Sharing { .. }, Sharing { .. })
                    );
                    file.status = next;
                    if !same_state {
                        msg.rev += 1;
                    }
                }
            }
        });
    }
}

async fn try_open_save_stream(suggested_name: &str) -> Option<JsValue> {
    let win = window()?;
    let picker: js_sys::Function = js_sys::Reflect::get(&win, &"showSaveFilePicker".into()).ok()?.dyn_into().ok()?;
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"suggestedName".into(), &suggested_name.into());
    let promise: js_sys::Promise = picker.call1(&win, &opts).ok()?.dyn_into().ok()?;
    let handle = JsFuture::from(promise).await.ok()?;
    let create: js_sys::Function = js_sys::Reflect::get(&handle, &"createWritable".into()).ok()?.dyn_into().ok()?;
    let promise: js_sys::Promise = create.call0(&handle).ok()?.dyn_into().ok()?;
    JsFuture::from(promise).await.ok()
}

/// Call `write(arg)` / `close()` / `abort()` on a `FileSystemWritableFileStream`.
async fn call_stream_method(writable: &JsValue, method: &str, arg: Option<&JsValue>) -> Result<(), JsValue> {
    let f: js_sys::Function = js_sys::Reflect::get(writable, &method.into())?.dyn_into()?;
    let result = match arg {
        Some(arg) => f.call1(writable, arg)?,
        None => f.call0(writable)?,
    };
    JsFuture::from(result.dyn_into::<js_sys::Promise>()?).await?;
    Ok(())
}

/// Chunks in a file of `size` bytes (an empty file is one empty chunk).
fn chunk_count(size: f64) -> u32 {
    ((size / CHUNK_SIZE as f64).ceil() as u32).max(1)
}

fn blob_type(mime_type: &str) -> &str {
    if mime_type.is_empty() {
        "application/octet-stream"
    } else {
        mime_type
    }
}

/// What a tap on Save started.
pub enum SaveStart {
    /// The share sheet is open: the promise resolves once shared and rejects with
    /// `AbortError` when the person closes the sheet.
    Sharing(js_sys::Promise),
    /// No share sheet for files here: the browser took it as a download.
    Downloaded,
    Failed,
}

/// Hand a finished file to the person: the share sheet (Save to Files, AirDrop, …) opens
/// over dchat. Call it straight from the tap, which the share sheet requires.
pub fn start_save(file: &web_sys::File) -> SaveStart {
    let Some(win) = window() else {
        return SaveStart::Failed;
    };
    let nav: JsValue = win.navigator().into();
    let data = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&data, &"files".into(), &js_sys::Array::of1(file));
    let method = |name: &str| js_sys::Reflect::get(&nav, &name.into()).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    let can_share = method("canShare").and_then(|f| f.call1(&nav, &data).ok()).and_then(|v| v.as_bool()) == Some(true);
    if let Some(share) = method("share").filter(|_| can_share) {
        if let Some(promise) = share.call1(&nav, &data).ok().and_then(|p| p.dyn_into::<js_sys::Promise>().ok()) {
            return SaveStart::Sharing(promise);
        }
    }
    match download_blob(file, &file.name()) {
        Ok(()) => SaveStart::Downloaded,
        Err(_) => SaveStart::Failed,
    }
}

/// Whether the file was handed over. A closed share sheet is not: Save stays.
pub async fn save_finished(start: SaveStart) -> bool {
    match start {
        SaveStart::Sharing(promise) => match JsFuture::from(promise).await {
            Ok(_) => true,
            Err(err) => {
                let name = js_sys::Reflect::get(&err, &"name".into()).ok().and_then(|n| n.as_string()).unwrap_or_default();
                if name != "AbortError" {
                    log::warn!("Sharing the file failed: {name}");
                }
                false
            }
        },
        SaveStart::Downloaded => true,
        SaveStart::Failed => false,
    }
}

/// Save a file held in RAM (a viewed photo, a voice message): the save picker where there
/// is one, the share sheet on iOS, else the browser's downloads. Call it from the tap.
pub fn save_blob(blob: &web_sys::Blob, name: &str) {
    if crate::pwa::is_ios() {
        let bag = web_sys::FilePropertyBag::new();
        bag.set_type(&blob.type_());
        if let Ok(file) = web_sys::File::new_with_blob_sequence_and_options(&js_sys::Array::of1(blob), name, &bag) {
            let started = start_save(&file);
            wasm_bindgen_futures::spawn_local(async move {
                save_finished(started).await;
            });
        }
        return;
    }
    let has_picker = window()
        .and_then(|w| js_sys::Reflect::get(&w, &"showSaveFilePicker".into()).ok())
        .is_some_and(|f| f.is_function());
    if !has_picker {
        let _ = download_blob(blob, name);
        return;
    }
    let (blob, name) = (blob.clone(), name.to_string());
    wasm_bindgen_futures::spawn_local(async move {
        // Closing the picker saves nothing, as for any download.
        if let Some(writable) = try_open_save_stream(&name).await {
            if call_stream_method(&writable, "write", Some(&blob.into())).await.is_ok() {
                let _ = call_stream_method(&writable, "close", None).await;
            } else {
                let _ = call_stream_method(&writable, "abort", None).await;
            }
        }
    });
}

fn download_blob(blob: &web_sys::Blob, name: &str) -> Result<(), JsValue> {
    let url = web_sys::Url::create_object_url_with_blob(blob)?;
    let win = window().ok_or("No window")?;
    let doc = win.document().ok_or("No document")?;
    let a: HtmlAnchorElement = doc.create_element("a")?.dyn_into()?;
    a.set_href(&url);
    a.set_download(name);
    let body = doc.body().ok_or("No body")?;
    body.append_child(&a)?;
    a.click();
    body.remove_child(&a)?;
    // Release the in-memory copy once the browser has taken the download.
    let revoke = Closure::once(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    win.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.as_ref().unchecked_ref(), 10_000)?;
    revoke.forget();
    Ok(())
}
