//! Media loaded for viewing in the chat: the bytes of images, videos and voice messages,
//! by file id, as `blob:` URLs the cards show. App level, so a rekey keeps them; RAM only,
//! gone on reload and on the removed screen.
//!
//! Members pull what they view from the author, at most `MEDIA_BUDGET_BYTES` in all: past it
//! the least recently viewed go first and their cards show the thumbnail again. Our own
//! files (`own`) are the files picked or recorded here: never counted, never released.

use std::collections::{HashMap, HashSet};

/// Media up to this size loads by itself once its card is on screen.
pub const AUTO_LOAD_BYTES: u64 = 16 * 1024 * 1024;
/// Larger media can't be viewed in the chat (its bytes would sit in RAM): download only.
pub const INLINE_MAX_BYTES: u64 = 200 * 1024 * 1024;
/// The most loaded media held at once.
pub const MEDIA_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
/// Fresh addresses tried before a file is shown as unplayable here.
pub const MAX_REFRESHES: u8 = 3;

struct Entry {
    blob: web_sys::Blob,
    url: String,
    name: String,
    bytes: u64,
    last_used: u64,
    own: bool,
    /// Fresh addresses given so far (see `refresh`), and whether one is on its way.
    attempts: u8,
    refresh_pending: bool,
}

pub struct HeldMedia {
    entries: HashMap<String, Entry>,
    clock: u64,
    budget: u64,
    /// Open in the viewer or playing: never released.
    protected: HashSet<String>,
}

impl Default for HeldMedia {
    fn default() -> Self {
        Self { entries: HashMap::new(), clock: 0, budget: MEDIA_BUDGET_BYTES, protected: HashSet::new() }
    }
}

impl HeldMedia {
    pub fn url(&self, file_id: &str) -> Option<String> {
        self.entries.get(file_id).map(|e| e.url.clone())
    }

    /// The bytes and the file name, to save them.
    pub fn blob(&self, file_id: &str) -> Option<(web_sys::Blob, String)> {
        self.entries.get(file_id).map(|e| (e.blob.clone(), e.name.clone()))
    }

    /// Hold `blob` under `url` (made from it; this store revokes it), releasing the least
    /// recently viewed others past the budget.
    pub fn insert(&mut self, file_id: &str, blob: web_sys::Blob, url: String, name: &str, own: bool) {
        self.clock += 1;
        let bytes = blob.size() as u64;
        if let Some(old) = self.entries.insert(
            file_id.to_string(),
            Entry { blob, url, name: name.to_string(), bytes, last_used: self.clock, own, attempts: 0, refresh_pending: false },
        ) {
            revoke(&old.url);
        }
        self.release_over_budget(file_id);
    }

    /// Hold the file picked or recorded here (`own`): it stays as long as we offer it.
    pub fn insert_own(&mut self, file_id: &str, file: &web_sys::File, url: Option<String>) {
        let url = url.or_else(|| web_sys::Url::create_object_url_with_blob(file).ok());
        if let Some(url) = url {
            self.insert(file_id, file.clone().into(), url, &file.name(), true);
        }
    }

    /// Release one file: its copy here can't be read (the browser dropped its bytes).
    pub fn remove(&mut self, file_id: &str) {
        if let Some(entry) = self.entries.remove(file_id) {
            revoke(&entry.url);
        }
        self.protected.remove(file_id);
    }

    /// Book a fresh `blob:` address for the same bytes, at most `MAX_REFRESHES` times per file:
    /// browsers sometimes fail to read one (Chrome's bare "MEDIA_ELEMENT_ERROR: Format error")
    /// and read the same bytes fine moments later. Returns the attempt number (1-based) to
    /// wait for, or `None` when none is left (or one is already on its way).
    pub fn book_refresh(&mut self, file_id: &str) -> Option<u8> {
        let entry = self.entries.get_mut(file_id).filter(|e| !e.refresh_pending && e.attempts < MAX_REFRESHES)?;
        entry.refresh_pending = true;
        entry.attempts += 1;
        Some(entry.attempts)
    }

    /// Give the booked fresh address (the card re-renders with it).
    pub fn refresh(&mut self, file_id: &str) {
        let Some(entry) = self.entries.get_mut(file_id) else {
            return;
        };
        entry.refresh_pending = false;
        if let Ok(url) = web_sys::Url::create_object_url_with_blob(&entry.blob) {
            revoke(&std::mem::replace(&mut entry.url, url));
        }
    }

    /// Seen or played just now: released last.
    pub fn touch(&mut self, file_id: &str) {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(file_id) {
            entry.last_used = self.clock;
        }
    }

    pub fn protect(&mut self, file_id: &str, on: bool) {
        if on {
            self.protected.insert(file_id.to_string());
        } else {
            self.protected.remove(file_id);
        }
    }

    /// Test hook: a smaller budget, applied at once.
    #[cfg(feature = "e2e-hooks")]
    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes;
        self.release_over_budget("");
    }

    /// Test hook: ids held, ours included.
    #[cfg(feature = "e2e-hooks")]
    pub fn ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Forget everything (the tab left the room).
    pub fn clear(&mut self) {
        for (_, entry) in self.entries.drain() {
            revoke(&entry.url);
        }
        self.protected.clear();
    }

    fn release_over_budget(&mut self, keep: &str) {
        loop {
            let used: u64 = self.entries.values().filter(|e| !e.own).map(|e| e.bytes).sum();
            if used <= self.budget {
                return;
            }
            let oldest = self
                .entries
                .iter()
                .filter(|(id, e)| !e.own && id.as_str() != keep && !self.protected.contains(id.as_str()))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| id.clone());
            let Some(id) = oldest else {
                return;
            };
            if let Some(entry) = self.entries.remove(&id) {
                log::info!("Released a viewed file from memory ({} bytes)", entry.bytes);
                revoke(&entry.url);
            }
        }
    }
}

fn revoke(url: &str) {
    let _ = web_sys::Url::revoke_object_url(url);
}
