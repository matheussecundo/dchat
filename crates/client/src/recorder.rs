//! Voice messages: record from the microphone, listen back, then send. Tap 🎤 to record
//! hands-free, or hold it (slide left to cancel, up to lock); stopping always leads to the
//! review, never straight into the chat. The recording is a Blob in this tab's RAM: once
//! sent it is offered like any shared file, so members pull it from this device.
//!
//! Every browser records MP3: a small AudioWorklet (`TAP_JS`) hands the microphone's samples
//! to the encoder in `voice_mp3` as they arrive. Browsers' own recorders make Opus in MP4 or
//! WebM, which some players (Brave on Android, older iPhones) can't open.

use crate::media::capture_microphone;
use crate::media_card::clock;
use crate::state::AudioSettings;
use crate::voice_mp3::{VoiceEncoder, VOICE_MIME};
use leptos::*;
use protocol::media::{waveform_from_levels, MediaInfo, MediaKind};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, AudioContext, AudioWorkletNode, Blob, MediaStream, MediaStreamAudioSourceNode, MediaStreamTrack};

/// Longest voice message: recording stops by itself there and goes to the review.
pub const MAX_RECORDING_MS: f64 = 15.0 * 60_000.0;
/// From here the timer warns that the end is near.
pub const WARN_BEFORE_MS: f64 = 30_000.0;
/// How long stopping waits for the audio thread's last samples.
const FLUSH_TIMEOUT_MS: i32 = 500;

/// The audio thread's side, loaded from a Blob (no extra file to host or cache): it mixes
/// the microphone to mono and posts blocks of 2048 samples (about 43 ms); any message from
/// the page makes it post what it still holds, then "end", and stop.
const TAP_JS: &str = r#"
class DchatVoiceTap extends AudioWorkletProcessor {
  constructor() {
    super();
    this.block = new Float32Array(2048);
    this.filled = 0;
    this.ended = false;
    this.port.onmessage = () => {
      if (this.ended) return;
      this.port.postMessage(this.block.slice(0, this.filled));
      this.port.postMessage("end");
      this.ended = true;
    };
  }
  process(inputs) {
    if (this.ended) return false;
    const channels = inputs[0];
    if (channels && channels.length > 0) {
      const frames = channels[0].length;
      for (let i = 0; i < frames; i++) {
        let sum = 0;
        for (let c = 0; c < channels.length; c++) sum += channels[c][i];
        this.block[this.filled++] = sum / channels.length;
        if (this.filled === this.block.length) {
          this.port.postMessage(this.block, [this.block.buffer]);
          this.block = new Float32Array(2048);
          this.filled = 0;
        }
      }
    }
    return true;
  }
}
registerProcessor("dchat-voice-tap", DchatVoiceTap);
"#;
const TAP_NAME: &str = "dchat-voice-tap";

/// A finished recording, waiting for Send or Discard.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub blob: Blob,
    /// `blob:` URL to listen back (handed to `HeldMedia` once sent).
    pub url: String,
    pub duration_ms: u64,
    pub waveform: Vec<u8>,
}

impl Take {
    /// The file members will pull, named by date.
    pub fn file(&self) -> Option<web_sys::File> {
        let bag = web_sys::FilePropertyBag::new();
        bag.set_type(VOICE_MIME);
        let name = format!("voice-{}.mp3", crate::staging::stamp());
        web_sys::File::new_with_blob_sequence_and_options(&js_sys::Array::of1(&self.blob), &name, &bag).ok()
    }

    pub fn media(&self) -> MediaInfo {
        MediaInfo {
            kind: MediaKind::Voice,
            width: 0,
            height: 0,
            duration_ms: self.duration_ms,
            thumb: None,
            waveform: Some(self.waveform.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// Waiting for the microphone. `held`: 🎤 is being held down.
    Starting { held: bool },
    Recording { held: bool, elapsed_ms: f64, level: f32 },
    Review(Take),
}

impl Phase {
    /// The microphone is open (or opening) for a recording.
    pub fn capturing(&self) -> bool {
        matches!(self, Phase::Starting { .. } | Phase::Recording { .. })
    }
}

/// Everything open while recording.
struct Live {
    /// Tells this recording apart from a later one (a late flush timeout finds nothing).
    id: u64,
    track: MediaStreamTrack,
    ctx: AudioContext,
    source: MediaStreamAudioSourceNode,
    node: AudioWorkletNode,
    encoder: VoiceEncoder,
    /// Level of each block, for the waveform.
    levels: Vec<f32>,
    stopping: bool,
    cancelled: bool,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_ended: Closure<dyn FnMut(web_sys::Event)>,
}

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

#[derive(Clone, Copy)]
pub struct Recorder {
    pub phase: RwSignal<Phase>,
    live: StoredValue<Option<Live>>,
    /// An audio context made during a tap or press (browsers only start audio from one).
    prepared: StoredValue<Option<AudioContext>>,
    cap_ms: StoredValue<f64>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            phase: create_rw_signal(Phase::Idle),
            live: store_value(None),
            prepared: store_value(None),
            cap_ms: store_value(MAX_RECORDING_MS),
        }
    }

    /// Test hook: a shorter maximum.
    #[cfg(feature = "e2e-hooks")]
    pub fn set_cap_ms(self, ms: f64) {
        self.cap_ms.set_value(ms);
    }

    pub fn cap_ms(self) -> f64 {
        self.cap_ms.get_value()
    }

    /// Make the audio context now, from the tap or press: a hold starts recording only after
    /// a moment, outside the gesture the browser wants for audio.
    pub fn prepare(self) {
        if self.prepared.with_value(Option::is_none) {
            if let Ok(ctx) = AudioContext::new() {
                let _ = ctx.resume();
                self.prepared.set_value(Some(ctx));
            }
        }
    }

    /// Open the microphone and record. Call it from the tap or press (see `prepare`).
    pub fn start(self, held: bool, settings: AudioSettings, device: Option<String>, toast: WriteSignal<Option<&'static str>>) {
        if self.phase.get_untracked() != Phase::Idle {
            return;
        }
        self.phase.set(Phase::Starting { held });
        self.prepare();
        let mut ctx = None;
        self.prepared.update_value(|p| ctx = p.take());
        wasm_bindgen_futures::spawn_local(async move {
            let captured = capture_microphone(&settings, device.as_deref()).await;
            let release = |track: Option<&MediaStreamTrack>, ctx: Option<AudioContext>| {
                if let Some(track) = track {
                    track.stop();
                }
                if let Some(ctx) = ctx {
                    let _ = ctx.close();
                }
            };
            // Released or cancelled while the microphone was opening.
            if !matches!(self.phase.get_untracked(), Phase::Starting { .. }) {
                release(captured.as_ref().ok().map(|c| &c.track), ctx);
                return;
            }
            let captured = match captured {
                Ok(captured) => captured,
                Err(_) => {
                    release(None, ctx);
                    toast.set(Some("toast_mic_denied"));
                    self.phase.set(Phase::Idle);
                    return;
                }
            };
            if captured.fell_back {
                toast.set(Some("toast_mic_device_missing"));
            }
            let Some(ctx) = ctx else {
                release(Some(&captured.track), None);
                toast.set(Some("toast_record_failed"));
                self.phase.set(Phase::Idle);
                return;
            };
            if let Err(err) = self.begin(captured.track.clone(), ctx.clone()).await {
                log::warn!("Could not start recording: {err:?}");
                release(Some(&captured.track), Some(ctx));
                toast.set(Some("toast_record_failed"));
                self.phase.set(Phase::Idle);
            }
        });
    }

    async fn begin(self, track: MediaStreamTrack, ctx: AudioContext) -> Result<(), JsValue> {
        let _ = ctx.resume();
        let bag = web_sys::BlobPropertyBag::new();
        bag.set_type("text/javascript");
        let module = Blob::new_with_str_sequence_and_options(&js_sys::Array::of1(&TAP_JS.into()), &bag)?;
        let url = web_sys::Url::create_object_url_with_blob(&module)?;
        let loaded = match ctx.audio_worklet()?.add_module(&url) {
            Ok(promise) => JsFuture::from(promise).await.map(|_| ()),
            Err(err) => Err(err),
        };
        let _ = web_sys::Url::revoke_object_url(&url);
        loaded?;
        // Cancelled while the module loaded.
        let Phase::Starting { held } = self.phase.get_untracked() else {
            track.stop();
            let _ = ctx.close();
            return Ok(());
        };
        let stream = MediaStream::new_with_tracks(&js_sys::Array::of1(&track))?;
        let source = ctx.create_media_stream_source(&stream)?;
        let node = AudioWorkletNode::new(&ctx, TAP_NAME)?;
        source.connect_with_audio_node(&node)?;
        // Its output is silence; connected so the browser keeps running it.
        node.connect_with_audio_node(&ctx.destination())?;
        let id = NEXT_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| self.on_block(id, ev.data()));
        node.port()?.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        // The microphone went away (unplugged, permission revoked): keep what was recorded.
        let on_ended = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| self.stop());
        track.set_onended(Some(on_ended.as_ref().unchecked_ref()));
        self.live.set_value(Some(Live {
            id,
            track,
            encoder: VoiceEncoder::new(ctx.sample_rate() as u32),
            ctx,
            source,
            node,
            levels: Vec::new(),
            stopping: false,
            cancelled: false,
            _on_message: on_message,
            _on_ended: on_ended,
        }));
        self.phase.set(Phase::Recording { held, elapsed_ms: 0.0, level: 0.0 });
        Ok(())
    }

    /// A block of samples from the audio thread (or its "end" after a stop).
    fn on_block(self, id: u64, data: JsValue) {
        if data.as_string().as_deref() == Some("end") {
            self.finish(id);
            return;
        }
        let Ok(block) = data.dyn_into::<js_sys::Float32Array>() else {
            return;
        };
        let samples = block.to_vec();
        let level = if samples.is_empty() {
            0.0
        } else {
            (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
        };
        let mut elapsed = None;
        self.live.update_value(|live| {
            if let Some(live) = live.as_mut().filter(|l| l.id == id) {
                live.encoder.push(&samples);
                if !live.stopping {
                    live.levels.push(level);
                }
                elapsed = Some(live.encoder.duration_ms() as f64);
            }
        });
        let Some(elapsed_ms) = elapsed else {
            return;
        };
        self.phase.update(|phase| {
            if let Phase::Recording { elapsed_ms: e, level: l, .. } = phase {
                *e = elapsed_ms;
                *l = level;
            }
        });
        if elapsed_ms >= self.cap_ms.get_value() {
            self.stop();
        }
    }

    /// Stop and go to the review.
    pub fn stop(self) {
        match self.phase.get_untracked() {
            Phase::Starting { .. } => self.phase.set(Phase::Idle),
            Phase::Recording { .. } => self.end(false),
            _ => {}
        }
    }

    /// Stop and throw the recording away.
    pub fn cancel(self) {
        match self.phase.get_untracked() {
            Phase::Starting { .. } => self.phase.set(Phase::Idle),
            Phase::Recording { .. } => self.end(true),
            Phase::Review(_) => self.discard(),
            Phase::Idle => {}
        }
    }

    /// Keep recording with 🎤 released (slid up while holding).
    pub fn lock(self) {
        self.phase.update(|phase| {
            if let Phase::Recording { held, .. } = phase {
                *held = false;
            }
        });
    }

    /// Ask the audio thread for its last samples; `finish` runs on its "end" (or after a
    /// short wait, should it never come).
    fn end(self, cancelled: bool) {
        let mut stopping = None;
        self.live.update_value(|live| {
            if let Some(live) = live.as_mut().filter(|l| !l.stopping) {
                live.stopping = true;
                live.cancelled = cancelled;
                stopping = Some((live.id, live.node.port().ok()));
            }
        });
        if cancelled {
            self.phase.set(Phase::Idle);
        }
        let Some((id, port)) = stopping else {
            return;
        };
        if port.is_none_or(|port| port.post_message(&JsValue::NULL).is_err()) {
            self.finish(id);
            return;
        }
        let fallback = Closure::once_into_js(move || self.finish(id));
        if let Some(win) = window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(fallback.unchecked_ref(), FLUSH_TIMEOUT_MS);
        }
    }

    /// Release the microphone and build the take (unless cancelled).
    fn finish(self, id: u64) {
        let mut taken = None;
        self.live.update_value(|l| {
            if l.as_ref().is_some_and(|live| live.id == id) {
                taken = l.take();
            }
        });
        let Some(live) = taken else {
            return;
        };
        if let Ok(port) = live.node.port() {
            port.set_onmessage(None);
        }
        live.track.set_onended(None);
        let _ = live.source.disconnect();
        let _ = live.node.disconnect();
        live.track.stop();
        let _ = live.ctx.close();
        if live.cancelled || !matches!(self.phase.get_untracked(), Phase::Recording { .. }) {
            return;
        }
        let duration_ms = live.encoder.duration_ms();
        let waveform = waveform_from_levels(&live.levels);
        let take = live.encoder.finish().and_then(|mp3| {
            let bag = web_sys::BlobPropertyBag::new();
            bag.set_type(VOICE_MIME);
            let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(&mp3[..]));
            let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &bag).ok()?;
            let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
            Some(Take { blob, url, duration_ms, waveform })
        });
        match take {
            Some(take) => self.phase.set(Phase::Review(take)),
            None => {
                log::warn!("The recording came out empty");
                self.phase.set(Phase::Idle);
            }
        }
    }

    /// Drop the take under review.
    pub fn discard(self) {
        if let Phase::Review(take) = self.phase.get_untracked() {
            let _ = web_sys::Url::revoke_object_url(&take.url);
        }
        self.phase.set(Phase::Idle);
    }

    /// The take under review, for sending: the recorder is idle again (its URL now belongs
    /// to the caller).
    pub fn take(self) -> Option<Take> {
        match self.phase.get_untracked() {
            Phase::Review(take) => {
                self.phase.set(Phase::Idle);
                Some(take)
            }
            _ => None,
        }
    }

    /// The tab leaves the room: nothing recorded stays.
    pub fn reset(self) {
        self.cancel();
        self.discard();
    }
}

/// `m:ss` of a recording's length so far (whole seconds, as a timer counts).
pub fn elapsed_label(elapsed_ms: f64) -> String {
    clock((elapsed_ms / 1000.0).floor())
}

// ---- Message bar ------------------------------------------------------------------------

/// What the message bar shows of the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecMode {
    Idle,
    Starting,
    /// Recording while 🎤 is held down.
    Held,
    /// Recording hands-free (tapped, or slid up to lock): ⏹ stops.
    HandsFree,
    Review,
}

impl RecMode {
    pub fn of(phase: &Phase) -> Self {
        match phase {
            Phase::Idle => RecMode::Idle,
            Phase::Starting { .. } => RecMode::Starting,
            Phase::Recording { held: true, .. } => RecMode::Held,
            Phase::Recording { held: false, .. } => RecMode::HandsFree,
            Phase::Review(_) => RecMode::Review,
        }
    }

    pub fn capturing(self) -> bool {
        matches!(self, RecMode::Starting | RecMode::Held | RecMode::HandsFree)
    }
}

/// Holding 🎤 this long records while held; a shorter tap records hands-free.
const HOLD_MS: i32 = 300;
/// While held: sliding this far toward the start cancels, this far up locks.
const SLIDE_CANCEL_PX: i32 = 80;
const SLIDE_LOCK_PX: i32 = 60;

struct Press {
    x: i32,
    y: i32,
    timer: i32,
    /// The hold started a recording: the click that follows the release is not a tap.
    recording: bool,
    _fire: Closure<dyn FnMut()>,
}

/// 🎤's gestures: tap to record hands-free (tap ⏹ to stop), hold to record while held
/// (release to stop, slide toward the start to cancel, up to lock). Enter and Space tap.
#[derive(Clone, Copy)]
pub struct MicPress {
    recorder: Recorder,
    start: StoredValue<Rc<dyn Fn(bool)>>,
    press: StoredValue<Option<Press>>,
    skip_click: StoredValue<bool>,
}

impl MicPress {
    /// `start(held)` opens the microphone and records.
    pub fn new(recorder: Recorder, start: impl Fn(bool) + 'static) -> Self {
        Self { recorder, start: store_value(Rc::new(start)), press: store_value(None), skip_click: store_value(false) }
    }

    pub fn down(self, ev: web_sys::PointerEvent) {
        if ev.button() != 0 || self.recorder.phase.get_untracked() != Phase::Idle {
            return;
        }
        self.recorder.prepare();
        // Leptos delegates events (`currentTarget` is the document): capture on the button.
        let button = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest(".record-btn").ok().flatten());
        if let Some(button) = button {
            let _ = button.set_pointer_capture(ev.pointer_id());
        }
        let fire = Closure::<dyn FnMut()>::new(move || {
            let mut pressed = false;
            self.press.update_value(|p| {
                if let Some(p) = p.as_mut() {
                    p.recording = true;
                    pressed = true;
                }
            });
            if pressed {
                (self.start.get_value())(true);
            }
        });
        let Some(timer) = window().and_then(|w| {
            w.set_timeout_with_callback_and_timeout_and_arguments_0(fire.as_ref().unchecked_ref(), HOLD_MS).ok()
        }) else {
            return;
        };
        self.press.set_value(Some(Press { x: ev.client_x(), y: ev.client_y(), timer, recording: false, _fire: fire }));
    }

    pub fn moved(self, ev: web_sys::PointerEvent) {
        let Some((x, y)) = self.press.with_value(|p| p.as_ref().map(|p| (p.x, p.y))) else {
            return;
        };
        if !matches!(self.recorder.phase.get_untracked(), Phase::Recording { held: true, .. }) {
            return;
        }
        let rtl = window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
            .and_then(|e| e.get_attribute("dir"))
            .as_deref()
            == Some("rtl");
        // Toward the start of the line: left, or right in right-to-left languages.
        let toward_start = if rtl { ev.client_x() - x } else { x - ev.client_x() };
        if toward_start > SLIDE_CANCEL_PX {
            self.release();
            self.skip_click.set_value(true);
            self.recorder.cancel();
        } else if y - ev.client_y() > SLIDE_LOCK_PX {
            self.recorder.lock();
        }
    }

    /// The pointer was released (`cancelled`: the browser took it away, e.g. to scroll).
    pub fn up(self, cancelled: bool) {
        let Some(recording) = self.release() else {
            return;
        };
        if recording {
            self.skip_click.set_value(true);
        }
        match self.recorder.phase.get_untracked() {
            // Released before the microphone opened: too short to keep.
            Phase::Starting { held: true } => self.recorder.cancel(),
            Phase::Recording { held: true, .. } if cancelled => self.recorder.cancel(),
            Phase::Recording { held: true, .. } => self.recorder.stop(),
            _ => {}
        }
    }

    pub fn click(self) {
        if self.skip_click.get_value() {
            self.skip_click.set_value(false);
            return;
        }
        match self.recorder.phase.get_untracked() {
            Phase::Idle => (self.start.get_value())(false),
            Phase::Recording { held: false, .. } => self.recorder.stop(),
            _ => {}
        }
    }

    /// End the press; returns whether it had started a recording.
    fn release(self) -> Option<bool> {
        let mut taken = None;
        self.press.update_value(|p| taken = p.take());
        let press = taken?;
        if let Some(win) = window() {
            win.clear_timeout_with_handle(press.timer);
        }
        Some(press.recording)
    }
}

/// While recording: the time, the live level, and how to cancel.
pub fn recording_panel(lang: ReadSignal<crate::i18n::Language>, recorder: Recorder, mode: RecMode) -> impl IntoView {
    use crate::i18n::t;
    let elapsed = move || recorder.phase.with(|p| if let Phase::Recording { elapsed_ms, .. } = p { *elapsed_ms } else { 0.0 });
    let level = move || recorder.phase.with(|p| if let Phase::Recording { level, .. } = p { *level } else { 0.0 });
    let near_end = move || elapsed() >= recorder.cap_ms() - WARN_BEFORE_MS;
    view! {
        <div class="recorder-panel" role="status">
            {(mode != RecMode::Held).then(|| view! {
                <button class="btn btn-secondary rec-cancel" title=move || t(lang.get(), "rec_discard")
                    on:click=move |_| recorder.cancel()>"🗑"</button>
            })}
            <span class="rec-dot"></span>
            <span class="rec-time" class:near-end=near_end>{move || elapsed_label(elapsed())}</span>
            <span class="rec-meter">
                <span class="rec-level" style=move || format!("transform: scaleX({:.2});", (level() * 6.0).min(1.0))></span>
            </span>
            {(mode == RecMode::Held).then(|| view! { <span class="rec-hint">{move || t(lang.get(), "rec_slide_hint")}</span> })}
            {move || near_end().then(|| view! { <span class="rec-warning">{move || t(lang.get(), "rec_ending_soon")}</span> })}
        </div>
    }
}

/// After recording: listen back, then send or discard.
pub fn review_panel(
    lang: ReadSignal<crate::i18n::Language>,
    take: Take,
    recorder: Recorder,
    can_send: impl Fn() -> bool + 'static,
    send: impl Fn() + 'static,
) -> impl IntoView {
    use crate::i18n::t;
    let audio = create_node_ref::<html::Audio>();
    let wave = create_node_ref::<html::Div>();
    let (playing, set_playing) = create_signal(false);
    let (position, set_position) = create_signal(0.0_f64);
    let duration = take.duration_ms as f64 / 1000.0;
    let element = move || audio.get().map(|a| {
        let el: &web_sys::HtmlMediaElement = a.as_ref();
        el.clone()
    });
    let toggle = move |_| {
        if let Some(el) = element() {
            if el.paused() {
                let _ = el.play();
            } else {
                let _ = el.pause();
            }
        }
    };
    let seek = move |ev: web_sys::PointerEvent| {
        if let (Some(el), Some(wave)) = (element(), wave.get()) {
            let rect = wave.get_bounding_client_rect();
            let fraction = ((ev.client_x() as f64 - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            el.set_current_time(fraction * duration);
            set_position.set(fraction * duration);
        }
    };
    let bars = take.waveform.clone();
    let count = bars.len().max(1) as f64;
    view! {
        <div class="recorder-review">
            <button class="btn btn-secondary rec-discard" title=move || t(lang.get(), "rec_discard")
                on:click=move |_| recorder.discard()>"🗑"</button>
            <button class="voice-play rec-play" title=move || t(lang.get(), if playing.get() { "voice_pause" } else { "voice_play" })
                on:click=toggle>{move || if playing.get() { "⏸" } else { "▶" }}</button>
            <div class="voice-wave" node_ref=wave on:pointerdown=seek>
                {bars.into_iter().enumerate().map(|(i, level)| {
                    let height = 12 + level as u32 * 88 / 255;
                    let played = move || duration > 0.0 && (i as f64 + 0.5) / count <= position.get() / duration;
                    view! { <span class="voice-bar" class:played=played style=format!("height: {height}%;")></span> }
                }).collect_view()}
            </div>
            <span class="voice-time">{move || if playing.get() || position.get() > 0.0 { clock(position.get()) } else { clock(duration) }}</span>
            <audio class="chat-media rec-audio" src=take.url.clone() preload="auto" node_ref=audio
                on:play=move |_| set_playing.set(true)
                on:pause=move |_| set_playing.set(false)
                on:ended=move |_| {
                    set_playing.set(false);
                    set_position.set(0.0);
                }
                on:timeupdate=move |ev| {
                    let el: web_sys::HtmlMediaElement = event_target(&ev);
                    set_position.set(el.current_time());
                }></audio>
            <button class="btn btn-primary rec-send" title=move || t(lang.get(), "rec_send")
                disabled=move || !can_send() on:click=move |_| send()>"➤"</button>
        </div>
    }
}
