//! Images, videos, audio files and voice messages shown in the chat. A card shows the
//! author's thumbnail until the bytes are here (`held_media`), then the media itself: small
//! files load by themselves once their card is on screen, larger ones on a tap, and anything
//! past `INLINE_MAX_BYTES` stays a download.
//!
//! Previews use only allow-listed types (`protocol::media::preview_type`, never SVG), built
//! into a Blob of that type; whatever the browser fails to decode becomes a plain file card.

use crate::held_media::{HeldMedia, AUTO_LOAD_BYTES, INLINE_MAX_BYTES, MAX_REFRESHES};
use crate::i18n::{t, Language};
use crate::state::{format_file_size, ChatMessageUi, FileOfferInfo, FileTransferStatus, LinkUi, MemberUi};
use crate::FileAction;
use leptos::*;
use protocol::media::{media_family, preview_type, MediaInfo, MediaKind, WAVEFORM_BARS};
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{window, HtmlMediaElement};

/// Largest size a preview takes in the chat.
const FRAME_MAX_W: f64 = 320.0;
const FRAME_MAX_H: f64 = 320.0;
/// Voice message speeds, in turn.
const SPEEDS: [f64; 3] = [1.0, 1.5, 2.0];

/// Shared by every media card.
#[derive(Clone, Copy)]
pub struct MediaCtx {
    pub lang: ReadSignal<Language>,
    pub members: ReadSignal<Vec<MemberUi>>,
    pub held: RwSignal<HeldMedia>,
    /// Files this browser failed to open, with why: their cards say so and keep Download.
    pub failed: RwSignal<HashMap<String, String>>,
    pub viewer: RwSignal<Option<ViewerItem>>,
    pub messages: WriteSignal<Vec<ChatMessageUi>>,
}

/// What the fullscreen viewer shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerItem {
    pub file_id: String,
    pub url: String,
    pub kind: MediaKind,
    pub name: String,
}

/// How a file shows in the chat, when not as a plain file card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preview {
    pub kind: MediaKind,
    /// This browser can play it (always true for images until one fails to decode).
    pub playable: bool,
}

thread_local! {
    /// A detached element, only to ask which types this browser plays.
    static PROBE: Option<HtmlMediaElement> = window()
        .and_then(|w| w.document())
        .and_then(|d| d.create_element("audio").ok())
        .and_then(|e| e.dyn_into().ok());
}

/// Whether this browser says it may play `mime_type` (codecs parameter included).
pub fn can_play(mime_type: &str) -> bool {
    PROBE.with(|probe| probe.as_ref().is_some_and(|p| !p.can_play_type(mime_type).is_empty()))
}

/// How `file` shows: `None` for a plain file card (not an allow-listed type, too large to
/// view, or audio or video this browser says it can't play). One that loaded but failed to
/// open here keeps its card, unplayable: it says why, and Download saves the copy held here.
/// A voice message stays a voice message, with Download instead of ▶ when it can't play.
pub fn preview_of(file: &FileOfferInfo, failed: &HashMap<String, String>) -> Option<Preview> {
    let family = media_family(&file.mime_type)?;
    let kind = match file.media.as_ref().map(|m| m.kind) {
        Some(MediaKind::Voice) if family == MediaKind::Audio => MediaKind::Voice,
        _ => family,
    };
    if failed.contains_key(&file.file_id) {
        return Some(Preview { kind, playable: false });
    }
    let playable = kind == MediaKind::Image || can_play(&file.mime_type) || preview_type(&file.mime_type).is_some_and(can_play);
    match kind {
        MediaKind::Voice => Some(Preview { kind, playable: playable && file.size <= INLINE_MAX_BYTES }),
        _ if !playable || file.size > INLINE_MAX_BYTES => None,
        _ => Some(Preview { kind, playable }),
    }
}

/// `w`×`h` fitted in the chat's preview box, never enlarged; a 4:3 box when unknown.
fn frame_size(media: Option<&MediaInfo>) -> (f64, f64, u32, u32) {
    let (w, h) = match media {
        Some(m) if m.width > 0 && m.height > 0 => (m.width, m.height),
        _ => (4, 3),
    };
    let scale = (FRAME_MAX_W / w as f64).min(FRAME_MAX_H / h as f64).min(if w == 4 && h == 3 { 60.0 } else { 1.0 });
    ((w as f64 * scale).max(48.0), (h as f64 * scale).max(48.0), w, h)
}

/// The author's thumbnail as a `data:` URL (its type is one `MediaInfo::is_valid` allows).
fn thumb_url(media: Option<&MediaInfo>) -> Option<String> {
    let thumb = media?.thumb.as_ref()?;
    Some(format!("data:{};base64,{}", thumb.mime_type, thumb.data))
}

/// `m:ss` for a length in seconds.
pub fn clock(seconds: f64) -> String {
    let total = if seconds.is_finite() { seconds.max(0.0).round() as u64 } else { 0 };
    format!("{}:{:02}", total / 60, total % 60)
}

/// This browser failed to open a file it loaded: its card says so (with the browser's
/// reason) and keeps the bytes, so Download saves them without pulling them again.
fn give_up(ctx: MediaCtx, file_id: &str, mime_type: &str, reason: String) {
    log::warn!("This browser could not open a shared {mime_type} file: {reason}");
    ctx.failed.update(|f| {
        f.insert(file_id.to_string(), reason);
    });
    ctx.messages.update(|msgs| {
        if let Some(m) = msgs.iter_mut().find(|m| m.id == file_id) {
            m.rev += 1;
        }
    });
}

/// An element showing `file_id` failed. An error from an element that no longer shows the
/// current copy (released, or already given a fresh address) is ignored; a real one gets a
/// fresh address for the same bytes a moment later, up to `MAX_REFRESHES` times (Chrome's
/// bare "MEDIA_ELEMENT_ERROR: Format error" means it couldn't read the address, and it often
/// reads the same bytes fine on a retry); after that the card says it can't open it, with
/// the browser's reason and what is held (type, size, still readable or not).
fn media_failed(ctx: MediaCtx, file_id: &str, mime_type: &str, ev: &web_sys::Event, picture: bool) {
    let src = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).and_then(|el| el.get_attribute("src"));
    if src.is_none() || src != ctx.held.with_untracked(|h| h.url(file_id)) {
        return;
    }
    let mut attempt = None;
    ctx.held.update_untracked(|h| attempt = h.book_refresh(file_id));
    if let Some(attempt) = attempt {
        log::info!("Retrying a shared {mime_type} file under a fresh address ({attempt}/{MAX_REFRESHES})");
        let id = file_id.to_string();
        set_timeout(
            move || ctx.held.update(|h| h.refresh(&id)),
            std::time::Duration::from_millis(300 * u64::from(attempt)),
        );
        return;
    }
    let reason = if picture { "the picture could not be decoded".to_string() } else { media_error(ev) };
    let Some((blob, _)) = ctx.held.with_untracked(|h| h.blob(file_id)) else {
        give_up(ctx, file_id, mime_type, reason);
        return;
    };
    // Can the copy still be read? Browsers cap the file data a page holds in memory (less in
    // private windows and on phones); past it, bytes can be dropped.
    let (file_id, mime_type) = (file_id.to_string(), mime_type.to_string());
    wasm_bindgen_futures::spawn_local(async move {
        let held_type = blob.type_();
        let held_type = if held_type.is_empty() { "no type".to_string() } else { held_type };
        let size = format_file_size(blob.size() as u64);
        match readable(&blob).await {
            Ok(()) => give_up(ctx, &file_id, &mime_type, format!("{reason} ({held_type}, {size} in memory, readable)")),
            Err(why) => {
                // Gone: Download pulls it again from the sender instead of saving a broken copy.
                ctx.held.update(|h| h.remove(&file_id));
                ctx.messages.update(|msgs| {
                    if let Some(file) = msgs.iter_mut().find(|m| m.id == file_id).and_then(|m| m.file.as_mut()) {
                        if matches!(file.status, FileTransferStatus::Completed { withdrawn: false, .. }) {
                            file.status = FileTransferStatus::Offered;
                        }
                    }
                });
                give_up(ctx, &file_id, &mime_type, format!("{reason}: the downloaded copy can't be read ({why}; {held_type}, {size})"));
            }
        }
    });
}

/// Read the first and last bytes of `blob`: `Err` with the browser's reason when it can't.
async fn readable(blob: &web_sys::Blob) -> Result<(), String> {
    let size = blob.size();
    if size <= 0.0 {
        return Err("empty".to_string());
    }
    for (start, end) in [(0.0, size.min(16.0)), ((size - 16.0).max(0.0), size)] {
        let part = blob.slice_with_f64_and_f64(start, end).map_err(|_| "slice failed".to_string())?;
        if let Err(err) = wasm_bindgen_futures::JsFuture::from(part.array_buffer()).await {
            let name = js_sys::Reflect::get(&err, &"name".into()).ok().and_then(|n| n.as_string());
            return Err(name.unwrap_or_else(|| "read failed".to_string()));
        }
    }
    Ok(())
}

/// Why an `<audio>` or `<video>` failed, as the browser puts it.
fn media_error(ev: &web_sys::Event) -> String {
    let error = ev.target().and_then(|t| t.dyn_into::<HtmlMediaElement>().ok()).and_then(|el| el.error());
    match error {
        Some(error) => {
            let code = match error.code() {
                1 => "aborted",
                2 => "network error",
                3 => "decode error",
                4 => "format not supported",
                _ => "error",
            };
            let message = error.message();
            if message.is_empty() {
                code.to_string()
            } else {
                format!("{code}: {message}")
            }
        }
        None => "error".to_string(),
    }
}

/// Track whether the element behind `node` is on screen.
fn watch_visibility(node: NodeRef<html::Div>, visible: RwSignal<bool>) {
    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
        for entry in entries.iter() {
            let entry: web_sys::IntersectionObserverEntry = entry.unchecked_into();
            visible.set(entry.is_intersecting());
        }
    });
    let observer = web_sys::IntersectionObserver::new(callback.as_ref().unchecked_ref()).ok();
    if let Some(observer) = observer.clone() {
        node.on_load(move |el| {
            let el: &web_sys::HtmlDivElement = &el;
            observer.observe(el);
        });
    }
    on_cleanup(move || {
        if let Some(observer) = observer {
            observer.disconnect();
        }
        drop(callback);
    });
}

/// An image, video, audio file or voice message card. `actions`: the status line and
/// buttons, shared with plain file cards (Download, Cancel, the sharing counts…).
#[allow(clippy::too_many_arguments)]
pub fn media_card(
    ctx: MediaCtx,
    preview: Preview,
    file: FileOfferInfo,
    live: Memo<Option<FileTransferStatus>>,
    caption: String,
    author: String,
    is_self: bool,
    actions: View,
    on_action: impl Fn(FileAction, String) + Copy + 'static,
) -> impl IntoView {
    let lang = ctx.lang;
    let id = file.file_id.clone();
    let url = {
        let id = id.clone();
        create_memo(move |_| ctx.held.with(|h| h.url(&id)))
    };
    let reachable = {
        let author = author.clone();
        create_memo(move |_| ctx.members.with(|m| m.iter().any(|x| x.pubkey == author && x.link == LinkUi::Direct)))
    };
    // Small media loads by itself once its card is on screen (and its author reachable).
    let visible = create_rw_signal(false);
    let requested = store_value(false);
    {
        let id = id.clone();
        let size = file.size;
        create_effect(move |_| {
            if !visible.get() {
                return;
            }
            ctx.held.update_untracked(|h| h.touch(&id));
            if is_self || requested.get_value() || url.get().is_some() || size > AUTO_LOAD_BYTES || !preview.playable {
                return;
            }
            if matches!(live.get(), Some(FileTransferStatus::Offered)) && reachable.get() {
                requested.set_value(true);
                on_action(FileAction::Load, id.clone());
            }
        });
    }
    let kind_attr = match preview.kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Voice => "voice",
    };
    let body = match preview.kind {
        MediaKind::Voice => voice_bubble(ctx, &file, url, preview.playable, live, visible).into_view(),
        MediaKind::Audio => {
            let frame = create_node_ref::<html::Div>();
            watch_visibility(frame, visible);
            let id = id.clone();
            let mime = file.mime_type.clone();
            view! {
                <div class="media-audio-frame" node_ref=frame>
                    {move || match url.get().filter(|_| preview.playable) {
                        Some(src) => {
                            let (id, mime) = (id.clone(), mime.clone());
                            view! {
                                <audio class="media-audio chat-media" src=src controls=true preload="metadata"
                                    on:error=move |ev| media_failed(ctx, &id, &mime, &ev, false)></audio>
                            }.into_view()
                        }
                        None => view! { <div class="media-audio-placeholder">"🎵"</div> }.into_view(),
                    }}
                </div>
            }
            .into_view()
        }
        MediaKind::Image | MediaKind::Video => {
            visual_frame(ctx, preview, &file, url, live, reachable, visible, on_action).into_view()
        }
    };
    // Why it can't open: the browser's error, or (refused before loading) its declared type.
    let reason = ctx
        .failed
        .with_untracked(|f| f.get(&id).cloned())
        .or_else(|| (!preview.playable).then(|| format!("{} ({})", file.mime_type, format_file_size(file.size))));
    view! {
        <div class=format!("file-card media-card media-{kind_attr}") data-file-id=id.clone() data-media-kind=kind_attr>
            {body}
            {(!caption.is_empty()).then(|| view! { <div class="file-caption" dir="auto">{caption}</div> })}
            <div class="media-footer">
                <span class="media-name" dir="auto" title=file.name.clone()>{file.name.clone()}</span>
                <span class="file-size">{format_file_size(file.size)}</span>
            </div>
            <div class="file-card-actions">{actions}</div>
            {(!preview.playable).then(|| view! {
                <div class="media-cant-play">
                    <span class="file-status-text cancelled">{move || t(lang.get(), "media_cant_play")}</span>
                    {reason.map(|why| view! { <span class="media-error-detail">{why}</span> })}
                </div>
            })}
        </div>
    }
}

/// An image or a video: the thumbnail with a load or progress overlay, then the media.
#[allow(clippy::too_many_arguments)]
fn visual_frame(
    ctx: MediaCtx,
    preview: Preview,
    file: &FileOfferInfo,
    url: Memo<Option<String>>,
    live: Memo<Option<FileTransferStatus>>,
    reachable: Memo<bool>,
    visible: RwSignal<bool>,
    on_action: impl Fn(FileAction, String) + Copy + 'static,
) -> impl IntoView {
    let lang = ctx.lang;
    let kind = preview.kind;
    let mime = file.mime_type.clone();
    let frame = create_node_ref::<html::Div>();
    watch_visibility(frame, visible);
    let (width, _, w, h) = frame_size(file.media.as_ref());
    let style = format!("width: {width:.0}px; aspect-ratio: {w} / {h};");
    let thumb = thumb_url(file.media.as_ref());
    let id = file.file_id.clone();
    let name = file.name.clone();
    let size = file.size;
    let open = {
        let (id, name) = (id.clone(), name.clone());
        move |src: String| {
            ctx.held.update_untracked(|h| {
                h.touch(&id);
                h.protect(&id, true);
            });
            ctx.viewer.set(Some(ViewerItem { file_id: id.clone(), url: src, kind, name: name.clone() }));
        }
    };
    let placeholder = {
        let (id, thumb) = (id.clone(), thumb.clone());
        move || {
            let id = id.clone();
            let overlay = move || match live.get() {
                // Couldn't open here: said below the thumbnail.
                _ if !preview.playable => ().into_view(),
                // The numbers, speed and Cancel are below, as on any file card; the ring
                // follows the live progress.
                Some(FileTransferStatus::Downloading { .. }) => {
                    let progress = move || match live.get() {
                        Some(FileTransferStatus::Downloading { progress, .. }) => progress,
                        _ => 0,
                    };
                    view! {
                        <div class="media-progress" role="progressbar" aria-valuenow=progress>
                            <span class="media-progress-ring" style=move || format!("--p: {};", progress())></span>
                            <span class="media-progress-label">{move || format!("{}%", progress())}</span>
                        </div>
                    }
                    .into_view()
                }
                Some(FileTransferStatus::Queued { .. }) => view! { <span class="media-progress-ring waiting"></span> }.into_view(),
                // Said below the picture (withdrawn, sender left): the thumbnail stays.
                Some(FileTransferStatus::Withdrawn | FileTransferStatus::SenderLeft | FileTransferStatus::Completed { withdrawn: true, .. }) => {
                    ().into_view()
                }
                _ if reachable.get() => {
                    let id = id.clone();
                    let icon = if kind == MediaKind::Video { "▶" } else { "⬇" };
                    view! {
                        <button class="media-load-btn" on:click=move |_| on_action(FileAction::Load, id.clone())>
                            <span class="media-load-icon">{icon}</span>
                            <span class="media-load-size">{format_file_size(size)}</span>
                        </button>
                    }
                    .into_view()
                }
                _ => ().into_view(),
            };
            view! {
                {thumb.clone().map(|src| view! { <img class="media-thumb" src=src alt="" /> })}
                <div class="media-overlay">{overlay}</div>
            }
            .into_view()
        }
    };
    view! {
        <div class="media-frame" node_ref=frame style=style>
            {move || match url.get().filter(|_| preview.playable) {
                Some(src) => {
                    let (id, open, mime) = (id.clone(), open.clone(), mime.clone());
                    match kind {
                        MediaKind::Video => {
                            let expand = src.clone();
                            view! {
                                <video class="media-video chat-media" src=src controls=true preload="metadata" playsinline=true
                                    poster=thumb.clone() on:error=move |ev| media_failed(ctx, &id, &mime, &ev, false)></video>
                                <button class="media-expand" title=move || t(lang.get(), "title_fullscreen")
                                    on:click=move |_| open(expand.clone())>"⛶"</button>
                            }
                            .into_view()
                        }
                        _ => {
                            let full = src.clone();
                            view! {
                                <img class="media-image" src=src alt=name.clone() decoding="async"
                                    on:click=move |_| open(full.clone())
                                    on:error=move |ev| media_failed(ctx, &id, &mime, &ev, true) />
                            }
                            .into_view()
                        }
                    }
                }
                None => placeholder(),
            }}
        </div>
    }
}

/// A voice message: ▶, its waveform (tap to seek), its length and speed.
fn voice_bubble(
    ctx: MediaCtx,
    file: &FileOfferInfo,
    url: Memo<Option<String>>,
    playable: bool,
    live: Memo<Option<FileTransferStatus>>,
    visible: RwSignal<bool>,
) -> impl IntoView {
    let lang = ctx.lang;
    let bubble = create_node_ref::<html::Div>();
    watch_visibility(bubble, visible);
    let audio = create_node_ref::<html::Audio>();
    let (playing, set_playing) = create_signal(false);
    let (position, set_position) = create_signal(0.0_f64);
    let (speed, set_speed) = create_signal(0_usize);
    let media = file.media.clone();
    let (duration, set_duration) = create_signal(media.as_ref().map_or(0.0, |m| m.duration_ms as f64 / 1000.0));
    let bars: Vec<u8> = media
        .as_ref()
        .and_then(|m| m.waveform.clone())
        .filter(|w| w.len() == WAVEFORM_BARS)
        .unwrap_or_else(|| vec![96; WAVEFORM_BARS]);
    let id = file.file_id.clone();
    let mime = file.mime_type.clone();
    let element = move || audio.get().map(|a| {
        let el: &HtmlMediaElement = a.as_ref();
        el.clone()
    });
    let ready = move || playable && url.get().is_some();
    let toggle = move |_| {
        let Some(el) = element() else {
            return;
        };
        if el.paused() {
            el.set_playback_rate(SPEEDS[speed.get_untracked()]);
            let _ = el.play();
        } else {
            let _ = el.pause();
        }
    };
    let wave = create_node_ref::<html::Div>();
    let seek = move |ev: web_sys::PointerEvent| {
        let (Some(el), Some(wave)) = (element(), wave.get()) else {
            return;
        };
        let rect = wave.get_bounding_client_rect();
        let mut fraction = ((ev.client_x() as f64 - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        // Right-to-left layouts fill the waveform from the right.
        if window().and_then(|w| w.document()).and_then(|d| d.document_element()).and_then(|e| e.get_attribute("dir")).as_deref() == Some("rtl") {
            fraction = 1.0 - fraction;
        }
        let total = duration.get_untracked();
        if total > 0.0 {
            el.set_current_time(fraction * total);
            set_position.set(fraction * total);
        }
    };
    let cycle_speed = move |_| {
        let next = (speed.get_untracked() + 1) % SPEEDS.len();
        set_speed.set(next);
        if let Some(el) = element() {
            el.set_playback_rate(SPEEDS[next]);
        }
    };
    let loading = move || matches!(live.get(), Some(FileTransferStatus::Downloading { .. } | FileTransferStatus::Queued { .. }));
    view! {
        <div class="voice-bubble" node_ref=bubble class:playing=playing>
            <button class="voice-play" disabled=move || !ready()
                title=move || t(lang.get(), if playing.get() { "voice_pause" } else { "voice_play" })
                on:click=toggle>
                {move || if loading() { "⏳" } else if playing.get() { "⏸" } else { "▶" }}
            </button>
            <div class="voice-wave" node_ref=wave on:pointerdown=seek>
                {bars.into_iter().enumerate().map(|(i, level)| {
                    let height = 12 + level as u32 * 88 / 255;
                    let played = move || {
                        let total = duration.get();
                        total > 0.0 && (i as f64 + 0.5) / WAVEFORM_BARS as f64 <= position.get() / total
                    };
                    view! { <span class="voice-bar" class:played=played style=format!("height: {height}%;")></span> }
                }).collect_view()}
            </div>
            <span class="voice-time">
                {move || if playing.get() || position.get() > 0.0 { clock(position.get()) } else { clock(duration.get()) }}
            </span>
            <button class="voice-speed" title=move || t(lang.get(), "voice_speed") on:click=cycle_speed>
                {move || match speed.get() { 0 => "1×", 1 => "1.5×", _ => "2×" }}
            </button>
            {move || url.get().filter(|_| playable).map(|src| {
                let (id, mime) = (id.clone(), mime.clone());
                view! {
                    <audio class="voice-audio chat-media" src=src preload="metadata" node_ref=audio
                        on:play=move |_| set_playing.set(true)
                        on:pause=move |_| set_playing.set(false)
                        on:ended=move |_| {
                            set_playing.set(false);
                            set_position.set(0.0);
                        }
                        on:timeupdate=move |ev| {
                            let el: HtmlMediaElement = event_target(&ev);
                            set_position.set(el.current_time());
                        }
                        on:loadedmetadata=move |ev| {
                            let el: HtmlMediaElement = event_target(&ev);
                            let length = el.duration();
                            if duration.get_untracked() <= 0.0 && length.is_finite() && length > 0.0 {
                                set_duration.set(length);
                            }
                        }
                        on:error=move |ev| media_failed(ctx, &id, &mime, &ev, false)></audio>
                }
            })}
        </div>
    }
}

/// The fullscreen viewer over everything: the image or the video, its name, Download and ✕.
pub fn media_viewer(ctx: MediaCtx, on_download: impl Fn(String) + Copy + 'static) -> impl IntoView {
    let lang = ctx.lang;
    let close_ref = create_node_ref::<html::Button>();
    // Focus ✕ when the viewer opens, so Enter or Esc closes it straight away.
    create_effect(move |_| {
        if ctx.viewer.with(|v| v.is_some()) {
            request_animation_frame(move || {
                if let Some(button) = close_ref.get_untracked() {
                    let _ = button.focus();
                }
            });
        }
    });
    move || {
        ctx.viewer.get().map(|item| {
            let id = item.file_id.clone();
            let media = match item.kind {
                MediaKind::Video => view! {
                    <video class="media-viewer-video chat-media" src=item.url.clone() controls=true autoplay=true playsinline=true></video>
                }
                .into_view(),
                _ => view! { <img class="media-viewer-image" src=item.url.clone() alt=item.name.clone() /> }.into_view(),
            };
            view! {
                <div class="media-viewer" role="dialog" aria-modal="true"
                    on:click=move |ev| {
                        // A tap on the backdrop closes it; taps on the media and buttons don't.
                        // (Leptos delegates clicks: `currentTarget` is the document.)
                        let backdrop = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).is_some_and(|el| {
                            let classes = el.class_list();
                            classes.contains("media-viewer") || classes.contains("media-viewer-stage")
                        });
                        if backdrop {
                            close_viewer(ctx);
                        }
                    }>
                    <div class="media-viewer-bar">
                        <span class="media-viewer-name" dir="auto">{item.name.clone()}</span>
                        <button class="btn btn-secondary media-viewer-download" on:click=move |_| on_download(id.clone())>
                            {move || t(lang.get(), "file_download")}
                        </button>
                        <button class="btn btn-secondary media-viewer-close" node_ref=close_ref
                            title=move || t(lang.get(), "title_close") on:click=move |_| close_viewer(ctx)>"✕"</button>
                    </div>
                    <div class="media-viewer-stage">{media}</div>
                </div>
            }
        })
    }
}

pub fn close_viewer(ctx: MediaCtx) {
    if let Some(item) = ctx.viewer.get_untracked() {
        ctx.held.update_untracked(|h| h.protect(&item.file_id, false));
        ctx.viewer.set(None);
    }
}

/// One player at a time: starting any chat audio or video pauses the others; a voice
/// message that ends starts the next one right below it, when it is loaded.
pub fn install_playback_rules() {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return;
    };
    let on_play = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let Some(started) = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
            return;
        };
        if !started.class_list().contains("chat-media") {
            return;
        }
        let Some(doc) = window().and_then(|w| w.document()) else {
            return;
        };
        if let Ok(all) = doc.query_selector_all(".chat-media") {
            for i in 0..all.length() {
                let Some(other) = all.get(i).and_then(|n| n.dyn_into::<HtmlMediaElement>().ok()) else {
                    continue;
                };
                let same: &web_sys::Element = other.as_ref();
                if same != &started && !other.paused() {
                    let _ = other.pause();
                }
            }
        }
    });
    let on_ended = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let Some(ended) = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
            return;
        };
        if !ended.class_list().contains("voice-audio") {
            return;
        }
        let next = ended
            .closest(".message-row")
            .ok()
            .flatten()
            .and_then(|row| row.next_element_sibling())
            .and_then(|row| row.query_selector("audio.voice-audio").ok().flatten())
            .and_then(|a| a.dyn_into::<HtmlMediaElement>().ok());
        if let Some(next) = next {
            let _ = next.play();
        }
    });
    let options = web_sys::AddEventListenerOptions::new();
    options.set_capture(true);
    let _ = doc.add_event_listener_with_callback_and_add_event_listener_options("play", on_play.as_ref().unchecked_ref(), &options);
    let _ = doc.add_event_listener_with_callback_and_add_event_listener_options("ended", on_ended.as_ref().unchecked_ref(), &options);
    on_play.forget();
    on_ended.forget();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_size_fits_the_box_without_enlarging() {
        let media = |w, h| MediaInfo { kind: MediaKind::Image, width: w, height: h, duration_ms: 0, thumb: None, waveform: None };
        assert_eq!(frame_size(Some(&media(4000, 3000))), (320.0, 240.0, 4000, 3000));
        assert_eq!(frame_size(Some(&media(1080, 1920))), (180.0, 320.0, 1080, 1920));
        assert_eq!(frame_size(Some(&media(100, 50))), (100.0, 50.0, 100, 50));
        assert_eq!(frame_size(Some(&media(10, 10))), (48.0, 48.0, 10, 10), "tiny images stay tappable");
        assert_eq!(frame_size(None), (240.0, 180.0, 4, 3));
    }

    #[test]
    fn test_preview_of_images() {
        let file = |mime: &str, size: u64| FileOfferInfo {
            file_id: "f".into(),
            name: "x".into(),
            size,
            mime_type: mime.into(),
            media: None,
            status: FileTransferStatus::Offered,
        };
        let none = HashMap::new();
        assert_eq!(preview_of(&file("image/png", 1000), &none), Some(Preview { kind: MediaKind::Image, playable: true }));
        assert_eq!(preview_of(&file("IMAGE/JPG", INLINE_MAX_BYTES), &none).map(|p| p.kind), Some(MediaKind::Image));
        assert_eq!(preview_of(&file("image/png", INLINE_MAX_BYTES + 1), &none), None, "too large to view: a download");
        assert_eq!(preview_of(&file("image/svg+xml", 10), &none), None, "never SVG");
        assert_eq!(preview_of(&file("application/pdf", 10), &none), None);
        assert_eq!(preview_of(&file("", 10), &none), None);
        let failed = HashMap::from([("f".to_string(), "decode error".to_string())]);
        assert_eq!(
            preview_of(&file("image/png", 10), &failed),
            Some(Preview { kind: MediaKind::Image, playable: false }),
            "failed to open here: the card stays, saying so"
        );
    }

    #[test]
    fn test_clock() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(61.4), "1:01");
        assert_eq!(clock(899.6), "15:00");
        assert_eq!(clock(f64::INFINITY), "0:00");
    }
}
