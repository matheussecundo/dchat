//! Browser media plumbing: capture, per-member audio elements, video attachment and
//! the speaking meter. No session state lives here.

use crate::state::AudioSettings;
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    window, AnalyserNode, AudioContext, HtmlAudioElement, HtmlVideoElement, MediaStream,
    MediaStreamAudioSourceNode, MediaStreamConstraints, MediaStreamTrack,
};

/// Single place where mic processing is requested; a future in-app denoiser
/// (e.g. RNNoise) would hook in alongside this.
fn build_audio_constraints(settings: &AudioSettings) -> Result<JsValue, JsValue> {
    // Plain booleans are "ideal" constraints: an unsupported switch never makes
    // getUserMedia fail with OverconstrainedError.
    let audio_opts = js_sys::Object::new();
    js_sys::Reflect::set(&audio_opts, &"noiseSuppression".into(), &settings.noise_suppression.into())?;
    js_sys::Reflect::set(&audio_opts, &"echoCancellation".into(), &settings.echo_cancellation.into())?;
    js_sys::Reflect::set(&audio_opts, &"autoGainControl".into(), &settings.auto_gain_control.into())?;
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

pub async fn capture_microphone(settings: &AudioSettings) -> Result<MediaStreamTrack, JsValue> {
    let stream = get_user_media(build_audio_constraints(settings)?, JsValue::FALSE).await?;
    first_track(&stream, "audio").ok_or_else(|| JsValue::from_str("No audio track captured"))
}

pub async fn capture_camera(front: bool) -> Result<MediaStreamTrack, JsValue> {
    let video_opts = js_sys::Object::new();
    let facing = if front { "user" } else { "environment" };
    js_sys::Reflect::set(&video_opts, &"facingMode".into(), &facing.into())?;
    let stream = get_user_media(JsValue::FALSE, video_opts.into()).await?;
    first_track(&stream, "video").ok_or_else(|| JsValue::from_str("No video track captured"))
}

pub async fn capture_screen() -> Result<MediaStreamTrack, JsValue> {
    let media_devices = window().ok_or("No window")?.navigator().media_devices()?;
    let stream: MediaStream = JsFuture::from(media_devices.get_display_media()?).await?.unchecked_into();
    first_track(&stream, "video").ok_or_else(|| JsValue::from_str("No screen track captured"))
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
    if try_attach_video(&element_id, &stream) {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..20 {
            sleep_ms(30).await;
            if try_attach_video(&element_id, &stream) {
                break;
            }
        }
    });
}

fn remote_audio_id(pubkey: &str) -> String {
    format!("remote-audio-{pubkey}")
}

/// Play `stream` through a hidden `<audio>` dedicated to this member. Returns the `play()`
/// promise, which rejects when the browser blocks autoplay (the user must tap the page once).
pub fn play_remote_audio(pubkey: &str, stream: &MediaStream, muted: bool) -> Option<js_sys::Promise> {
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

/// Resume remote audio the browser paused for lack of a user gesture.
pub fn resume_remote_audio() {
    for audio in remote_audio_elements() {
        if audio.src_object().is_some() && audio.paused() {
            let _ = audio.play();
        }
    }
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
