//! Live connection stats for a video tile (the ⓘ button). While a panel is open its link's
//! `getStats()` is read once a second, keeping only the previous reading to compute rates;
//! everything is dropped when the panel closes and nothing is stored anywhere.
//!
//! Only sizes, rates, timings and codec names are shown: never addresses, ports, or
//! candidate ids or types (they are looked up by id internally and never rendered).
//! Every field is checked at run time, since browsers expose different subsets; a missing
//! one shows as "–".

use crate::i18n::{t, Language};
use crate::names::pubkey_tag;
use crate::session::RoomSession;
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::HtmlVideoElement;

/// How often an open panel reads the stats.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What a missing value shows as.
const MISSING: &str = "–";

// ---- Pure part: deltas and formatting (unit-tested) ------------------------------------

/// Cumulative counters of the video we receive on one link.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InboundCounters {
    pub timestamp_ms: Option<f64>,
    pub bytes_received: Option<f64>,
    pub jitter_buffer_delay: Option<f64>,
    pub jitter_buffer_emitted: Option<f64>,
    pub jitter_buffer_target: Option<f64>,
    pub jitter_buffer_minimum: Option<f64>,
    pub total_decode_time: Option<f64>,
    pub frames_decoded: Option<f64>,
}

/// One reading of the received video (`inbound-rtp`), plus the link's round trip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InboundReading {
    pub counters: InboundCounters,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub fps: Option<f64>,
    pub freezes: Option<f64>,
    /// Short codec name ("VP8").
    pub codec: Option<String>,
    pub rtt_ms: Option<f64>,
}

/// What a viewer's panel shows for a member's video.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerStats {
    pub resolution: Option<(u32, u32)>,
    pub fps: Option<f64>,
    pub bitrate_bps: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub buffer_ms: Option<f64>,
    pub sync_ms: Option<f64>,
    pub decode_ms: Option<f64>,
    /// Packet received to frame expected on screen (`requestVideoFrameCallback`).
    pub display_ms: Option<f64>,
    pub freezes: Option<u32>,
    pub codec: Option<String>,
}

/// `now - prev`, when both exist and the counter did not go backwards (a reset).
fn delta(now: Option<f64>, prev: Option<f64>) -> Option<f64> {
    let d = now? - prev?;
    (d >= 0.0).then_some(d)
}

/// Milliseconds per unit: Δseconds / Δcount × 1000, when anything was counted.
fn per_unit_ms(seconds: Option<f64>, count: Option<f64>) -> Option<f64> {
    let count = count?;
    (count > 0.0).then(|| seconds.map(|s| s / count * 1000.0)).flatten()
}

/// Bits per second from Δbytes over Δtimestamp (ms).
fn bitrate(bytes: Option<f64>, elapsed_ms: Option<f64>) -> Option<f64> {
    let elapsed = elapsed_ms?;
    (elapsed > 0.0).then(|| bytes.map(|b| b * 8.0 * 1000.0 / elapsed)).flatten()
}

fn size(width: Option<f64>, height: Option<f64>) -> Option<(u32, u32)> {
    match (width, height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w as u32, h as u32)),
        _ => None,
    }
}

/// The sum of whichever parts exist (`None` when none does).
fn sum_present(parts: &[Option<f64>]) -> Option<f64> {
    parts.iter().flatten().copied().reduce(|a, b| a + b)
}

impl ViewerStats {
    /// From this reading, the previous one (for rates) and the to-screen average.
    pub fn compute(now: &InboundReading, prev: Option<&InboundCounters>, display_ms: Option<f64>) -> Self {
        let c = &now.counters;
        let p = prev.copied().unwrap_or_default();
        let emitted = delta(c.jitter_buffer_emitted, p.jitter_buffer_emitted);
        // The delay added to wait for audio: target minus the minimum the network needed.
        let sync_seconds = match (
            delta(c.jitter_buffer_target, p.jitter_buffer_target),
            delta(c.jitter_buffer_minimum, p.jitter_buffer_minimum),
        ) {
            (Some(target), Some(minimum)) => Some((target - minimum).max(0.0)),
            _ => None,
        };
        Self {
            resolution: size(now.width, now.height),
            fps: now.fps,
            bitrate_bps: bitrate(delta(c.bytes_received, p.bytes_received), delta(c.timestamp_ms, p.timestamp_ms)),
            rtt_ms: now.rtt_ms,
            buffer_ms: per_unit_ms(delta(c.jitter_buffer_delay, p.jitter_buffer_delay), emitted),
            sync_ms: per_unit_ms(sync_seconds, emitted),
            decode_ms: per_unit_ms(
                delta(c.total_decode_time, p.total_decode_time),
                delta(c.frames_decoded, p.frames_decoded),
            ),
            display_ms,
            freezes: now.freezes.map(|f| f.max(0.0) as u32),
            codec: now.codec.clone(),
        }
    }

    /// A lower bound on glass-to-glass delay: half the round trip, plus the time from a
    /// frame's packets arriving to its display. That second part is measured two ways that
    /// overlap (jitter buffer + decode count from a frame's first packet; to-screen counts
    /// from its last packet and includes decoding), so the larger one is used, never the
    /// sum. Capture and encoding on the sharer's side are invisible here. Missing parts are
    /// left out; `None` until something on this side was measured (half a round trip alone
    /// says nothing about the video).
    pub fn latency_ms(&self) -> Option<f64> {
        let pipeline = sum_present(&[self.buffer_ms, self.decode_ms]);
        let receive_side = match (pipeline, self.display_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }?;
        sum_present(&[self.rtt_ms.map(|rtt| rtt / 2.0), Some(receive_side)])
    }
}

/// Cumulative counters of the video we send on one link.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OutboundCounters {
    pub timestamp_ms: Option<f64>,
    pub bytes_sent: Option<f64>,
    pub total_encode_time: Option<f64>,
    pub frames_encoded: Option<f64>,
    pub total_packet_send_delay: Option<f64>,
    pub packets_sent: Option<f64>,
}

/// One reading of the video we send (`outbound-rtp`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutboundReading {
    pub counters: OutboundCounters,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub fps: Option<f64>,
    pub limitation: Option<String>,
    pub codec: Option<String>,
    pub power_efficient: Option<bool>,
    pub implementation: Option<String>,
}

/// `qualityLimitationReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limitation {
    None,
    Cpu,
    Bandwidth,
    Other,
}

impl Limitation {
    pub fn from_reason(reason: &str) -> Self {
        match reason {
            "none" => Self::None,
            "cpu" => Self::Cpu,
            "bandwidth" => Self::Bandwidth,
            _ => Self::Other,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::None => "stats_limited_none",
            Self::Cpu => "stats_limited_cpu",
            Self::Bandwidth => "stats_limited_bandwidth",
            Self::Other => "stats_limited_other",
        }
    }
}

/// What the sharer's own panel shows for one viewer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SenderStats {
    pub resolution: Option<(u32, u32)>,
    pub fps: Option<f64>,
    pub bitrate_bps: Option<f64>,
    pub encode_ms: Option<f64>,
    pub send_delay_ms: Option<f64>,
    pub limitation: Option<Limitation>,
    pub codec: Option<String>,
    /// `powerEfficientEncoder` (Chrome): hardware when true.
    pub hardware: Option<bool>,
    /// `encoderImplementation`, shown only when `hardware` is unknown.
    pub implementation: Option<String>,
}

impl SenderStats {
    pub fn compute(now: &OutboundReading, prev: Option<&OutboundCounters>) -> Self {
        let c = &now.counters;
        let p = prev.copied().unwrap_or_default();
        Self {
            resolution: size(now.width, now.height),
            fps: now.fps,
            bitrate_bps: bitrate(delta(c.bytes_sent, p.bytes_sent), delta(c.timestamp_ms, p.timestamp_ms)),
            encode_ms: per_unit_ms(
                delta(c.total_encode_time, p.total_encode_time),
                delta(c.frames_encoded, p.frames_encoded),
            ),
            send_delay_ms: per_unit_ms(
                delta(c.total_packet_send_delay, p.total_packet_send_delay),
                delta(c.packets_sent, p.packets_sent),
            ),
            limitation: now.limitation.as_deref().map(Limitation::from_reason),
            codec: now.codec.clone(),
            hardware: now.power_efficient,
            implementation: now.implementation.clone(),
        }
    }
}

/// "VP8" from "video/VP8".
pub fn short_codec(mime: &str) -> String {
    mime.rsplit('/').next().unwrap_or(mime).to_string()
}

pub fn fmt_ms(ms: Option<f64>) -> String {
    match ms {
        Some(v) if v < 10.0 => format!("{v:.1} ms"),
        Some(v) => format!("{v:.0} ms"),
        None => MISSING.to_string(),
    }
}

pub fn fmt_bitrate(bps: Option<f64>) -> String {
    match bps {
        Some(v) if v >= 1_000_000.0 => format!("{:.1} Mbps", v / 1_000_000.0),
        Some(v) => format!("{:.0} kbps", v / 1000.0),
        None => MISSING.to_string(),
    }
}

pub fn fmt_fps(fps: Option<f64>) -> String {
    fps.map_or_else(|| MISSING.to_string(), |v| format!("{v:.0} fps"))
}

pub fn fmt_resolution(size: Option<(u32, u32)>) -> String {
    size.map_or_else(|| MISSING.to_string(), |(w, h)| format!("{w}×{h}"))
}

fn fmt_text(text: Option<String>) -> String {
    text.filter(|s| !s.is_empty()).unwrap_or_else(|| MISSING.to_string())
}

// ---- Reading RTCStatsReport ------------------------------------------------------------

/// `'key' in obj` (false for anything that isn't an object).
fn has(obj: &JsValue, key: &str) -> bool {
    obj.is_object() && js_sys::Reflect::has(obj, &key.into()).unwrap_or(false)
}

fn field(obj: &JsValue, key: &str) -> Option<JsValue> {
    if !has(obj, key) {
        return None;
    }
    js_sys::Reflect::get(obj, &key.into()).ok()
}

fn num(obj: &JsValue, key: &str) -> Option<f64> {
    field(obj, key)?.as_f64().filter(|v| v.is_finite())
}

fn text(obj: &JsValue, key: &str) -> Option<String> {
    field(obj, key)?.as_string()
}

fn flag(obj: &JsValue, key: &str) -> Option<bool> {
    field(obj, key)?.as_bool()
}

/// Every stats object of a report (`report.values()`).
fn report_entries(report: &JsValue) -> Vec<JsValue> {
    let Some(values) = field(report, "values").and_then(|f| f.dyn_into::<js_sys::Function>().ok()) else {
        return Vec::new();
    };
    match values.call0(report) {
        Ok(iter) => js_sys::Array::from(&iter).to_vec(),
        Err(_) => Vec::new(),
    }
}

fn is_video_rtp(entry: &JsValue, kind: &str) -> bool {
    text(entry, "type").as_deref() == Some(kind)
        && text(entry, "kind").or_else(|| text(entry, "mediaType")).as_deref() == Some("video")
}

fn by_id(entries: &[JsValue]) -> HashMap<String, &JsValue> {
    entries.iter().filter_map(|e| text(e, "id").map(|id| (id, e))).collect()
}

fn codec_of(entry: &JsValue, ids: &HashMap<String, &JsValue>) -> Option<String> {
    let codec = ids.get(&text(entry, "codecId")?)?;
    text(codec, "mimeType").map(|m| short_codec(&m))
}

/// `currentRoundTripTime` of the pair the link uses: the transport's selected pair, else a
/// nominated succeeded pair, else any succeeded one. Only the time is read.
fn round_trip_ms(entries: &[JsValue], ids: &HashMap<String, &JsValue>) -> Option<f64> {
    let selected = entries
        .iter()
        .filter(|e| text(e, "type").as_deref() == Some("transport"))
        .find_map(|t| text(t, "selectedCandidatePairId").and_then(|id| ids.get(&id).copied()));
    let pairs = || entries.iter().filter(|e| text(e, "type").as_deref() == Some("candidate-pair"));
    let succeeded = |e: &&JsValue| text(e, "state").as_deref() == Some("succeeded");
    let pair = selected
        .or_else(|| pairs().filter(succeeded).find(|e| flag(e, "nominated") == Some(true)))
        .or_else(|| pairs().find(succeeded))?;
    num(pair, "currentRoundTripTime").map(|s| s * 1000.0)
}

fn parse_inbound(entries: &[JsValue]) -> InboundReading {
    let ids = by_id(entries);
    let rtt_ms = round_trip_ms(entries, &ids);
    let Some(rtp) = entries
        .iter()
        .filter(|e| is_video_rtp(e, "inbound-rtp"))
        .max_by(|a, b| num(a, "bytesReceived").unwrap_or(0.0).total_cmp(&num(b, "bytesReceived").unwrap_or(0.0)))
    else {
        return InboundReading { rtt_ms, ..Default::default() };
    };
    InboundReading {
        counters: InboundCounters {
            timestamp_ms: num(rtp, "timestamp"),
            bytes_received: num(rtp, "bytesReceived"),
            jitter_buffer_delay: num(rtp, "jitterBufferDelay"),
            jitter_buffer_emitted: num(rtp, "jitterBufferEmittedCount"),
            jitter_buffer_target: num(rtp, "jitterBufferTargetDelay"),
            jitter_buffer_minimum: num(rtp, "jitterBufferMinimumDelay"),
            total_decode_time: num(rtp, "totalDecodeTime"),
            frames_decoded: num(rtp, "framesDecoded"),
        },
        width: num(rtp, "frameWidth"),
        height: num(rtp, "frameHeight"),
        fps: num(rtp, "framesPerSecond"),
        freezes: num(rtp, "freezeCount"),
        codec: codec_of(rtp, &ids),
        rtt_ms,
    }
}

fn parse_outbound(entries: &[JsValue]) -> OutboundReading {
    let ids = by_id(entries);
    let Some(rtp) = entries
        .iter()
        .filter(|e| is_video_rtp(e, "outbound-rtp"))
        .max_by(|a, b| num(a, "bytesSent").unwrap_or(0.0).total_cmp(&num(b, "bytesSent").unwrap_or(0.0)))
    else {
        return OutboundReading::default();
    };
    OutboundReading {
        counters: OutboundCounters {
            timestamp_ms: num(rtp, "timestamp"),
            bytes_sent: num(rtp, "bytesSent"),
            total_encode_time: num(rtp, "totalEncodeTime"),
            frames_encoded: num(rtp, "framesEncoded"),
            total_packet_send_delay: num(rtp, "totalPacketSendDelay"),
            packets_sent: num(rtp, "packetsSent"),
        },
        width: num(rtp, "frameWidth"),
        height: num(rtp, "frameHeight"),
        fps: num(rtp, "framesPerSecond"),
        limitation: text(rtp, "qualityLimitationReason"),
        codec: codec_of(rtp, &ids),
        power_efficient: flag(rtp, "powerEfficientEncoder"),
        implementation: text(rtp, "encoderImplementation"),
    }
}

async fn read_report(promise: js_sys::Promise) -> Vec<JsValue> {
    match JsFuture::from(promise).await {
        Ok(report) => report_entries(&report),
        Err(_) => Vec::new(),
    }
}

// ---- To-screen time: requestVideoFrameCallback ------------------------------------------

type FrameCallback = Closure<dyn FnMut(JsValue, JsValue)>;

/// Samples `expectedDisplayTime - receiveTime` for every frame a `<video>` presents, where
/// the browser has `requestVideoFrameCallback` and reports `receiveTime` (WebRTC sources).
struct DisplaySampler {
    video: HtmlVideoElement,
    pending: Rc<Cell<Option<f64>>>,
    callback: Rc<RefCell<Option<FrameCallback>>>,
    samples: Rc<RefCell<Vec<f64>>>,
}

/// Ask for the next presented frame; the request handle.
fn request_frame(video: &HtmlVideoElement, callback: &FrameCallback) -> Option<f64> {
    let request = field(video, "requestVideoFrameCallback")?.dyn_into::<js_sys::Function>().ok()?;
    request.call1(video, callback.as_ref().unchecked_ref()).ok()?.as_f64()
}

impl DisplaySampler {
    fn start(video: HtmlVideoElement) -> Option<Self> {
        if !has(&video, "requestVideoFrameCallback") {
            return None;
        }
        let pending = Rc::new(Cell::new(None));
        let callback: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
        let samples = Rc::new(RefCell::new(Vec::new()));
        let on_frame = {
            let (pending, callback, samples, video) = (pending.clone(), callback.clone(), samples.clone(), video.clone());
            Closure::wrap(Box::new(move |_now: JsValue, metadata: JsValue| {
                pending.set(None);
                if let (Some(shown), Some(received)) = (num(&metadata, "expectedDisplayTime"), num(&metadata, "receiveTime")) {
                    let ms = shown - received;
                    let mut samples = samples.borrow_mut();
                    // Bounded: a second of frames is read and cleared at every poll.
                    if (0.0..10_000.0).contains(&ms) && samples.len() < 512 {
                        samples.push(ms);
                    }
                }
                if let Some(cb) = callback.borrow().as_ref() {
                    pending.set(request_frame(&video, cb));
                }
            }) as Box<dyn FnMut(JsValue, JsValue)>)
        };
        pending.set(request_frame(&video, &on_frame));
        *callback.borrow_mut() = Some(on_frame);
        Some(Self { video, pending, callback, samples })
    }

    /// The average since the last call (frames presented in about the last second).
    fn take_average(&self) -> Option<f64> {
        let samples = std::mem::take(&mut *self.samples.borrow_mut());
        (!samples.is_empty()).then(|| samples.iter().sum::<f64>() / samples.len() as f64)
    }

    /// Cancel the pending request, then drop the callback (which also ends its self-reference).
    fn stop(&self) {
        if let Some(handle) = self.pending.take() {
            if let Some(cancel) = field(&self.video, "cancelVideoFrameCallback").and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
                let _ = cancel.call1(&self.video, &handle.into());
            }
        }
        self.callback.borrow_mut().take();
    }
}

// ---- The panel --------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum PanelData {
    Collecting,
    /// A member's video, as this tab receives it.
    Viewer(ViewerStats),
    /// Our own video, per member it goes to.
    Sharer(Vec<(String, SenderStats)>),
}

/// What one open panel keeps between polls.
struct Poller {
    alive: Cell<bool>,
    busy: Cell<bool>,
    prev_in: Cell<Option<InboundCounters>>,
    prev_out: RefCell<HashMap<String, OutboundCounters>>,
    display: Option<DisplaySampler>,
}

/// Read the stats once and publish them to the panel.
fn poll(
    poller: &Rc<Poller>,
    member: &str,
    is_self: bool,
    session: StoredValue<Option<RoomSession>, LocalStorage>,
    set_data: WriteSignal<PanelData>,
) {
    if poller.busy.get() || !poller.alive.get() {
        return;
    }
    let poller = poller.clone();
    if is_self {
        let jobs: Vec<(String, js_sys::Promise)> = session
            .try_with_value(|s| {
                s.as_ref().map_or_else(Vec::new, |s| {
                    s.video_viewers().into_iter().filter_map(|v| s.peer_stats(&v).map(|p| (v, p))).collect()
                })
            })
            .unwrap_or_default();
        poller.busy.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            let reports = futures::future::join_all(jobs.into_iter().map(|(viewer, promise)| async move {
                (viewer, read_report(promise).await)
            }))
            .await;
            let mut rows = Vec::new();
            let mut counters = HashMap::new();
            for (viewer, entries) in reports {
                let reading = parse_outbound(&entries);
                let stats = SenderStats::compute(&reading, poller.prev_out.borrow().get(&viewer));
                counters.insert(viewer.clone(), reading.counters);
                rows.push((viewer, stats));
            }
            *poller.prev_out.borrow_mut() = counters;
            poller.busy.set(false);
            if poller.alive.get() {
                let _ = set_data.try_set(PanelData::Sharer(rows));
            }
        });
    } else {
        let promise = session.try_with_value(|s| s.as_ref().and_then(|s| s.peer_stats(member))).flatten();
        let display_ms = poller.display.as_ref().and_then(DisplaySampler::take_average);
        poller.busy.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            let entries = match promise {
                Some(promise) => read_report(promise).await,
                None => Vec::new(),
            };
            let reading = parse_inbound(&entries);
            let stats = ViewerStats::compute(&reading, poller.prev_in.get().as_ref(), display_ms);
            poller.prev_in.set(Some(reading.counters));
            poller.busy.set(false);
            if poller.alive.get() {
                let _ = set_data.try_set(PanelData::Viewer(stats));
            }
        });
    }
}

/// One "label value" pair. `data-stat` names it for tests.
fn stat(lang: ReadSignal<Language>, name: &'static str, label_key: &'static str, value: String) -> impl IntoView {
    view! {
        <div class="stat" data-stat=name>
            <span class="stat-label">{move || t(lang.get(), label_key)}</span>
            <span class="stat-value">{value}</span>
        </div>
    }
}

fn viewer_view(lang: ReadSignal<Language>, s: ViewerStats) -> impl IntoView {
    let latency = fmt_ms(s.latency_ms());
    // The summary first: small tiles clip the bottom of the panel.
    view! {
        <div class="stat stat-latency" data-stat="latency">
            <span class="stat-label">{move || t(lang.get(), "stats_latency")}</span>
            <span class="stat-value">{latency}</span>
        </div>
        <div class="stats-grid">
            {stat(lang, "resolution", "stats_resolution", fmt_resolution(s.resolution))}
            {stat(lang, "fps", "stats_fps", fmt_fps(s.fps))}
            {stat(lang, "bitrate", "stats_bitrate", fmt_bitrate(s.bitrate_bps))}
            {stat(lang, "rtt", "stats_rtt", fmt_ms(s.rtt_ms))}
            {stat(lang, "buffer", "stats_buffer", fmt_ms(s.buffer_ms))}
            {stat(lang, "sync", "stats_sync", fmt_ms(s.sync_ms))}
            {stat(lang, "decode", "stats_decode", fmt_ms(s.decode_ms))}
            {stat(lang, "display", "stats_display", fmt_ms(s.display_ms))}
            {stat(lang, "freezes", "stats_freezes", s.freezes.map_or_else(|| MISSING.to_string(), |f| f.to_string()))}
            {stat(lang, "codec", "stats_codec", fmt_text(s.codec))}
        </div>
    }
}

fn sender_view(
    lang: ReadSignal<Language>,
    rows: Vec<(String, SenderStats)>,
    names: ReadSignal<HashMap<String, String>>,
) -> impl IntoView {
    if rows.is_empty() {
        return view! { <p class="stats-empty">{move || t(lang.get(), "stats_no_viewers")}</p> }.into_any();
    }
    rows.into_iter()
        .map(|(viewer, s)| {
            let name_pk = viewer.clone();
            let name = move || names.with(|n| n.get(&name_pk).cloned()).unwrap_or_else(|| pubkey_tag(&name_pk));
            let limitation = s.limitation;
            let (hardware, implementation) = (s.hardware, s.implementation.clone());
            let encoder = move || match hardware {
                Some(true) => t(lang.get(), "stats_hardware").to_string(),
                Some(false) => t(lang.get(), "stats_software").to_string(),
                None => fmt_text(implementation.clone()),
            };
            view! {
                <div class="stats-viewer" data-viewer=viewer>
                    <div class="stats-viewer-name" dir="auto">{name}</div>
                    <div class="stats-grid">
                        {stat(lang, "resolution", "stats_resolution", fmt_resolution(s.resolution))}
                        {stat(lang, "fps", "stats_fps", fmt_fps(s.fps))}
                        {stat(lang, "bitrate", "stats_bitrate", fmt_bitrate(s.bitrate_bps))}
                        {stat(lang, "encode", "stats_encode", fmt_ms(s.encode_ms))}
                        {stat(lang, "send-delay", "stats_send_delay", fmt_ms(s.send_delay_ms))}
                        <div class="stat" data-stat="limited">
                            <span class="stat-label">{move || t(lang.get(), "stats_limited")}</span>
                            <span class="stat-value">{move || limitation.map_or(MISSING, |l| t(lang.get(), l.key()))}</span>
                        </div>
                        {stat(lang, "codec", "stats_codec", fmt_text(s.codec))}
                        <div class="stat" data-stat="encoder">
                            <span class="stat-label">{move || t(lang.get(), "stats_encoder")}</span>
                            <span class="stat-value">{encoder}</span>
                        </div>
                    </div>
                </div>
            }
        })
        .collect_view()
        .into_any()
}

/// The stats panel of one tile: `member`'s video as we receive it, or, on our own tile,
/// our video per member it goes to. It polls while it exists and stops when the panel
/// closes or the tile goes away.
pub fn stats_panel(
    lang: ReadSignal<Language>,
    member: String,
    is_self: bool,
    session: StoredValue<Option<RoomSession>, LocalStorage>,
    names: ReadSignal<HashMap<String, String>>,
    video: Option<HtmlVideoElement>,
) -> impl IntoView {
    let (data, set_data) = signal(PanelData::Collecting);
    let poller = Rc::new(Poller {
        alive: Cell::new(true),
        busy: Cell::new(false),
        prev_in: Cell::new(None),
        prev_out: RefCell::new(HashMap::new()),
        display: if is_self { None } else { video.and_then(DisplaySampler::start) },
    });
    poll(&poller, &member, is_self, session, set_data);
    let interval = {
        let (poller, member) = (poller.clone(), member.clone());
        set_interval_with_handle(move || poll(&poller, &member, is_self, session, set_data), POLL_INTERVAL).ok()
    };
    let held = SendWrapper::new((poller, interval));
    on_cleanup(move || {
        let (poller, interval) = held.take();
        poller.alive.set(false);
        if let Some(handle) = interval {
            handle.clear();
        }
        if let Some(display) = poller.display.as_ref() {
            display.stop();
        }
    });
    view! {
        <div
            class="tile-stats"
            data-side=if is_self { "sharer" } else { "viewer" }
            data-state=move || if data.with(|d| *d == PanelData::Collecting) { "collecting" } else { "ready" }
        >
            {move || match data.get() {
                PanelData::Collecting => view! { <p class="stats-empty">{move || t(lang.get(), "stats_collecting")}</p> }.into_any(),
                PanelData::Viewer(stats) => viewer_view(lang, stats).into_any(),
                PanelData::Sharer(rows) => sender_view(lang, rows, names).into_any(),
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound(ts: f64, bytes: f64, jb: f64, emitted: f64, target: f64, min: f64, decode: f64, decoded: f64) -> InboundCounters {
        InboundCounters {
            timestamp_ms: Some(ts),
            bytes_received: Some(bytes),
            jitter_buffer_delay: Some(jb),
            jitter_buffer_emitted: Some(emitted),
            jitter_buffer_target: Some(target),
            jitter_buffer_minimum: Some(min),
            total_decode_time: Some(decode),
            frames_decoded: Some(decoded),
        }
    }

    #[test]
    fn viewer_rates_come_from_deltas() {
        let prev = inbound(1000.0, 100_000.0, 1.0, 100.0, 2.0, 1.5, 0.5, 100.0);
        let now = InboundReading {
            // One second later: 250 kB, 30 frames held 20 ms each, 4 ms to decode.
            counters: inbound(2000.0, 350_000.0, 1.6, 130.0, 2.9, 1.8, 0.62, 130.0),
            width: Some(1280.0),
            height: Some(720.0),
            fps: Some(30.0),
            freezes: Some(2.0),
            codec: Some("VP8".into()),
            rtt_ms: Some(40.0),
        };
        let s = ViewerStats::compute(&now, Some(&prev), Some(25.0));
        assert_eq!(s.resolution, Some((1280, 720)));
        assert_eq!(s.bitrate_bps.map(f64::round), Some(2_000_000.0));
        assert!((s.buffer_ms.unwrap() - 20.0).abs() < 1e-6);
        // (0.9 - 0.3) s added over 30 frames.
        assert!((s.sync_ms.unwrap() - 20.0).abs() < 1e-6);
        assert!((s.decode_ms.unwrap() - 4.0).abs() < 1e-6);
        assert_eq!(s.freezes, Some(2));
        // 20 (rtt / 2) + max(20 + 4, 25).
        assert!((s.latency_ms().unwrap() - 45.0).abs() < 1e-6);
    }

    #[test]
    fn first_reading_and_missing_fields_show_nothing_made_up() {
        let now = InboundReading { rtt_ms: Some(30.0), ..Default::default() };
        let s = ViewerStats::compute(&now, None, None);
        assert_eq!(s.bitrate_bps, None);
        assert_eq!(s.buffer_ms, None);
        assert_eq!(s.sync_ms, None);
        assert_eq!(s.decode_ms, None);
        assert_eq!(s.resolution, None);
        assert_eq!(s.latency_ms(), None, "half a round trip alone is no video latency");
        assert_eq!(ViewerStats::default().latency_ms(), None);
        // Only to-screen known, then with the round trip.
        let s = ViewerStats { display_ms: Some(12.0), ..Default::default() };
        assert_eq!(s.latency_ms(), Some(12.0));
        let s = ViewerStats { rtt_ms: Some(10.0), decode_ms: Some(3.0), ..Default::default() };
        assert_eq!(s.latency_ms(), Some(8.0));
    }

    #[test]
    fn counters_that_go_backwards_or_stand_still_give_no_rate() {
        let prev = inbound(1000.0, 500.0, 1.0, 100.0, 1.0, 1.0, 1.0, 100.0);
        let reset = inbound(2000.0, 100.0, 0.1, 10.0, 0.1, 0.1, 0.1, 10.0);
        let s = ViewerStats::compute(&InboundReading { counters: reset, ..Default::default() }, Some(&prev), None);
        assert_eq!(s.bitrate_bps, None);
        assert_eq!(s.buffer_ms, None);
        let still = ViewerStats::compute(&InboundReading { counters: prev, ..Default::default() }, Some(&prev), None);
        assert_eq!(still.buffer_ms, None, "no frames emitted: no per-frame time");
        assert_eq!(still.bitrate_bps, None, "no time elapsed");
    }

    #[test]
    fn sender_rates_and_limits() {
        let prev = OutboundCounters {
            timestamp_ms: Some(0.0),
            bytes_sent: Some(0.0),
            total_encode_time: Some(0.0),
            frames_encoded: Some(0.0),
            total_packet_send_delay: Some(0.0),
            packets_sent: Some(0.0),
        };
        let now = OutboundReading {
            counters: OutboundCounters {
                timestamp_ms: Some(1000.0),
                bytes_sent: Some(750_000.0),
                total_encode_time: Some(0.18),
                frames_encoded: Some(60.0),
                total_packet_send_delay: Some(0.5),
                packets_sent: Some(500.0),
            },
            width: Some(1920.0),
            height: Some(1080.0),
            fps: Some(60.0),
            limitation: Some("bandwidth".into()),
            codec: Some("H264".into()),
            power_efficient: Some(true),
            implementation: None,
        };
        let s = SenderStats::compute(&now, Some(&prev));
        assert_eq!(s.bitrate_bps.map(f64::round), Some(6_000_000.0));
        assert!((s.encode_ms.unwrap() - 3.0).abs() < 1e-6);
        assert!((s.send_delay_ms.unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(s.limitation, Some(Limitation::Bandwidth));
        assert_eq!(s.hardware, Some(true));
        assert_eq!(Limitation::from_reason("none"), Limitation::None);
        assert_eq!(Limitation::from_reason("cpu"), Limitation::Cpu);
        assert_eq!(Limitation::from_reason("something-new"), Limitation::Other);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_ms(None), "–");
        assert_eq!(fmt_ms(Some(4.26)), "4.3 ms");
        assert_eq!(fmt_ms(Some(123.4)), "123 ms");
        assert_eq!(fmt_bitrate(Some(2_450_000.0)), "2.5 Mbps");
        assert_eq!(fmt_bitrate(Some(850_000.0)), "850 kbps");
        assert_eq!(fmt_bitrate(None), "–");
        assert_eq!(fmt_fps(Some(29.97)), "30 fps");
        assert_eq!(fmt_resolution(Some((1280, 720))), "1280×720");
        assert_eq!(fmt_resolution(None), "–");
        assert_eq!(short_codec("video/VP8"), "VP8");
        assert_eq!(short_codec("AV1"), "AV1");
        assert_eq!(fmt_text(Some(String::new())), "–");
    }
}
