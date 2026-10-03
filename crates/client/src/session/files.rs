//! Room-wide file sharing. The author posts a file card to the room; every member who
//! wants the file pulls it from the author over their own direct link. Chunks are sealed
//! with the room key (ChaCha20-Poly1305, authenticated header) and never relayed. All
//! transfer state lives in RAM and dies with the link.

use super::RoomSession;
use crate::media::sleep_ms;
use crate::state::{current_time_string, ChatMessageUi, FileOfferInfo, FileTransferStatus};
use leptos::*;
use protocol::{
    decrypt_chunk, encrypt_chunk, QueueDecision, RoomBody, RoomEnvelope, Upload, UploadQueue,
    CHUNK_SIZE, MAX_CONCURRENT_UPLOADS,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, HtmlAnchorElement};

/// Pause sending while this much is still queued on the file channel.
const BACKPRESSURE_HIGH: u32 = 512 * 1024;

struct Offer {
    author: String,
    name: String,
    size: u64,
    mime_type: String,
}

struct Download {
    author: String,
    name: String,
    mime_type: String,
    /// In-memory fallback when the File System Access API is unavailable.
    chunks: Vec<js_sys::Uint8Array>,
    /// `FileSystemWritableFileStream` when the user picked a save location.
    writable: Option<JsValue>,
    started: f64,
    received: u64,
    next_index: u32,
}

pub(super) struct Files {
    offers: RefCell<HashMap<String, Offer>>,
    /// Files this tab offered, still available to request.
    outgoing: RefCell<HashMap<String, web_sys::File>>,
    queue: RefCell<UploadQueue>,
    /// Uploads to stop at the next chunk: cancelled by the requester or link lost.
    stopped: RefCell<HashSet<Upload>>,
    withdrawn: RefCell<HashSet<String>>,
    /// Completed uploads per offered file, for the author's card.
    delivered: RefCell<HashMap<String, usize>>,
    downloads: RefCell<HashMap<String, Download>>,
    /// Test-only pause between chunks, so queues and interruptions are observable.
    #[cfg(feature = "e2e-hooks")]
    pub chunk_delay_ms: std::cell::Cell<i32>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            offers: RefCell::new(HashMap::new()),
            outgoing: RefCell::new(HashMap::new()),
            queue: RefCell::new(UploadQueue::new(MAX_CONCURRENT_UPLOADS)),
            stopped: RefCell::new(HashSet::new()),
            withdrawn: RefCell::new(HashSet::new()),
            delivered: RefCell::new(HashMap::new()),
            downloads: RefCell::new(HashMap::new()),
            #[cfg(feature = "e2e-hooks")]
            chunk_delay_ms: std::cell::Cell::new(0),
        }
    }
}

impl RoomSession {
    // ---- Author side ------------------------------------------------------------------

    /// Post a file card to the room; members then request it individually.
    pub fn share_file(&self, file: web_sys::File, caption: Option<String>) -> Result<(), String> {
        if !self.has_open_link() {
            return Err("No member is connected yet".into());
        }
        let file_id = uuid::Uuid::new_v4().to_string();
        let body = RoomBody::FileOffer {
            file_id: file_id.clone(),
            name: file.name(),
            size: file.size() as u64,
            mime_type: file.type_(),
            caption,
        };
        self.inner.files.outgoing.borrow_mut().insert(file_id, file);
        self.publish(body).map(|_| ()).ok_or_else(|| "Failed to sign file offer".into())
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

    fn on_file_request(&self, requester: &str, file_id: &str) {
        if !self.inner.files.outgoing.borrow().contains_key(file_id) {
            return;
        }
        let decision = self.inner.files.queue.borrow_mut().request(file_id, requester);
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
        let Some(file) = self.inner.files.outgoing.borrow().get(&file_id).cloned() else {
            return;
        };
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let completed = s.upload(&file, &file_id, &peer).await;
            s.inner.files.stopped.borrow_mut().remove(&(file_id.clone(), peer.clone()));
            if completed {
                *s.inner.files.delivered.borrow_mut().entry(file_id.clone()).or_default() += 1;
            }
            s.finish_upload(&file_id, &peer);
        });
    }

    /// Stream `file` to `peer` in sealed chunks; returns whether every chunk was sent.
    async fn upload(&self, file: &web_sys::File, file_id: &str, peer: &str) -> bool {
        let Ok(file_uuid) = uuid::Uuid::parse_str(file_id) else {
            return false;
        };
        let total_size = file.size();
        let total_chunks = ((total_size / CHUNK_SIZE as f64).ceil() as u32).max(1);
        let upload = (file_id.to_string(), peer.to_string());

        for index in 0..total_chunks {
            // Backpressure; a link that closed reports no buffer and ends the upload.
            loop {
                if self.inner.files.stopped.borrow().contains(&upload)
                    || self.inner.files.withdrawn.borrow().contains(file_id)
                {
                    return false;
                }
                match self.link(peer).and_then(|l| l.file_buffered_amount()) {
                    None => return false,
                    Some(buffered) if buffered < BACKPRESSURE_HIGH => break,
                    Some(_) => sleep_ms(15).await,
                }
            }

            let start = index as f64 * CHUNK_SIZE as f64;
            let end = (start + CHUNK_SIZE as f64).min(total_size);
            let Ok(blob) = file.slice_with_f64_and_f64(start, end) else {
                return false;
            };
            let Ok(buffer) = JsFuture::from(blob.array_buffer()).await else {
                return false;
            };
            let plaintext = js_sys::Uint8Array::new(&buffer).to_vec();
            let Ok(packet) = encrypt_chunk(&self.inner.key, file_uuid.as_bytes(), index, total_chunks, &plaintext) else {
                return false;
            };
            if !self.link(peer).is_some_and(|l| l.send_bytes(&packet)) {
                return false;
            }
            #[cfg(feature = "e2e-hooks")]
            {
                let delay = self.inner.files.chunk_delay_ms.get();
                if delay > 0 {
                    sleep_ms(delay).await;
                }
            }
        }
        true
    }

    fn refresh_sharing_card(&self, file_id: &str) {
        let files = &self.inner.files;
        if !files.outgoing.borrow().contains_key(file_id) {
            return;
        }
        let (active, waiting) = files.queue.borrow().counts(file_id);
        let done = files.delivered.borrow().get(file_id).copied().unwrap_or(0);
        self.set_file_status(file_id, FileTransferStatus::Sharing { active, waiting, done });
    }

    // ---- Downloader side --------------------------------------------------------------

    /// Ask the author for the file (needs a direct link). Call from the Download click:
    /// the save-file picker requires the user gesture.
    pub fn download_file(&self, file_id: &str) {
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
        if self.inner.files.downloads.borrow().contains_key(file_id) {
            return;
        }
        if !self.link(&author).is_some_and(|l| l.is_open()) {
            self.toast("file_unreachable");
            return;
        }
        log::info!("Requesting {} ({} bytes)", name, size);
        let s = self.clone();
        let file_id = file_id.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let writable = try_open_save_stream(&name).await;
            s.inner.files.downloads.borrow_mut().insert(
                file_id.clone(),
                Download {
                    author: author.clone(),
                    name,
                    mime_type,
                    chunks: Vec::new(),
                    writable,
                    started: js_sys::Date::now(),
                    received: 0,
                    next_index: 0,
                },
            );
            s.set_file_status(&file_id, FileTransferStatus::Downloading { progress: 0, speed_kb: 0 });
            let request = RoomBody::FileRequest { to: author.clone(), file_id: file_id.clone() };
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

    /// Hide the download buttons for an offer we don't want; nobody is told.
    pub fn decline_file(&self, file_id: &str) {
        self.set_file_status(file_id, FileTransferStatus::Declined);
    }

    fn abort_download(&self, file_id: &str, status: FileTransferStatus) {
        if let Some(download) = self.inner.files.downloads.borrow_mut().remove(file_id) {
            if let Some(writable) = download.writable {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = call_stream_method(&writable, "abort", None).await;
                });
            }
        }
        self.set_file_status(file_id, status);
    }

    pub(super) fn on_file_chunk(&self, from: &str, packet: &[u8]) {
        let Ok((header, plaintext)) = decrypt_chunk(&self.inner.key, packet) else {
            log::warn!("Dropping undecryptable file chunk");
            return;
        };
        let Ok(file_uuid) = uuid::Uuid::from_slice(&header.file_id) else {
            return;
        };
        let file_id = file_uuid.to_string();
        let mut downloads = self.inner.files.downloads.borrow_mut();
        // Only the file's author may feed its download, in order.
        let Some(download) = downloads.get_mut(&file_id).filter(|d| d.author == from) else {
            return;
        };
        if header.chunk_index != download.next_index || header.total_chunks == 0 {
            drop(downloads);
            log::warn!("Out-of-order chunk for {file_id}; aborting download");
            self.abort_download(&file_id, FileTransferStatus::Interrupted);
            return;
        }
        download.next_index += 1;
        download.received += plaintext.len() as u64;
        match &download.writable {
            Some(writable) => {
                let writable = writable.clone();
                let bytes = js_sys::Uint8Array::from(&plaintext[..]);
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = call_stream_method(&writable, "write", Some(&bytes)).await;
                });
            }
            None => download.chunks.push(js_sys::Uint8Array::from(&plaintext[..])),
        }

        if header.chunk_index + 1 < header.total_chunks {
            let elapsed = (js_sys::Date::now() - download.started) / 1000.0;
            let speed_kb = if elapsed > 0.0 { (download.received as f64 / 1024.0 / elapsed) as u64 } else { 0 };
            let progress = ((header.chunk_index + 1) as f64 / header.total_chunks as f64 * 100.0) as u8;
            drop(downloads);
            self.set_file_status(&file_id, FileTransferStatus::Downloading { progress, speed_kb });
            return;
        }

        let Some(done) = downloads.remove(&file_id) else {
            return;
        };
        drop(downloads);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match done.writable {
                Some(writable) => {
                    let _ = call_stream_method(&writable, "close", None).await;
                }
                None => {
                    let _ = trigger_blob_download(&done.name, &done.mime_type, &done.chunks);
                }
            }
            s.set_file_status(&file_id, FileTransferStatus::Completed);
            s.toast("file_download_complete");
        });
    }

    // ---- Messages ---------------------------------------------------------------------

    pub(super) fn on_file_offer(&self, envelope: &RoomEnvelope) {
        let RoomBody::FileOffer { file_id, name, size, mime_type, caption } = &envelope.body else {
            return;
        };
        if self.inner.files.offers.borrow().contains_key(file_id) {
            return;
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
        let status = if is_self {
            FileTransferStatus::Sharing { active: 0, waiting: 0, done: 0 }
        } else {
            FileTransferStatus::Offered
        };
        self.push_message(ChatMessageUi {
            id: file_id.clone(),
            author: envelope.author.clone(),
            is_self,
            text: caption.clone().unwrap_or_default(),
            time: current_time_string(),
            ts: envelope.ts,
            notice: None,
            file: Some(FileOfferInfo {
                file_id: file_id.clone(),
                name: name.clone(),
                size: *size,
                mime_type: mime_type.clone(),
                status,
            }),
        });
    }

    /// Route a file message by its author and (direct-only) recipient.
    pub(super) fn on_file_message(&self, author: &str, body: &RoomBody) {
        match body {
            RoomBody::FileRequest { file_id, .. } => self.on_file_request(author, file_id),
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
                    // chunk (a queued one just leaves the line).
                    if self.inner.files.queue.borrow().is_active(file_id, author) {
                        self.inner.files.stopped.borrow_mut().insert((file_id.clone(), author.to_string()));
                    }
                    self.finish_upload(file_id, author);
                } else if self.inner.files.downloads.borrow().get(file_id).is_some_and(|d| d.author == author) {
                    self.abort_download(file_id, FileTransferStatus::Cancelled);
                }
            }
            _ => {}
        }
    }

    /// The direct link to `remote` is gone: its transfers in either direction stop.
    pub(super) fn on_file_peer_lost(&self, remote: &str) {
        let files = &self.inner.files;
        let downloads: Vec<String> = files
            .downloads
            .borrow()
            .iter()
            .filter(|(_, d)| d.author == remote)
            .map(|(id, _)| id.clone())
            .collect();
        for file_id in downloads {
            self.abort_download(&file_id, FileTransferStatus::Interrupted);
        }
        let started = files.queue.borrow_mut().remove_peer(remote);
        self.after_queue_change(started);
        let mine: Vec<String> = files.outgoing.borrow().keys().cloned().collect();
        for file_id in mine {
            self.refresh_sharing_card(&file_id);
        }
    }

    /// `author` is no longer in the room (left, or unreachable by anyone we can reach):
    /// their pending offers can no longer be downloaded.
    pub(super) fn on_file_author_gone(&self, author: &str) {
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
                    }
                }
            }
        });
    }

    fn set_file_status(&self, file_id: &str, status: FileTransferStatus) {
        self.inner.signals.messages.update(|msgs| {
            if let Some(file) = msgs.iter_mut().find(|m| m.id == file_id).and_then(|m| m.file.as_mut()) {
                // A finished transfer stays finished.
                if !matches!(file.status, FileTransferStatus::Completed) || matches!(status, FileTransferStatus::Sharing { .. }) {
                    file.status = status;
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

fn trigger_blob_download(name: &str, mime_type: &str, chunks: &[js_sys::Uint8Array]) -> Result<(), JsValue> {
    let parts: js_sys::Array = chunks.iter().collect();
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(if mime_type.is_empty() { "application/octet-stream" } else { mime_type });
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
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
