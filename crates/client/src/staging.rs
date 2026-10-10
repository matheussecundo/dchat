//! Files waiting to be sent: picked with 📎, dropped on the room or pasted, up to
//! `MAX_STAGED`. Each gets what its card will show before anyone pulls it (`MediaInfo`:
//! thumbnail, size, length), and photos that record where they were taken say so.

use leptos::html;
use leptos::prelude::*;
use protocol::media::{jpeg_has_gps, media_family, mime_from_name, preview_type, strip_jpeg_gps, MediaInfo, MediaKind, Thumbnail, THUMB_MAX_BYTES};
use std::cell::Cell;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, File};

/// The most files sent at once.
pub const MAX_STAGED: usize = 10;
/// A photo's metadata sits at its start: this much is read to look for a location.
const GPS_SCAN_BYTES: f64 = 256.0 * 1024.0;
/// Thumbnail attempts, largest first, until one fits `THUMB_MAX_BYTES`.
const THUMB_TRIES: [(f64, f64); 4] = [(160.0, 0.7), (120.0, 0.55), (80.0, 0.45), (48.0, 0.4)];
/// Longest wait for a video or audio file to tell its size and length.
const METADATA_TIMEOUT_MS: i32 = 4000;

#[derive(Clone, Debug)]
pub struct StagedFile {
    pub key: u64,
    pub file: File,
    /// `blob:` URL of the file, for the chip's preview (handed to `HeldMedia` once sent).
    pub url: Option<String>,
    pub kind: Option<MediaKind>,
    pub media: Option<MediaInfo>,
    /// The preview is still being made: Send waits for it.
    pub preparing: bool,
    /// A JPEG that records where it was taken.
    pub gps: bool,
}

thread_local! {
    static NEXT_KEY: Cell<u64> = const { Cell::new(1) };
}

/// The type this file's card will declare.
pub fn mime_of(file: &File) -> String {
    let declared = file.type_();
    if declared.is_empty() {
        mime_from_name(&file.name()).unwrap_or_default().to_string()
    } else {
        declared
    }
}

/// Stage `files` after those already there; returns how many didn't fit.
pub fn stage(staged: RwSignal<Vec<StagedFile>, LocalStorage>, files: Vec<File>) -> usize {
    let room = MAX_STAGED.saturating_sub(staged.with_untracked(|s| s.len()));
    let refused = files.len().saturating_sub(room);
    for file in files.into_iter().take(room) {
        let key = NEXT_KEY.with(|k| {
            let key = k.get();
            k.set(key + 1);
            key
        });
        let mime = mime_of(&file);
        let kind = media_family(&mime);
        let jpeg = preview_type(&mime) == Some("image/jpeg");
        let url = kind.and_then(|_| web_sys::Url::create_object_url_with_blob(&file).ok());
        staged.update(|s| {
            s.push(StagedFile { key, file: file.clone(), url: url.clone(), kind, media: None, preparing: kind.is_some(), gps: false })
        });
        if kind.is_none() {
            continue;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let media = match (kind, &url) {
                (Some(kind), Some(url)) => describe(kind, url).await,
                _ => None,
            };
            let gps = jpeg && has_gps(&file).await;
            staged.update(|s| {
                if let Some(item) = s.iter_mut().find(|i| i.key == key) {
                    item.media = media;
                    item.gps = gps;
                    item.preparing = false;
                }
            });
        });
    }
    refused
}

/// Take one file off the list.
pub fn unstage(staged: RwSignal<Vec<StagedFile>, LocalStorage>, key: u64) {
    staged.update(|s| {
        if let Some(at) = s.iter().position(|i| i.key == key) {
            let item = s.remove(at);
            if let Some(url) = item.url {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
        }
    });
}

/// Replace a staged photo with the same photo minus its location.
pub fn remove_location(staged: RwSignal<Vec<StagedFile>, LocalStorage>, key: u64) {
    let Some(file) = staged.with_untracked(|s| s.iter().find(|i| i.key == key).map(|i| i.file.clone())) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let Some(clean) = without_location(&file).await else {
            log::warn!("Could not remove the location from this photo");
            return;
        };
        staged.update(|s| {
            if let Some(item) = s.iter_mut().find(|i| i.key == key) {
                if let Some(url) = item.url.take() {
                    let _ = web_sys::Url::revoke_object_url(&url);
                }
                item.url = web_sys::Url::create_object_url_with_blob(&clean).ok();
                item.file = clean;
                item.gps = false;
            }
        });
    });
}

async fn read_bytes(blob: &web_sys::Blob) -> Option<Vec<u8>> {
    let buffer = JsFuture::from(blob.array_buffer()).await.ok()?;
    Some(js_sys::Uint8Array::new(&buffer).to_vec())
}

async fn has_gps(file: &File) -> bool {
    let Ok(head) = file.slice_with_f64_and_f64(0.0, GPS_SCAN_BYTES.min(file.size())) else {
        return false;
    };
    read_bytes(&head).await.is_some_and(|bytes| jpeg_has_gps(&bytes))
}

async fn without_location(file: &File) -> Option<File> {
    let bytes = read_bytes(file).await?;
    let clean = strip_jpeg_gps(&bytes)?;
    let bag = web_sys::FilePropertyBag::new();
    bag.set_type(&file.type_());
    bag.set_last_modified(file.last_modified());
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(&clean[..]));
    File::new_with_u8_array_sequence_and_options(&parts, &file.name(), &bag).ok()
}

/// Resolve once `ok_event` fires on `target` (true), or `fail_event` / the timeout (false).
async fn wait_for(target: &web_sys::EventTarget, ok_event: &str, fail_event: &str, timeout_ms: i32) -> bool {
    let target = target.clone();
    let (ok_event, fail_event) = (ok_event.to_string(), fail_event.to_string());
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        let on = |value: bool| {
            let resolve = resolve.clone();
            Closure::once_into_js(move || {
                let _ = resolve.call1(&JsValue::NULL, &JsValue::from_bool(value));
            })
        };
        let _ = target.add_event_listener_with_callback_and_add_event_listener_options(&ok_event, on(true).unchecked_ref(), &options);
        let _ = target.add_event_listener_with_callback_and_add_event_listener_options(&fail_event, on(false).unchecked_ref(), &options);
        if let Some(win) = window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(on(false).unchecked_ref(), timeout_ms);
        }
    });
    JsFuture::from(promise).await.ok().and_then(|v| v.as_bool()).unwrap_or(false)
}

/// What the card shows before anyone pulls the file: pixel size, length and a thumbnail
/// drawn from the file itself (so no metadata of the original goes along).
async fn describe(kind: MediaKind, url: &str) -> Option<MediaInfo> {
    let doc = window()?.document()?;
    match kind {
        MediaKind::Image => {
            let img = web_sys::HtmlImageElement::new().ok()?;
            img.set_src(url);
            JsFuture::from(img.decode()).await.ok()?;
            let (w, h) = (img.natural_width(), img.natural_height());
            let thumb = thumbnail(w, h, |ctx, tw, th| ctx.draw_image_with_html_image_element_and_dw_and_dh(&img, 0.0, 0.0, tw, th));
            Some(MediaInfo { kind, width: w, height: h, duration_ms: 0, thumb, waveform: None })
        }
        MediaKind::Video => {
            let video: web_sys::HtmlVideoElement = doc.create_element("video").ok()?.dyn_into().ok()?;
            video.set_muted(true);
            video.set_preload("auto");
            let _ = video.set_attribute("playsinline", "");
            video.set_src(url);
            if !wait_for(&video, "loadeddata", "error", METADATA_TIMEOUT_MS).await {
                return None;
            }
            let (w, h) = (video.video_width(), video.video_height());
            let duration = video.duration();
            let duration_ms = if duration.is_finite() { (duration * 1000.0) as u64 } else { 0 };
            // A frame just after the start: the very first is often black.
            video.set_current_time((duration / 2.0).clamp(0.0, 0.5));
            let thumb = if wait_for(&video, "seeked", "error", METADATA_TIMEOUT_MS).await {
                thumbnail(w, h, |ctx, tw, th| ctx.draw_image_with_html_video_element_and_dw_and_dh(&video, 0.0, 0.0, tw, th))
            } else {
                None
            };
            video.remove_attribute("src").ok();
            video.load();
            Some(MediaInfo { kind, width: w, height: h, duration_ms, thumb, waveform: None })
        }
        MediaKind::Audio | MediaKind::Voice => {
            let audio: web_sys::HtmlAudioElement = doc.create_element("audio").ok()?.dyn_into().ok()?;
            audio.set_preload("metadata");
            audio.set_src(url);
            let duration = if wait_for(&audio, "loadedmetadata", "error", METADATA_TIMEOUT_MS).await { audio.duration() } else { 0.0 };
            audio.remove_attribute("src").ok();
            let duration_ms = if duration.is_finite() { (duration * 1000.0) as u64 } else { 0 };
            Some(MediaInfo { kind: MediaKind::Audio, width: 0, height: 0, duration_ms, thumb: None, waveform: None })
        }
    }
}

/// A small WebP (else JPEG) of a `w`×`h` picture that `draw` paints, at most `THUMB_MAX_BYTES`.
fn thumbnail(
    w: u32,
    h: u32,
    draw: impl Fn(&web_sys::CanvasRenderingContext2d, f64, f64) -> Result<(), JsValue>,
) -> Option<Thumbnail> {
    if w == 0 || h == 0 {
        return None;
    }
    let canvas: web_sys::HtmlCanvasElement = window()?.document()?.create_element("canvas").ok()?.dyn_into().ok()?;
    let ctx: web_sys::CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    for (side, quality) in THUMB_TRIES {
        let scale = (side / w.max(h) as f64).min(1.0);
        let (tw, th) = ((w as f64 * scale).round().max(1.0), (h as f64 * scale).round().max(1.0));
        canvas.set_width(tw as u32);
        canvas.set_height(th as u32);
        draw(&ctx, tw, th).ok()?;
        for mime in ["image/webp", "image/jpeg"] {
            let Ok(data_url) = canvas.to_data_url_with_type_and_encoder_options(mime, &JsValue::from_f64(quality)) else {
                continue;
            };
            // A browser that can't write this type answers with a PNG instead.
            let Some(data) = data_url.strip_prefix(&format!("data:{mime};base64,")) else {
                continue;
            };
            if data.len() / 4 * 3 <= THUMB_MAX_BYTES {
                return Some(Thumbnail { mime_type: mime.to_string(), data: data.to_string() });
            }
        }
    }
    None
}

/// Whether a drag carries files (not text or a link).
pub fn carries_files(transfer: &web_sys::DataTransfer) -> bool {
    transfer.types().iter().any(|t| t.as_string().as_deref() == Some("Files"))
}

/// The files in a drop or a paste, folders left out.
pub fn files_of(transfer: &web_sys::DataTransfer) -> Vec<File> {
    let items = transfer.items();
    let mut files = Vec::new();
    for i in 0..items.length() {
        let Some(item) = items.get(i) else {
            continue;
        };
        if item.kind() != "file" {
            continue;
        }
        if item.webkit_get_as_entry().ok().flatten().is_some_and(|entry| entry.is_directory()) {
            continue;
        }
        if let Ok(Some(file)) = item.get_as_file() {
            files.push(file);
        }
    }
    files
}

/// A pasted screenshot comes without a real name (`image.png`): give it one by date.
pub fn named_for_paste(file: File) -> File {
    let name = file.name();
    let generic = name.is_empty() || ["image.png", "image.jpeg", "image.jpg", "image.gif", "image.webp"].contains(&name.to_ascii_lowercase().as_str());
    if !generic {
        return file;
    }
    let ext = match preview_type(&file.type_()) {
        Some("image/jpeg") => "jpg",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        _ => "png",
    };
    let bag = web_sys::FilePropertyBag::new();
    bag.set_type(&file.type_());
    match File::new_with_blob_sequence_and_options(&js_sys::Array::of1(&file), &format!("paste-{}.{ext}", stamp()), &bag) {
        Ok(renamed) => renamed,
        Err(_) => file,
    }
}

/// `YYYYMMDD-HHMMSS`, local time, for generated file names.
pub fn stamp() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
}

/// Drop every staged file (the tab left the room).
pub fn clear(staged: RwSignal<Vec<StagedFile>, LocalStorage>) {
    staged.update(|s| {
        for item in s.drain(..) {
            if let Some(url) = item.url {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
        }
    });
}

/// A file waiting to be sent: its preview, name and size, the location warning, ✕.
pub fn staged_chip(lang: ReadSignal<crate::i18n::Language>, staged: RwSignal<Vec<StagedFile>, LocalStorage>, item: StagedFile) -> impl IntoView {
    use crate::i18n::t;
    let key = item.key;
    let thumb = item.media.as_ref().and_then(|m| m.thumb.as_ref()).map(|t| format!("data:{};base64,{}", t.mime_type, t.data));
    let preview = match (item.kind, thumb, item.url.clone()) {
        (_, Some(src), _) => view! { <img class="attachment-thumb" src=src alt="" /> }.into_any(),
        _ if item.preparing => view! { <span class="attachment-icon">"⏳"</span> }.into_any(),
        // Our own file: shown as it is (only allow-listed types get a URL).
        (Some(MediaKind::Image), None, Some(url)) => view! { <img class="attachment-thumb" src=url alt="" /> }.into_any(),
        (Some(MediaKind::Audio | MediaKind::Voice), _, Some(url)) => {
            let audio = NodeRef::<html::Audio>::new();
            let (playing, set_playing) = signal(false);
            view! {
                <button class="attachment-play" title=move || t(lang.get(), if playing.get() { "voice_pause" } else { "voice_play" })
                    on:click=move |_| {
                        if let Some(a) = audio.get() {
                            if a.paused() {
                                let _ = a.play();
                            } else {
                                let _ = a.pause();
                            }
                        }
                    }>
                    {move || if playing.get() { "⏸" } else { "▶" }}
                </button>
                <audio class="chat-media" src=url preload="metadata" node_ref=audio
                    on:play=move |_| set_playing.set(true)
                    on:pause=move |_| set_playing.set(false)
                    on:ended=move |_| set_playing.set(false)></audio>
            }
            .into_any()
        }
        (Some(MediaKind::Video), None, _) => view! { <span class="attachment-icon">"🎬"</span> }.into_any(),
        _ => view! { <span class="attachment-icon">"📎"</span> }.into_any(),
    };
    view! {
        <div class="attachment-chip" data-ready=if item.preparing { "false" } else { "true" } data-gps=item.gps.to_string()>
            {preview}
            <span class="attachment-name" dir="auto">{item.file.name()}</span>
            <span class="attachment-size">{format!("({})", crate::state::format_file_size(item.file.size() as u64))}</span>
            {item.gps.then(|| view! {
                <span class="attachment-gps">{move || format!("📍 {}", t(lang.get(), "gps_warning"))}</span>
                <button class="btn btn-sm btn-secondary btn-remove-location" on:click=move |_| remove_location(staged, key)>
                    {move || t(lang.get(), "gps_remove")}
                </button>
            })}
            <button class="btn-remove-attachment" title=move || t(lang.get(), "file_remove_title") on:click=move |_| unstage(staged, key)>
                "✕"
            </button>
        </div>
    }
}
