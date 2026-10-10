//! Browser media plumbing: capture, per-member audio elements, video attachment and
//! the speaking meter. No session state lives here.

use crate::state::{AudioSettings, DeviceEntry};
use protocol::video::{CaptureTarget, CodecKind, CodecProbe};
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    window, AnalyserNode, AudioContext, HtmlAudioElement, HtmlMediaElement, HtmlVideoElement, MediaStream,
    MediaStreamAudioSourceNode, MediaStreamConstraints, MediaStreamTrack,
};

/// Single place where mic processing is requested; a future in-app denoiser
/// (e.g. RNNoise) would hook in alongside this. `device` asks for that microphone exactly.
fn build_audio_constraints(settings: &AudioSettings, device: Option<&str>) -> Result<JsValue, JsValue> {
    // Plain booleans are "ideal" constraints: an unsupported switch never makes
    // getUserMedia fail with OverconstrainedError.
    let audio_opts = js_sys::Object::new();
    js_sys::Reflect::set(&audio_opts, &"noiseSuppression".into(), &settings.noise_suppression.into())?;
    js_sys::Reflect::set(&audio_opts, &"echoCancellation".into(), &settings.echo_cancellation.into())?;
    js_sys::Reflect::set(&audio_opts, &"autoGainControl".into(), &settings.auto_gain_control.into())?;
    if let Some(id) = device {
        js_sys::Reflect::set(&audio_opts, &"deviceId".into(), &bound("exact", id)?)?;
    }
    Ok(audio_opts.into())
}

/// Which mic processing switches this browser recognizes, as
/// (noise suppression, echo cancellation, auto gain control).
pub fn supported_audio_constraints() -> (bool, bool, bool) {
    let supported = window()
        .and_then(|w| w.navigator().media_devices().ok())
        .and_then(|md| {
            let f = js_sys::Reflect::get(&md, &"getSupportedConstraints".into()).ok()?;
            let f: js_sys::Function = f.dyn_into().ok()?;
            f.call0(&md).ok()
        });
    let has = |key: &str| {
        supported
            .as_ref()
            .and_then(|s| js_sys::Reflect::get(s, &key.into()).ok())
            .map(|v| v.is_truthy())
            .unwrap_or(false)
    };
    (has("noiseSuppression"), has("echoCancellation"), has("autoGainControl"))
}

async fn get_user_media(audio: JsValue, video: JsValue) -> Result<MediaStream, JsValue> {
    let media_devices = window().ok_or("No window")?.navigator().media_devices()?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&audio);
    constraints.set_video(&video);
    let stream = JsFuture::from(media_devices.get_user_media_with_constraints(&constraints)?).await?;
    Ok(stream.unchecked_into())
}

/// The first track of `kind` captured into `stream`.
pub fn first_track(stream: &MediaStream, kind: &str) -> Option<MediaStreamTrack> {
    let tracks = if kind == "audio" { stream.get_audio_tracks() } else { stream.get_video_tracks() };
    (tracks.length() > 0).then(|| tracks.get(0).unchecked_into())
}

/// A fresh capture. `fell_back`: the chosen device was missing, so the system default
/// stands in.
pub struct Captured {
    pub track: MediaStreamTrack,
    pub fell_back: bool,
}

/// What `getUserMedia` says when an exact `deviceId` isn't there (unplugged, or an id from
/// another browser profile). The other constraints are only ideal: they never fail.
fn device_missing(err: &JsValue) -> bool {
    let name = js_sys::Reflect::get(err, &"name".into()).ok().and_then(|n| n.as_string());
    matches!(name.as_deref(), Some("OverconstrainedError" | "NotFoundError"))
}

async fn open_microphone(settings: &AudioSettings, device: Option<&str>) -> Result<MediaStreamTrack, JsValue> {
    let stream = get_user_media(build_audio_constraints(settings, device)?, JsValue::FALSE).await?;
    first_track(&stream, "audio").ok_or_else(|| JsValue::from_str("No audio track captured"))
}

/// Open the microphone `device` (`None`: the system default). A chosen device that is gone
/// is retried once as the system default (`Captured::fell_back`).
pub async fn capture_microphone(settings: &AudioSettings, device: Option<&str>) -> Result<Captured, JsValue> {
    match open_microphone(settings, device).await {
        Err(err) if device.is_some() && device_missing(&err) => {
            let track = open_microphone(settings, None).await?;
            Ok(Captured { track, fell_back: true })
        }
        result => result.map(|track| Captured { track, fell_back: false }),
    }
}

/// `facingMode` for the front (`user`) or rear (`environment`) camera.
pub fn facing_mode(front: bool) -> &'static str {
    if front {
        "user"
    } else {
        "environment"
    }
}

async fn open_camera(front: bool, device: Option<&str>, target: &CaptureTarget) -> Result<MediaStreamTrack, JsValue> {
    let video_opts = match device {
        Some(id) => {
            let opts = capture_constraints(target, None)?;
            js_sys::Reflect::set(&opts, &"deviceId".into(), &bound("exact", id)?)?;
            opts
        }
        None => capture_constraints(target, Some(facing_mode(front)))?,
    };
    let stream = get_user_media(JsValue::FALSE, video_opts.into()).await?;
    first_track(&stream, "video").ok_or_else(|| JsValue::from_str("No video track captured"))
}

/// Open the camera `device`, or without one the `front` or rear camera, asking for the
/// preset's size and frame rate as ideal values (the camera gets as close as it can;
/// nothing fails over them). A chosen device that is gone is retried once as the system
/// default (`Captured::fell_back`).
pub async fn capture_camera(front: bool, device: Option<&str>, target: &CaptureTarget) -> Result<Captured, JsValue> {
    match open_camera(front, device, target).await {
        Err(err) if device.is_some() && device_missing(&err) => {
            let track = open_camera(front, None, target).await?;
            Ok(Captured { track, fell_back: true })
        }
        result => result.map(|track| Captured { track, fell_back: false }),
    }
}

/// `navigator.mediaDevices.enumerateDevices()`: every microphone, camera and speaker the
/// browser lists. Labels stay empty until the page may use a device.
pub async fn enumerate_devices() -> Result<Vec<DeviceEntry>, JsValue> {
    let media_devices = window().ok_or("No window")?.navigator().media_devices()?;
    let enumerate: js_sys::Function = js_sys::Reflect::get(&media_devices, &"enumerateDevices".into())?.dyn_into()?;
    let promise: js_sys::Promise = enumerate.call0(&media_devices)?.dyn_into()?;
    let list: js_sys::Array = JsFuture::from(promise).await?.dyn_into()?;
    Ok(list.iter().map(|info| device_entry(&info)).collect())
}

fn device_entry(info: &JsValue) -> DeviceEntry {
    let field = |name: &str| js_sys::Reflect::get(info, &name.into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
    DeviceEntry { kind: field("kind"), device_id: field("deviceId"), label: field("label"), group_id: field("groupId") }
}

/// Whether media elements can play through a chosen speaker (`setSinkId`; not Safari).
pub fn speaker_selection_supported() -> bool {
    window()
        .and_then(|w| js_sys::Reflect::get(&w, &"HTMLMediaElement".into()).ok())
        .and_then(|class| js_sys::Reflect::get(&class, &"prototype".into()).ok())
        .is_some_and(|proto| proto.is_object() && js_sys::Reflect::has(&proto, &"setSinkId".into()).unwrap_or(false))
}

/// Whether the browser has its own speaker chooser (`selectAudioOutput`, Firefox), for
/// when it lists no speakers to pick from.
pub fn audio_output_chooser_supported() -> bool {
    window()
        .and_then(|w| w.navigator().media_devices().ok())
        .is_some_and(|md| js_sys::Reflect::has(&md, &"selectAudioOutput".into()).unwrap_or(false))
}

/// The browser's own speaker chooser (`navigator.mediaDevices.selectAudioOutput()`). It is
/// asked for right away, so call it straight from a click (it needs the user gesture); the
/// future resolves with the speaker picked, or fails when the chooser is dismissed.
pub fn select_audio_output() -> impl std::future::Future<Output = Result<DeviceEntry, JsValue>> {
    let started = (|| -> Result<js_sys::Promise, JsValue> {
        let media_devices = window().ok_or("No window")?.navigator().media_devices()?;
        let select: js_sys::Function = js_sys::Reflect::get(&media_devices, &"selectAudioOutput".into())?.dyn_into()?;
        select.call0(&media_devices)?.dyn_into()
    })();
    async move {
        let info = JsFuture::from(started?).await?;
        Ok(device_entry(&info))
    }
}

/// Play `element` through the speaker `id` ("" = the system default). Does nothing where
/// the browser can't choose speakers; a refusal is logged and the element keeps its output.
pub fn set_sink_id(element: &HtmlMediaElement, id: &str) {
    let current = js_sys::Reflect::get(element, &"sinkId".into()).ok().and_then(|v| v.as_string());
    if current.as_deref() == Some(id) {
        return;
    }
    let Ok(set) = js_sys::Reflect::get(element, &"setSinkId".into()).and_then(|f| f.dyn_into::<js_sys::Function>()) else {
        return;
    };
    match set.call1(element, &id.into()).and_then(|p| p.dyn_into::<js_sys::Promise>()) {
        Ok(promise) => wasm_bindgen_futures::spawn_local(async move {
            if let Err(err) = JsFuture::from(promise).await {
                log::warn!("setSinkId refused: {:?}", js_sys::Reflect::get(&err, &"name".into()).unwrap_or(err));
            }
        }),
        Err(err) => log::warn!("setSinkId failed: {:?}", err),
    }
}

/// `{ key: value }`, as in `{ ideal: 30 }`.
fn bound(key: &str, value: impl Into<JsValue>) -> Result<JsValue, JsValue> {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &key.into(), &value.into())?;
    Ok(obj.into())
}

/// The whole constraint set for a capture target: `applyConstraints` replaces every
/// constraint given before, so nothing may be left out. Screens get upper bounds (and never
/// a minimum frame rate: Chrome's zero-hertz capture would then hold frames back); cameras
/// get ideal values plus their `facingMode`.
fn capture_constraints(target: &CaptureTarget, facing: Option<&str>) -> Result<js_sys::Object, JsValue> {
    let c = js_sys::Object::new();
    let size_key = if target.ideal { "ideal" } else { "max" };
    if let Some(w) = target.width {
        js_sys::Reflect::set(&c, &"width".into(), &bound(size_key, w)?)?;
    }
    if let Some(h) = target.height {
        js_sys::Reflect::set(&c, &"height".into(), &bound(size_key, h)?)?;
    }
    let rate = bound("ideal", target.fps)?;
    if !target.ideal {
        js_sys::Reflect::set(&rate, &"max".into(), &target.fps.into())?;
    }
    js_sys::Reflect::set(&c, &"frameRate".into(), &rate)?;
    if let Some(facing) = facing {
        js_sys::Reflect::set(&c, &"facingMode".into(), &bound("ideal", facing)?)?;
    }
    Ok(c)
}

/// Re-constrain a live capture to `target` (the full set, see `capture_constraints`).
/// `facing` keeps a camera on the same side. A refusal is ignored: the track keeps what it had.
pub async fn apply_capture(track: &MediaStreamTrack, target: &CaptureTarget, facing: Option<&str>) {
    let Ok(constraints) = capture_constraints(target, facing) else {
        return;
    };
    let Ok(apply) = js_sys::Reflect::get(track, &"applyConstraints".into()).and_then(|f| f.dyn_into::<js_sys::Function>())
    else {
        return;
    };
    if let Ok(promise) = apply.call1(track, &constraints).and_then(|p| p.dyn_into::<js_sys::Promise>()) {
        if let Err(err) = JsFuture::from(promise).await {
            log::info!("applyConstraints refused: {:?}", err);
        }
    }
}

/// The size a video track delivers now (`getSettings()`), `(0, 0)` when unknown.
pub fn track_size(track: &MediaStreamTrack) -> (u32, u32) {
    let settings = track_settings(track);
    let field = |name: &str| js_sys::Reflect::get(&settings, &name.into()).ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    (field("width").max(0.0) as u32, field("height").max(0.0) as u32)
}

fn track_settings(track: &MediaStreamTrack) -> JsValue {
    js_sys::Reflect::get(track, &"getSettings".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call0(track).ok())
        .unwrap_or(JsValue::UNDEFINED)
}

/// What a screen share shows, from `track.getSettings()`: remote control maps pointer
/// positions only onto a whole monitor (`surface == "monitor"`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ScreenInfo {
    pub surface: String,
    pub width: u32,
    pub height: u32,
    pub label: String,
}

impl ScreenInfo {
    pub fn is_monitor(&self) -> bool {
        self.surface == "monitor"
    }
}

/// Share the screen at `fps` (both the ideal and the maximum). The picker leans towards a
/// whole screen (what remote control needs) and leaves this dchat tab out. There is no size
/// cap here: the returned `ScreenInfo` must be the source's own size, which dchat-host
/// matches against its monitors. Presets cap the size afterwards with `apply_capture`.
pub async fn capture_screen(fps: u32) -> Result<(MediaStreamTrack, ScreenInfo), JsValue> {
    let media_devices = window().ok_or("No window")?.navigator().media_devices()?;
    let video = js_sys::Object::new();
    js_sys::Reflect::set(&video, &"displaySurface".into(), &"monitor".into())?;
    let frame_rate = bound("ideal", fps)?;
    js_sys::Reflect::set(&frame_rate, &"max".into(), &fps.into())?;
    js_sys::Reflect::set(&video, &"frameRate".into(), &frame_rate)?;
    let options = js_sys::Object::new();
    js_sys::Reflect::set(&options, &"video".into(), &video)?;
    js_sys::Reflect::set(&options, &"audio".into(), &JsValue::FALSE)?;
    js_sys::Reflect::set(&options, &"selfBrowserSurface".into(), &"exclude".into())?;
    js_sys::Reflect::set(&options, &"monitorTypeSurfaces".into(), &"include".into())?;
    let constraints: web_sys::DisplayMediaStreamConstraints = options.unchecked_into();
    let stream: MediaStream = JsFuture::from(media_devices.get_display_media_with_constraints(&constraints)?).await?.unchecked_into();
    let track = first_track(&stream, "video").ok_or_else(|| JsValue::from_str("No screen track captured"))?;
    let info = screen_info(&track);
    Ok((track, info))
}

/// Whether this browser can share a screen at all (mobile browsers can't), for the screen
/// controls.
pub fn screen_capture_supported() -> bool {
    window()
        .and_then(|w| w.navigator().media_devices().ok())
        .is_some_and(|md| js_sys::Reflect::has(&md, &"getDisplayMedia".into()).unwrap_or(false))
}

pub fn screen_info(track: &MediaStreamTrack) -> ScreenInfo {
    let settings = track_settings(track);
    let field = |name: &str| js_sys::Reflect::get(&settings, &name.into()).unwrap_or(JsValue::UNDEFINED);
    ScreenInfo {
        surface: field("displaySurface").as_string().unwrap_or_default(),
        width: field("width").as_f64().unwrap_or(0.0) as u32,
        height: field("height").as_f64().unwrap_or(0.0) as u32,
        label: track.label(),
    }
}

/// How long a `MediaCapabilities.encodingInfo` probe may take before it counts as "no".
const PROBE_TIMEOUT_MS: i32 = 1500;

/// `navigator.mediaCapabilities.encodingInfo({ type: "webrtc" })` for one codec at the size,
/// frame rate and bitrate a preset would send. A missing API, an error or a timeout all
/// count as unsupported.
pub async fn encoding_info(kind: CodecKind, width: u32, height: u32, fps: u32, bitrate: u32) -> CodecProbe {
    let unsupported = CodecProbe { kind, supported: false, smooth: false, power_efficient: false };
    let start = || -> Result<js_sys::Promise, JsValue> {
        let navigator = window().ok_or("No window")?.navigator();
        let capabilities = js_sys::Reflect::get(&navigator, &"mediaCapabilities".into())?;
        let encoding_info: js_sys::Function = js_sys::Reflect::get(&capabilities, &"encodingInfo".into())?.dyn_into()?;
        let video = js_sys::Object::new();
        js_sys::Reflect::set(&video, &"contentType".into(), &kind.mime().into())?;
        js_sys::Reflect::set(&video, &"width".into(), &width.max(1).into())?;
        js_sys::Reflect::set(&video, &"height".into(), &height.max(1).into())?;
        js_sys::Reflect::set(&video, &"bitrate".into(), &bitrate.max(1).into())?;
        js_sys::Reflect::set(&video, &"framerate".into(), &fps.max(1).into())?;
        let config = js_sys::Object::new();
        js_sys::Reflect::set(&config, &"type".into(), &"webrtc".into())?;
        js_sys::Reflect::set(&config, &"video".into(), &video)?;
        encoding_info.call1(&capabilities, &config)?.dyn_into()
    };
    let Ok(promise) = start() else {
        return unsupported;
    };
    let Ok(info) = JsFuture::from(with_timeout(promise, PROBE_TIMEOUT_MS)).await else {
        return unsupported;
    };
    if !info.is_object() {
        return unsupported;
    }
    let flag = |name: &str| js_sys::Reflect::get(&info, &name.into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    CodecProbe { kind, supported: flag("supported"), smooth: flag("smooth"), power_efficient: flag("powerEfficient") }
}

/// Every codec kind `codec_ladder` knows.
pub const CODEC_KINDS: [CodecKind; 5] = [CodecKind::H265, CodecKind::H264, CodecKind::Av1, CodecKind::Vp9, CodecKind::Vp8];

/// `encoding_info` for every codec kind, all at once.
pub async fn probe_codecs(width: u32, height: u32, fps: u32, bitrate: u32) -> Vec<CodecProbe> {
    futures::future::join_all(CODEC_KINDS.map(|kind| encoding_info(kind, width, height, fps, bitrate))).await
}

/// `promise`, or `undefined` after `ms` if it hasn't settled by then.
fn with_timeout(promise: js_sys::Promise, ms: i32) -> js_sys::Promise {
    let timer = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    js_sys::Promise::race(&js_sys::Array::of2(&promise, &timer))
}

/// `contentHint` ("" leaves it to the browser). Browsers read it when the track is attached
/// to a sender, so set it before that. Firefox has no such property: harmless there.
pub fn set_content_hint(track: &MediaStreamTrack, hint: &str) {
    let _ = js_sys::Reflect::set(track, &"contentHint".into(), &hint.into());
}

pub async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(win) = window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

fn try_attach_video(element_id: &str, stream: &MediaStream) -> bool {
    let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(element_id)) else {
        return false;
    };
    let Ok(video) = el.dyn_into::<HtmlVideoElement>() else {
        return false;
    };
    // Video elements stay muted: remote audio plays through one <audio> per member.
    video.set_muted(true);
    if video.src_object().map(|cur| cur.id()) != Some(stream.id()) {
        video.set_src_object(Some(stream));
    }
    let _ = video.play();
    true
}

/// Attach `stream` to the `<video id=element_id>`, retrying briefly while the view renders.
pub fn attach_video(element_id: String, stream: &MediaStream) {
    // `Clone::clone` copies the JS handle; the inherent `MediaStream::clone` would call
    // JS `clone()`, duplicating tracks that then hold devices open.
    let stream = Clone::clone(stream);
    wasm_bindgen_futures::spawn_local(async move {
        // Callers attach right after changing the lounge, and the view re-renders its tiles
        // (new <video> elements) on the next microtask: wait for that, or the stream would go
        // to an element about to be replaced.
        sleep_ms(0).await;
        for _ in 0..20 {
            if try_attach_video(&element_id, &stream) {
                break;
            }
            sleep_ms(30).await;
        }
    });
}

fn remote_audio_id(pubkey: &str) -> String {
    format!("remote-audio-{pubkey}")
}

/// Play `stream` through a hidden `<audio>` dedicated to this member, on the speaker `sink`
/// ("" = the system default). Returns the `play()` promise, which rejects when the browser
/// blocks autoplay (the user must tap the page once).
pub fn play_remote_audio(pubkey: &str, stream: &MediaStream, muted: bool, sink: &str) -> Option<js_sys::Promise> {
    let doc = window()?.document()?;
    let id = remote_audio_id(pubkey);
    let audio: HtmlAudioElement = match doc.get_element_by_id(&id) {
        Some(el) => el.dyn_into().ok()?,
        None => {
            let el = doc.create_element("audio").ok()?;
            let _ = el.set_attribute("id", &id);
            let _ = el.set_attribute("class", "remote-audio");
            let _ = el.set_attribute("autoplay", "true");
            let _ = el.set_attribute("playsinline", "true");
            let _ = el.set_attribute("style", "display: none;");
            doc.body()?.append_child(&el).ok()?;
            el.dyn_into().ok()?
        }
    };
    set_sink_id(&audio, sink);
    if audio.src_object().map(|cur| cur.id()) != Some(stream.id()) {
        audio.set_src_object(Some(stream));
    }
    audio.set_muted(muted);
    audio.play().ok()
}

pub fn remove_remote_audio(pubkey: &str) {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(&remote_audio_id(pubkey))) {
        if let Ok(audio) = el.clone().dyn_into::<HtmlAudioElement>() {
            let _ = audio.pause();
            audio.set_src_object(None);
        }
        el.remove();
    }
}

fn remote_audio_elements() -> Vec<HtmlAudioElement> {
    let Some(list) = window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector_all("audio.remote-audio").ok())
    else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.get(i))
        .filter_map(|node| node.dyn_into::<HtmlAudioElement>().ok())
        .collect()
}

/// Mute or unmute every member's incoming audio locally. Peers are not notified.
pub fn set_remote_audio_muted(muted: bool) {
    for audio in remote_audio_elements() {
        audio.set_muted(muted);
    }
}

/// Move every member's incoming audio to the speaker `id` ("" = the system default).
pub fn set_remote_audio_sink(id: &str) {
    for audio in remote_audio_elements() {
        set_sink_id(&audio, id);
    }
}

/// Resume remote audio the browser paused for lack of a user gesture.
pub fn resume_remote_audio() {
    for audio in remote_audio_elements() {
        if audio.src_object().is_some() && audio.paused() {
            let _ = audio.play();
        }
    }
}

/// A short two-tone ping for @mentions (silently skipped if the browser blocks audio).
pub fn play_chime() {
    let Ok(ctx) = AudioContext::new() else {
        return;
    };
    let start = ctx.current_time();
    for (i, freq) in [880.0_f32, 1320.0].iter().enumerate() {
        let (Ok(osc), Ok(gain)) = (ctx.create_oscillator(), ctx.create_gain()) else {
            return;
        };
        osc.frequency().set_value(*freq);
        let at = start + i as f64 * 0.12;
        let _ = gain.gain().set_value_at_time(0.0001, at);
        let _ = gain.gain().exponential_ramp_to_value_at_time(0.15, at + 0.02);
        let _ = gain.gain().exponential_ramp_to_value_at_time(0.0001, at + 0.2);
        if osc.connect_with_audio_node(&gain).is_err() || gain.connect_with_audio_node(&ctx.destination()).is_err() {
            return;
        }
        let _ = osc.start_with_when(at);
        let _ = osc.stop_with_when(at + 0.22);
    }
    // Free the context once the ping has played.
    let close = wasm_bindgen::closure::Closure::once(move || {
        let _ = ctx.close();
    });
    if let Some(w) = window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(close.as_ref().unchecked_ref(), 1000);
    }
    close.forget();
}

/// RMS above which a member counts as speaking, and how long the highlight lingers.
const SPEAKING_RMS: f32 = 0.02;
pub const SPEAKING_HOLD_MS: f64 = 400.0;

/// Per-member audio level detection with the Web Audio API, used for the
/// speaking highlight. Analysis only: nothing is played or recorded.
pub struct SpeakingMeter {
    ctx: AudioContext,
    sources: HashMap<String, (MediaStreamAudioSourceNode, AnalyserNode)>,
    last_loud: HashMap<String, f64>,
    buffer: Vec<f32>,
}

impl SpeakingMeter {
    /// Create inside a user gesture (e.g. the Join Voice click) so the context may run.
    pub fn new() -> Result<Self, JsValue> {
        let ctx = AudioContext::new()?;
        let _ = ctx.resume();
        Ok(Self {
            ctx,
            sources: HashMap::new(),
            last_loud: HashMap::new(),
            buffer: vec![0.0; 512],
        })
    }

    /// Measure `pubkey`'s audio from `stream`, replacing any previous source.
    pub fn watch(&mut self, pubkey: &str, stream: &MediaStream) {
        self.unwatch(pubkey);
        let Ok(source) = self.ctx.create_media_stream_source(stream) else {
            return;
        };
        let Ok(analyser) = self.ctx.create_analyser() else {
            return;
        };
        analyser.set_fft_size(512);
        if source.connect_with_audio_node(&analyser).is_ok() {
            self.sources.insert(pubkey.to_string(), (source, analyser));
        }
    }

    pub fn unwatch(&mut self, pubkey: &str) {
        if let Some((source, analyser)) = self.sources.remove(pubkey) {
            let _ = source.disconnect();
            let _ = analyser.disconnect();
        }
        self.last_loud.remove(pubkey);
    }

    /// Members heard within the last `SPEAKING_HOLD_MS`.
    pub fn sample(&mut self, now: f64) -> Vec<String> {
        for (pubkey, (_, analyser)) in &self.sources {
            analyser.get_float_time_domain_data(&mut self.buffer);
            let rms = (self.buffer.iter().map(|x| x * x).sum::<f32>() / self.buffer.len() as f32).sqrt();
            if rms > SPEAKING_RMS {
                self.last_loud.insert(pubkey.clone(), now);
            }
        }
        self.last_loud
            .iter()
            .filter(|(_, t)| now - **t < SPEAKING_HOLD_MS)
            .map(|(pubkey, _)| pubkey.clone())
            .collect()
    }

    pub fn close(&mut self) {
        for pubkey in self.sources.keys().cloned().collect::<Vec<_>>() {
            self.unwatch(&pubkey);
        }
        let _ = self.ctx.close();
    }
}
