//! Video quality: the only place where the sharer's presets (`protocol::video`) reach the
//! browser. Capture constraints and `contentHint` go on the local track before it is
//! attached; encoder settings go on every link's video sender with `setParameters`, always
//! as a complete state (a key the preset leaves to the browser is deleted, never left over
//! from an earlier preset).
//!
//! Presets are local: they never go on the wire, so changing them never splits a room.
//!
//! It also keeps camera tiles in step with their voice on the viewer's side: video travels
//! under its own msid (so browsers never hold it back for lip sync on their own), and every
//! 2 s each camera's video receiver is asked to buffer as long as that member's audio does
//! (`jitterBufferTarget`). Screens never wait.

use super::lounge::VoiceInfo;
use super::RoomSession;
use crate::media;
use protocol::video::{
    camera_target, codec_ladder, pick_negotiated, screen_target, sender_params, CodecIntent, CodecKind, CodecProbe,
    NegotiatedCodec, VideoPresets, VideoTarget,
};
use protocol::VideoKind;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, MediaStreamTrack, RtcRtpSender};

/// Camera re-sync and the sharer's source-size check run this often.
const QUALITY_TICK_MS: i32 = 2000;
/// `jitterBufferTarget` accepts 0 to 4000 ms.
const MAX_JITTER_TARGET_MS: f64 = 4000.0;
/// Bitrate to probe codecs with when the preset leaves it to the browser (Chrome's default
/// ceiling above 960×540).
const DEFAULT_PROBE_BITRATE: u32 = 2_500_000;
/// How often the source-size and preset loops may re-read before settling.
const MAX_SETTLE_ROUNDS: usize = 3;

type ProbeKey = (CodecKind, u32, u32, u32, u32);

/// `setParameters` calls are serialized per link: one in flight, the latest request waits.
#[derive(Default)]
struct ParamJob {
    in_flight: bool,
    pending: bool,
}

/// What a member's audio buffered at the last tick (cumulative `jitterBufferDelay` in
/// seconds and `jitterBufferEmittedCount`).
#[derive(Clone, Copy)]
struct BufferSample {
    delay_s: f64,
    emitted: f64,
}

#[derive(Default)]
pub(super) struct VideoQuality {
    /// `RTCRtpReceiver` per member and kind ("audio" / "video"), from their track events.
    receivers: RefCell<HashMap<String, HashMap<String, JsValue>>>,
    /// Camera re-sync: each member's previous audio buffer sample.
    audio_samples: RefCell<HashMap<String, BufferSample>>,
    /// Members whose video receiver was told not to wait (screen or no video).
    unsynced: RefCell<HashSet<String>>,
    resync_busy: Cell<bool>,
    /// Codecs to prefer for the current source, best first, and the intent they serve.
    ladder: RefCell<(Option<CodecIntent>, Vec<CodecKind>)>,
    /// The probes behind the last ladder, to rebuild one for another intent at once.
    last_probes: RefCell<Vec<CodecProbe>>,
    ladder_gen: Cell<u64>,
    probe_cache: RefCell<HashMap<ProbeKey, CodecProbe>>,
    /// Frame size of the local video track when parameters were last worked out.
    last_frame_size: Cell<Option<(u32, u32)>>,
    /// Preset changes in flight: only the newest finishes.
    preset_gen: Cell<u64>,
    param_jobs: RefCell<HashMap<String, ParamJob>>,
    timer_started: Cell<bool>,
}

impl RoomSession {
    // ---- Presets -----------------------------------------------------------------------

    /// The sharer's quality presets. Stored for the next camera or share; while one is
    /// live and its preset changed, the capture, the hint, the codec choice and every
    /// link's encoder follow at once.
    pub fn set_video_presets(&self, presets: VideoPresets) {
        let lounge = &self.inner.lounge;
        let old = lounge.video_presets.replace(presets);
        let changed = match self.my_voice().video {
            VideoKind::Camera => old.camera != presets.camera,
            VideoKind::Screen => old.screen != presets.screen,
            VideoKind::None => false,
        };
        if !changed {
            return;
        }
        let Some(track) = self.local_track("video") else {
            return;
        };
        let generation = lounge.quality.preset_gen.get() + 1;
        lounge.quality.preset_gen.set(generation);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let current = |s: &RoomSession| {
                s.inner.lounge.quality.preset_gen.get() == generation
                    && !s.inner.closed.get()
                    && s.local_track("video").is_some_and(|t| t.id() == track.id())
            };
            let kind = s.my_voice().video;
            let Some(target) = s.target_for(kind) else {
                return;
            };
            media::apply_capture(&track, &target.capture, s.facing_for(kind)).await;
            if !current(&s) {
                return;
            }
            media::set_content_hint(&track, target.hint.as_str());
            let (w, h) = media::track_size(&track);
            s.rebuild_ladder(&target, w, h).await;
            if !current(&s) {
                return;
            }
            // WebKit reads `contentHint` only when a track is attached: attach it again.
            let senders: Vec<(String, RtcRtpSender)> = s
                .inner
                .lounge
                .senders
                .borrow()
                .iter()
                .filter_map(|(remote, l)| l.video.clone().map(|v| (remote.clone(), v)))
                .filter(|(_, sender)| sender.track().is_some_and(|t| t.id() == track.id()))
                .collect();
            for (remote, sender) in senders {
                let promise = sender.replace_track(Some(&track));
                let s = s.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(err) = JsFuture::from(promise).await {
                        log::warn!("replaceTrack (same track) failed: {:?}", err);
                    }
                    s.apply_video_params(&remote);
                });
            }
        });
    }

    /// The preset settings for a source of `kind` (`None` without video).
    pub(super) fn target_for(&self, kind: VideoKind) -> Option<VideoTarget> {
        let presets = self.inner.lounge.video_presets.get();
        match kind {
            VideoKind::Camera => Some(camera_target(presets.camera)),
            VideoKind::Screen => Some(screen_target(presets.screen)),
            VideoKind::None => None,
        }
    }

    /// The camera side to keep when re-constraining; none for a camera chosen by device.
    fn facing_for(&self, kind: VideoKind) -> Option<&'static str> {
        let lounge = &self.inner.lounge;
        (kind == VideoKind::Camera && lounge.devices.borrow().camera.is_none())
            .then(|| media::facing_mode(lounge.front_camera.get()))
    }

    /// Get a fresh capture ready before it is attached: probe codecs for what it will
    /// send, re-read the presets (the user may have changed them meanwhile), cap the
    /// capture and set the hint (Safari reads it only at attach).
    pub(super) async fn prepare_video_track(&self, track: &MediaStreamTrack, kind: VideoKind) {
        let Some(mut target) = self.target_for(kind) else {
            return;
        };
        let (w, h) = media::track_size(track);
        for _ in 0..MAX_SETTLE_ROUNDS {
            self.rebuild_ladder(&target, w, h).await;
            match self.target_for(kind) {
                Some(now) if now != target => {
                    target = now;
                    continue;
                }
                _ => {}
            }
            media::apply_capture(track, &target.capture, self.facing_for(kind)).await;
            match self.target_for(kind) {
                Some(now) if now != target => target = now,
                _ => break,
            }
        }
        media::set_content_hint(track, target.hint.as_str());
    }

    // ---- Codecs ------------------------------------------------------------------------

    /// Probe codecs for what `target` will send from a `w`×`h` source and keep the ladder
    /// for its intent. A slower, older rebuild never overwrites a newer one.
    async fn rebuild_ladder(&self, target: &VideoTarget, w: u32, h: u32) {
        let quality = &self.inner.lounge.quality;
        let generation = quality.ladder_gen.get() + 1;
        quality.ladder_gen.set(generation);
        let params = sender_params(target, w, h);
        let (enc_w, enc_h) = if w > 0 && h > 0 {
            let scale = params.scale_resolution_down_by.max(1.0);
            ((w as f64 / scale).round() as u32, (h as f64 / scale).round() as u32)
        } else {
            match (target.max_width.or(target.capture.width), target.max_height.or(target.capture.height)) {
                (Some(w), Some(h)) => (w, h),
                _ => (1920, 1080),
            }
        };
        let fps = target.max_framerate.unwrap_or(target.capture.fps);
        let bitrate = params.max_bitrate_bps.unwrap_or(DEFAULT_PROBE_BITRATE);
        let probes = self.probe_cached(enc_w, enc_h, fps, bitrate).await;
        if quality.ladder_gen.get() != generation {
            return;
        }
        let ladder = codec_ladder(target.codec_intent, &probes);
        log::info!("Video codec ladder for {:?} at {enc_w}x{enc_h}@{fps}: {:?}", target.codec_intent, ladder);
        *quality.ladder.borrow_mut() = (Some(target.codec_intent), ladder);
        *quality.last_probes.borrow_mut() = probes;
    }

    /// Probes for every codec kind, asked once per session for each size, rate and bitrate.
    async fn probe_cached(&self, w: u32, h: u32, fps: u32, bitrate: u32) -> Vec<CodecProbe> {
        let cache = &self.inner.lounge.quality.probe_cache;
        let cached = || -> Option<Vec<CodecProbe>> {
            let cache = cache.borrow();
            media::CODEC_KINDS.into_iter().map(|kind| cache.get(&(kind, w, h, fps, bitrate)).copied()).collect()
        };
        if let Some(probes) = cached() {
            return probes;
        }
        let probes = media::probe_codecs(w, h, fps, bitrate).await;
        let mut cache = cache.borrow_mut();
        for probe in &probes {
            cache.insert((probe.kind, w, h, fps, bitrate), *probe);
        }
        probes
    }

    fn ladder_for(&self, intent: CodecIntent) -> Vec<CodecKind> {
        let quality = &self.inner.lounge.quality;
        let (for_intent, ladder) = &*quality.ladder.borrow();
        if *for_intent == Some(intent) {
            ladder.clone()
        } else {
            codec_ladder(intent, &quality.last_probes.borrow())
        }
    }

    // ---- Sender parameters -------------------------------------------------------------

    /// Bring `remote`'s video sender in line with the current preset. Runs on its own
    /// (never inside the caller's borrows), one `setParameters` per link at a time; asking
    /// again while one is in flight re-runs once it settles, with the state of that moment.
    pub(super) fn apply_video_params(&self, remote: &str) {
        {
            let mut jobs = self.inner.lounge.quality.param_jobs.borrow_mut();
            let job = jobs.entry(remote.to_string()).or_default();
            if job.in_flight {
                job.pending = true;
                return;
            }
            job.in_flight = true;
        }
        let s = self.clone();
        let remote = remote.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                if !s.inner.closed.get() {
                    s.run_video_params(&remote).await;
                }
                let again = match s.inner.lounge.quality.param_jobs.borrow_mut().get_mut(&remote) {
                    Some(job) if job.pending && !s.inner.closed.get() => {
                        job.pending = false;
                        true
                    }
                    Some(job) => {
                        job.in_flight = false;
                        job.pending = false;
                        false
                    }
                    None => false,
                };
                if !again {
                    break;
                }
            }
        });
    }

    pub(super) fn apply_all_video_params(&self) {
        let remotes: Vec<String> = self
            .inner
            .lounge
            .senders
            .borrow()
            .iter()
            .filter(|(_, l)| l.video.is_some())
            .map(|(remote, _)| remote.clone())
            .collect();
        for remote in remotes {
            self.apply_video_params(&remote);
        }
    }

    /// One attempt, retried once with fresh parameters when the browser says they went
    /// stale (`InvalidStateError`) or changed underneath (`InvalidModificationError`).
    async fn run_video_params(&self, remote: &str) {
        for attempt in 0..2 {
            let sender = self.inner.lounge.senders.borrow().get(remote).and_then(|l| l.video.clone());
            let Some(sender) = sender else {
                return;
            };
            let result = match self.write_video_params(&sender) {
                Ok(None) => return,
                Ok(Some(promise)) => JsFuture::from(promise).await.map(|_| ()),
                Err(err) => Err(err),
            };
            match result {
                Ok(()) => return,
                Err(err) if attempt == 0 && is_retryable(&err) => continue,
                Err(err) => {
                    log::warn!("setParameters failed for {}: {:?}", crate::names::pubkey_tag(remote), err);
                    return;
                }
            }
        }
    }

    /// `getParameters()`, the complete preset state, `setParameters()`: synchronous, so
    /// nothing else can touch the parameters in between. `Ok(None)` when there is nothing
    /// to do yet (no track, no video, or no encoding before negotiation: `Negotiated`
    /// comes back for it).
    fn write_video_params(&self, sender: &RtcRtpSender) -> Result<Option<js_sys::Promise>, JsValue> {
        let Some(track) = sender.track() else {
            return Ok(None);
        };
        let Some(target) = self.target_for(self.my_voice().video) else {
            return Ok(None);
        };
        let (w, h) = media::track_size(&track);
        self.inner.lounge.quality.last_frame_size.set(Some((w, h)));
        let wanted = sender_params(&target, w, h);

        let get: js_sys::Function = js_sys::Reflect::get(sender, &"getParameters".into())?.dyn_into()?;
        let params = get.call0(sender)?;
        let Ok(encodings) = js_sys::Reflect::get(&params, &"encodings".into())?.dyn_into::<js_sys::Array>() else {
            return Ok(None);
        };
        if encodings.length() == 0 {
            return Ok(None);
        }
        let encoding = encodings.get(0);
        set_or_delete(&encoding, "maxBitrate", wanted.max_bitrate_bps.map(JsValue::from))?;
        set_or_delete(&encoding, "maxFramerate", wanted.max_framerate.map(JsValue::from))?;
        js_sys::Reflect::set(
            &encoding,
            &"scaleResolutionDownBy".into(),
            &wanted.scale_resolution_down_by.max(1.0).into(),
        )?;
        set_or_delete(&params, "degradationPreference", wanted.degradation.map(|d| JsValue::from(d.as_str())))?;

        // The codec: the first of the ladder this link negotiated, as the receiver listed
        // it. Otherwise none, so the link's default applies (never an earlier choice).
        let codecs = js_sys::Reflect::get(&params, &"codecs".into())
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Array>().ok())
            .unwrap_or_default();
        let field = |obj: &JsValue, name: &str| js_sys::Reflect::get(obj, &name.into()).ok().and_then(|v| v.as_string());
        let negotiated: Vec<NegotiatedCodec> = codecs
            .iter()
            .map(|c| NegotiatedCodec {
                mime_type: field(&c, "mimeType").unwrap_or_default(),
                sdp_fmtp_line: field(&c, "sdpFmtpLine"),
            })
            .collect();
        let picked = pick_negotiated(&self.ladder_for(target.codec_intent), &negotiated);
        set_or_delete(&encoding, "codec", picked.map(|i| codecs.get(i as u32)))?;

        let set: js_sys::Function = js_sys::Reflect::get(sender, &"setParameters".into())?.dyn_into()?;
        Ok(Some(set.call1(sender, &params)?.dyn_into()?))
    }

    // ---- Receivers ---------------------------------------------------------------------

    pub(super) fn record_receiver(&self, remote: &str, kind: &str, receiver: JsValue) {
        if !receiver.is_object() {
            return;
        }
        let quality = &self.inner.lounge.quality;
        quality.receivers.borrow_mut().entry(remote.to_string()).or_default().insert(kind.to_string(), receiver);
        if kind == "video" {
            // A new receiver starts from the browser's default.
            quality.unsynced.borrow_mut().remove(remote);
        }
    }

    pub(super) fn forget_member_quality(&self, remote: &str) {
        let quality = &self.inner.lounge.quality;
        quality.receivers.borrow_mut().remove(remote);
        quality.audio_samples.borrow_mut().remove(remote);
        quality.unsynced.borrow_mut().remove(remote);
        quality.param_jobs.borrow_mut().remove(remote);
    }

    // ---- Periodic checks ---------------------------------------------------------------

    /// Start the 2 s quality tick (once per session).
    pub(super) fn start_quality_timer(&self) {
        let quality = &self.inner.lounge.quality;
        if quality.timer_started.replace(true) {
            return;
        }
        let s = self.clone();
        let handle: std::rc::Rc<Cell<Option<i32>>> = std::rc::Rc::new(Cell::new(None));
        let handle_c = handle.clone();
        let cb = Closure::wrap(Box::new(move || {
            if s.inner.closed.get() {
                if let (Some(h), Some(w)) = (handle_c.take(), window()) {
                    w.clear_interval_with_handle(h);
                }
                return;
            }
            s.quality_tick();
        }) as Box<dyn FnMut()>);
        if let Some(w) = window() {
            if let Ok(h) = w.set_interval_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), QUALITY_TICK_MS) {
                handle.set(Some(h));
            }
        }
        cb.forget();
    }

    fn quality_tick(&self) {
        let mine = self.my_voice();
        if !mine.in_voice {
            return;
        }
        // A window or tab share changes size with its source: re-budget every link.
        if mine.video != VideoKind::None {
            if let Some(track) = self.local_track("video") {
                let size = media::track_size(&track);
                let quality = &self.inner.lounge.quality;
                if quality.last_frame_size.get() != Some(size) {
                    quality.last_frame_size.set(Some(size));
                    self.apply_all_video_params();
                }
            }
        }
        self.resync_cameras();
    }

    /// Hold each camera's video as long as that member's audio waits in its jitter buffer,
    /// so lips and voice stay together; screens never wait.
    fn resync_cameras(&self) {
        let quality = &self.inner.lounge.quality;
        if quality.resync_busy.get() {
            return;
        }
        let me = self.inner.me.clone();
        let mut cameras: Vec<(String, JsValue, JsValue)> = Vec::new();
        {
            let receivers = quality.receivers.borrow();
            let states = self.inner.lounge.states.borrow();
            let mut unsynced = quality.unsynced.borrow_mut();
            let mut samples = quality.audio_samples.borrow_mut();
            for (member, by_kind) in receivers.iter().filter(|(m, _)| **m != me) {
                let Some(video) = by_kind.get("video").filter(|r| has_jitter_target(r)) else {
                    continue;
                };
                let info: VoiceInfo = states.get(member).copied().unwrap_or_default();
                if info.in_voice && info.video == VideoKind::Camera {
                    if let Some(audio) = by_kind.get("audio") {
                        cameras.push((member.clone(), audio.clone(), video.clone()));
                    }
                } else {
                    samples.remove(member);
                    if unsynced.insert(member.clone()) {
                        // `null` drops the target; 0 would only set a minimum (a no-op).
                        let _ = js_sys::Reflect::set(video, &"jitterBufferTarget".into(), &JsValue::NULL);
                    }
                }
            }
        }
        if cameras.is_empty() {
            return;
        }
        quality.resync_busy.set(true);
        let s = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            for (member, audio, video) in cameras {
                let Some(sample) = audio_buffer_sample(&audio).await else {
                    continue;
                };
                let quality = &s.inner.lounge.quality;
                // The member may have left or switched to a screen meanwhile.
                let still_camera = quality.receivers.borrow().get(&member).and_then(|r| r.get("video")).is_some_and(|v| v == &video);
                if !still_camera {
                    continue;
                }
                let previous = quality.audio_samples.borrow_mut().insert(member.clone(), sample);
                quality.unsynced.borrow_mut().remove(&member);
                let Some(previous) = previous else {
                    continue;
                };
                let emitted = sample.emitted - previous.emitted;
                if emitted <= 0.0 {
                    continue;
                }
                let delay_ms = ((sample.delay_s - previous.delay_s) / emitted * 1000.0).clamp(0.0, MAX_JITTER_TARGET_MS);
                if delay_ms.is_finite() {
                    let _ = js_sys::Reflect::set(&video, &"jitterBufferTarget".into(), &delay_ms.into());
                }
            }
            s.inner.lounge.quality.resync_busy.set(false);
        });
    }
}

/// Set `key` on `obj`, or delete it for `None` (the browser's default).
fn set_or_delete(obj: &JsValue, key: &str, value: Option<JsValue>) -> Result<(), JsValue> {
    match value {
        Some(value) => js_sys::Reflect::set(obj, &key.into(), &value).map(|_| ()),
        None => js_sys::Reflect::delete_property(obj.unchecked_ref::<js_sys::Object>(), &key.into()).map(|_| ()),
    }
}

fn is_retryable(err: &JsValue) -> bool {
    let name = js_sys::Reflect::get(err, &"name".into()).ok().and_then(|n| n.as_string());
    matches!(name.as_deref(), Some("InvalidStateError" | "InvalidModificationError"))
}

/// `'jitterBufferTarget' in receiver` (through its prototype).
fn has_jitter_target(receiver: &JsValue) -> bool {
    receiver.is_object() && js_sys::Reflect::has(receiver, &"jitterBufferTarget".into()).unwrap_or(false)
}

/// The cumulative audio buffer figures of one receiver (`inbound-rtp`, kind audio).
async fn audio_buffer_sample(receiver: &JsValue) -> Option<BufferSample> {
    let get_stats: js_sys::Function = js_sys::Reflect::get(receiver, &"getStats".into()).ok()?.dyn_into().ok()?;
    let promise: js_sys::Promise = get_stats.call0(receiver).ok()?.dyn_into().ok()?;
    let report = JsFuture::from(promise).await.ok()?;
    let values: js_sys::Function = js_sys::Reflect::get(&report, &"values".into()).ok()?.dyn_into().ok()?;
    let iter = values.call0(&report).ok()?;
    let field = |obj: &JsValue, name: &str| js_sys::Reflect::get(obj, &name.into()).unwrap_or(JsValue::UNDEFINED);
    for stat in js_sys::try_iter(&iter).ok()??.flatten() {
        if field(&stat, "type").as_string().as_deref() != Some("inbound-rtp") || field(&stat, "kind").as_string().as_deref() != Some("audio") {
            continue;
        }
        let delay_s = field(&stat, "jitterBufferDelay").as_f64()?;
        let emitted = field(&stat, "jitterBufferEmittedCount").as_f64()?;
        return Some(BufferSample { delay_s, emitted });
    }
    None
}
